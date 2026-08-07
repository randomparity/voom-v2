#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result through fixture assertions"
)]
#![expect(
    clippy::expect_used,
    reason = "every HANG_GUARD expiry names the wait it bounds; a bare Elapsed(()) panic \
              reports only a line number, which is how #446 was filed against the wrong wait"
)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use time::OffsetDateTime;
use tokio::sync::{Mutex, Notify, oneshot};
use voom_api::router_with_control_plane;
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{
    ArtifactAccessMode, NodeId, NodeIncarnationEndReason, NodeIncarnationStatus, OperationKind,
    TicketId, TicketOperation, WorkerStatus,
};
use voom_node_agent::config::{AgentConfig, LoadedAgentConfig, TokenSource, WorkerConfig};
use voom_node_agent::runtime::AgentRuntime;
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::execution::tickets::{NewTicket, TicketState};
use voom_test_support::TempDatabase;

/// Upper bound on every wait in this suite. It detects a hang; it is not a latency
/// assertion, so it is sized to fail fast rather than to outlast a slow host.
///
/// Raising it was considered and rejected. Under 16 concurrent copies of this suite on a
/// CPU-oversubscribed host, run durations are bimodal: a run either settles in 6-9s or
/// consumes the whole budget exactly, with nothing in between. Bounds of 60s and 150s each
/// reproduced the same expiry at the same rate as 10s, so the waits that fail are hung, not
/// slow, and a larger budget only delays the report. 30s keeps headroom over the observed
/// 6-9s loaded ceiling while still surfacing a hang promptly.
const HANG_GUARD: Duration = Duration::from_secs(30);

#[tokio::test]
async fn live_agent_fences_prior_incarnation_and_retires_orderly() {
    assert_current_agent_version();
    let fixture = LiveFixture::start(None).await;
    let echo_worker = echo_worker_binary();
    let config = fixture.agent_config(echo_worker.clone());

    let (_first_stop_guard, first_shutdown) = oneshot::channel::<()>();
    let mut first = tokio::spawn(AgentRuntime::new(config.clone()).unwrap().run_until(
        async move {
            let _ = first_shutdown.await;
        },
    ));
    let first_history = tokio::select! {
        result = &mut first => Err(format!("first agent exited during startup: {result:?}")),
        history = wait_for_incarnations(&fixture.cp, fixture.node_id, 1) => Ok(history),
    }
    .unwrap();
    let first_id = first_history[0].id;
    wait_for_request_count(
        &fixture.requests,
        &format!("/v1/execution/node/{}/heartbeat", fixture.node_id.0),
        2,
    )
    .await;

    let ticket_id = fixture.ready_probe_ticket().await;
    assert!(
        wait_for_ticket_state(&fixture.cp, ticket_id, TicketState::Succeeded).await,
        "ticket did not complete; requests={:?}",
        fixture.requests.lock().await
    );

    let (stop_second, second_shutdown) = oneshot::channel();
    let second = tokio::spawn(AgentRuntime::new(config).unwrap().run_until(async move {
        let _ = second_shutdown.await;
    }));
    let superseded = wait_for_incarnations(&fixture.cp, fixture.node_id, 2).await;
    assert_ne!(superseded[0].id, first_id);
    assert_eq!(superseded[0].status, NodeIncarnationStatus::Active);
    assert_eq!(superseded[1].status, NodeIncarnationStatus::Superseded);
    assert_eq!(
        superseded[1].end_reason,
        Some(NodeIncarnationEndReason::Superseded)
    );
    assert!(superseded[1].last_seen_at > superseded[1].started_at);

    let first_result = tokio::time::timeout(HANG_GUARD, first)
        .await
        .expect("fenced first agent did not exit after being superseded")
        .unwrap();
    assert!(first_result.is_err(), "the fenced agent must exit nonzero");

    stop_second.send(()).unwrap();
    tokio::time::timeout(HANG_GUARD, second)
        .await
        .expect("second agent did not exit after a graceful stop request")
        .unwrap()
        .unwrap();
    let retired =
        wait_for_incarnation_status(&fixture.cp, fixture.node_id, NodeIncarnationStatus::Retired)
            .await;
    assert_eq!(
        retired[0].end_reason,
        Some(NodeIncarnationEndReason::GracefulShutdown)
    );
    assert_eq!(retired[1].status, NodeIncarnationStatus::Superseded);

    let workers = fixture
        .cp
        .list_worker_inspections(Some(WorkerStatus::Retired), 10)
        .await
        .unwrap();
    assert_eq!(workers.len(), 2);
    assert!(
        workers
            .iter()
            .all(|worker| worker.worker.retired_at.is_some())
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn delayed_acquire_replay_never_dispatches() {
    let delay = Arc::new(DelayedAcquire::default());
    let fixture = LiveFixture::start(Some(delay.clone())).await;
    let config = fixture.agent_config(echo_worker_binary());
    let ticket_id = fixture.ready_probe_ticket().await;
    let (stop, shutdown) = oneshot::channel();
    let agent = tokio::spawn(AgentRuntime::new(config).unwrap().run_until(async move {
        let _ = shutdown.await;
    }));

    wait_for_count(&delay.committed_count, 1).await;
    fixture
        .cp
        .expire_due(OffsetDateTime::now_utc() + time::Duration::seconds(30))
        .await
        .unwrap();
    delay.release_response.notify_one();
    wait_for_count(&delay.acquire_count, 2).await;
    stop.send(()).unwrap();
    tokio::time::timeout(HANG_GUARD, agent)
        .await
        .expect("agent did not exit after a graceful stop request")
        .unwrap()
        .unwrap();

    let ticket = fixture.cp.tickets().get(ticket_id).await.unwrap().unwrap();
    assert_eq!(ticket.state, TicketState::Ready);
    assert_eq!(delay.complete_count.load(Ordering::SeqCst), 0);
    assert_eq!(delay.fail_count.load(Ordering::SeqCst), 0);
    fixture.shutdown().await;
}

struct LiveFixture {
    _database: TempDatabase,
    cp: ControlPlane,
    node_id: NodeId,
    token: SecretString,
    base_url: String,
    server_shutdown: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl LiveFixture {
    async fn start(delay: Option<Arc<DelayedAcquire>>) -> Self {
        let database = TempDatabase::new().unwrap();
        let database_url = voom_store::test_support::sqlite_url_for(database.path());
        voom_store::init(&database_url).await.unwrap();
        let cp = ControlPlane::open(&database_url).await.unwrap();
        let registered = cp
            .register_node(RegisterNodeInput {
                name: "lifecycle-node".to_owned(),
                kind: NodeKind::Remote,
                heartbeat_ttl_seconds: 6,
                metadata: json!({}),
            })
            .await
            .unwrap();
        let health = HealthPlane::open(&database_url).await.unwrap();
        let mut app = router_with_control_plane(health, cp.clone());
        if let Some(delay) = delay {
            app = app.layer(middleware::from_fn_with_state(delay, delay_acquire));
        }
        let requests = Arc::new(Mutex::new(Vec::new()));
        app = app.layer(middleware::from_fn_with_state(
            requests.clone(),
            record_request,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (server_shutdown, shutdown) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown.await;
                })
                .await
                .unwrap();
        });
        Self {
            _database: database,
            cp,
            node_id: registered.node.id,
            token: registered.token,
            base_url: format!("http://{address}"),
            server_shutdown,
            server,
            requests,
        }
    }

    fn agent_config(&self, echo_worker: PathBuf) -> LoadedAgentConfig {
        LoadedAgentConfig {
            config: AgentConfig {
                control_plane_url: self.base_url.clone(),
                ca_cert: None,
                node_id: self.node_id,
                poll_interval_ms: 50,
                lease_ttl_seconds: 6,
                progress_idle_timeout_seconds: 5,
                shutdown_grace_seconds: 1,
                node_token: TokenSource::Env {
                    name: "VOOM_NODE_TOKEN".to_owned(),
                },
                workers: vec![WorkerConfig {
                    name: "echo".to_owned(),
                    program: echo_worker,
                    args: Vec::new(),
                    operations: vec![OperationKind::ProbeFile],
                    artifact_access: vec![ArtifactAccessMode::SharedMount],
                    max_parallel: 1,
                }],
            },
            node_token: SecretString::from(self.token.expose_secret().to_owned()),
        }
    }

    async fn ready_probe_ticket(&self) -> TicketId {
        let now = OffsetDateTime::now_utc();
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: TicketOperation::new("probe_file").unwrap(),
                priority: 0,
                payload: json!({
                    "path": "/media/lifecycle.mkv",
                    "artifact_access": {
                        "inputs": ["handle:input:lifecycle"],
                        "outputs": ["handle:output:lifecycle"]
                    }
                }),
                max_attempts: 2,
                created_at: now,
            })
            .await
            .unwrap();
        self.cp
            .mark_ready_if_unblocked(ticket.id, now)
            .await
            .unwrap();
        ticket.id
    }

    async fn shutdown(self) {
        self.server_shutdown.send(()).unwrap();
        self.server.await.unwrap();
    }
}

#[derive(Default)]
struct DelayedAcquire {
    acquire_count: AtomicUsize,
    committed_count: AtomicUsize,
    complete_count: AtomicUsize,
    fail_count: AtomicUsize,
    release_response: Notify,
}

async fn delay_acquire(
    State(delay): State<Arc<DelayedAcquire>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if path.ends_with("/complete") {
        delay.complete_count.fetch_add(1, Ordering::SeqCst);
    } else if path.ends_with("/fail") {
        delay.fail_count.fetch_add(1, Ordering::SeqCst);
    }
    if path != "/v1/execution/lease/acquire" {
        return next.run(request).await;
    }
    let attempt = delay.acquire_count.fetch_add(1, Ordering::SeqCst);
    if attempt > 0 {
        return (
            StatusCode::OK,
            axum::Json(json!({
                "schema_version": "0",
                "command": "execution.acquire",
                "status": "ok",
                "data": {
                    "outcome": "idle",
                    "worker_id": 1,
                    "scheduler_decision_id": 1
                },
                "warnings": [],
                "error": null
            })),
        )
            .into_response();
    }
    let response = next.run(request).await;
    delay.committed_count.fetch_add(1, Ordering::SeqCst);
    delay.release_response.notified().await;
    response
}

async fn record_request(
    State(requests): State<Arc<Mutex<Vec<String>>>>,
    request: Request,
    next: Next,
) -> Response {
    requests.lock().await.push(request.uri().path().to_owned());
    next.run(request).await
}

fn assert_current_agent_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_voom-node-agent"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("voom-node-agent {}", env!("CARGO_PKG_VERSION"))
    );
}

fn echo_worker_binary() -> PathBuf {
    voom_test_support::worker::cargo_bin_or_build("voom-conformance", "echo-worker").unwrap()
}

async fn wait_for_incarnations(
    cp: &ControlPlane,
    node_id: NodeId,
    count: usize,
) -> Vec<voom_store::repo::execution::node_incarnations::NodeIncarnation> {
    tokio::time::timeout(HANG_GUARD, async {
        loop {
            let history = cp.list_node_incarnations(node_id, 10).await.unwrap();
            if history.len() >= count {
                return history;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("node never reached the expected incarnation count")
}

async fn wait_for_incarnation_status(
    cp: &ControlPlane,
    node_id: NodeId,
    status: NodeIncarnationStatus,
) -> Vec<voom_store::repo::execution::node_incarnations::NodeIncarnation> {
    tokio::time::timeout(HANG_GUARD, async {
        loop {
            let history = cp.list_node_incarnations(node_id, 10).await.unwrap();
            if history.first().is_some_and(|row| row.status == status) {
                return history;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("node incarnation never reached the expected status")
}

async fn wait_for_request_count(requests: &Mutex<Vec<String>>, path: &str, expected: usize) {
    tokio::time::timeout(HANG_GUARD, async {
        loop {
            let count = requests
                .lock()
                .await
                .iter()
                .filter(|request| request.as_str() == path)
                .count();
            if count >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("control plane never saw the expected request count");
}

async fn wait_for_ticket_state(
    cp: &ControlPlane,
    ticket_id: TicketId,
    expected: TicketState,
) -> bool {
    let result = tokio::time::timeout(HANG_GUARD, async {
        loop {
            let ticket = cp.tickets().get(ticket_id).await.unwrap().unwrap();
            if ticket.state == expected {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    result.unwrap_or(false)
}

async fn wait_for_count(count: &AtomicUsize, expected: usize) {
    tokio::time::timeout(HANG_GUARD, async {
        while count.load(Ordering::SeqCst) < expected {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("counter never reached the expected value");
}
