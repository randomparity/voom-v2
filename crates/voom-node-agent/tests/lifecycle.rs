#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result through fixture assertions"
)]
#![expect(
    clippy::expect_used,
    reason = "every HANG_GUARD expiry names the wait it bounds; a bare Elapsed(()) panic \
              reports only a line number, which is how #446 was filed against the wrong wait"
)]

use std::fmt::Debug;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use time::OffsetDateTime;
use tokio::sync::{Mutex, Notify, oneshot};
use tokio::task::JoinHandle;
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
/// 6-9s loaded ceiling while still surfacing a hang promptly.
const HANG_GUARD: Duration = Duration::from_secs(30);

/// Install one process-wide `tracing` subscriber so `voom_store`'s warnings reach
/// this binary's stderr.
///
/// Issue #592's acceptance sweep counts the orphan `warn` that
/// `voom_store::tx::begin_detached` emits when its caller was already cancelled, and
/// that count is the only evidence for the pool-occupancy residual the detach
/// introduces. Nothing else installs a subscriber here: `voom-api`'s is in its binary
/// and `voom-cli`'s in its own, and neither is linked into this test. Without this,
/// `tracing::warn!` is a no-op that emits no bytes, so the sweep's grep would report
/// zero orphans on every run whatever the code did — indistinguishable from an
/// instrument that was never connected.
///
/// `OnceLock` rather than a bare `try_init` call so concurrent test entry cannot race,
/// and a second call is a no-op rather than a panic. The writer is stderr because the
/// sweep redirects both streams to one log.
fn init_tracing() {
    static INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INSTALLED.get_or_init(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("voom_store=warn"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init();
    });
}

#[tokio::test]
async fn live_agent_fences_prior_incarnation_and_retires_orderly() {
    init_tracing();
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
    let completed = wait_for_ticket_state(&fixture.cp, ticket_id, TicketState::Succeeded).await;
    let mut request_counts = std::collections::BTreeMap::new();
    for request in fixture.requests.lock().await.iter() {
        *request_counts.entry(request.clone()).or_insert(0usize) += 1;
    }
    assert!(
        completed,
        "ticket did not complete; paths={request_counts:?}"
    );

    let (stop_second, second_shutdown) = oneshot::channel();
    let mut second = tokio::spawn(AgentRuntime::new(config).unwrap().run_until(async move {
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
    let retired =
        wait_for_graceful_shutdown(&fixture.cp, fixture.node_id, &mut second, &fixture.requests)
            .await
            .expect("second agent graceful-shutdown lifecycle did not complete");
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
    let mut agent = tokio::spawn(AgentRuntime::new(config).unwrap().run_until(async move {
        let _ = shutdown.await;
    }));

    wait_for_acquire_transition(
        &delay.first_response_committed,
        &mut agent,
        "first acquire response committed",
    )
    .await
    .unwrap();
    fixture
        .cp
        .expire_due(OffsetDateTime::now_utc() + time::Duration::seconds(30))
        .await
        .unwrap();
    delay.release_response.notify_one();
    wait_for_acquire_transition(
        &delay.replay_acquire_started,
        &mut agent,
        "replay acquire started",
    )
    .await
    .unwrap();
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

#[tokio::test]
async fn acquire_transition_reports_ready_agent_before_ready_milestone() {
    let milestone = Notify::new();
    milestone.notify_one();
    let mut agent = tokio::spawn(std::future::ready(()));
    while !agent.is_finished() {
        tokio::task::yield_now().await;
    }

    let error = wait_for_acquire_transition(&milestone, &mut agent, "simultaneous test milestone")
        .await
        .unwrap_err();

    assert_eq!(
        error,
        "agent exited before simultaneous test milestone: Ok(())"
    );
}

#[tokio::test]
async fn acquire_transition_observes_milestone_notified_before_waiting() {
    let milestone = Notify::new();
    milestone.notify_one();
    let mut agent = tokio::spawn(std::future::pending::<()>());

    tokio::time::timeout(
        Duration::from_millis(100),
        wait_for_acquire_transition(&milestone, &mut agent, "pre-notified test milestone"),
    )
    .await
    .unwrap()
    .unwrap();

    agent.abort();
    assert!(agent.await.unwrap_err().is_cancelled());
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
    /// Storage root the probe-ticket fixture resolves against; bound into
    /// every agent config this fixture spawns.
    media_root: PathBuf,
    _media_root_guard: tempfile::TempDir,
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
        let media_root_guard = tempfile::tempdir().unwrap();
        let media_root = media_root_guard.path().to_path_buf();
        Self {
            _database: database,
            cp,
            node_id: registered.node.id,
            token: registered.token,
            base_url: format!("http://{address}"),
            server_shutdown,
            server,
            requests,
            media_root,
            _media_root_guard: media_root_guard,
        }
    }

    fn agent_config(&self, echo_worker: PathBuf) -> LoadedAgentConfig {
        LoadedAgentConfig {
            config: AgentConfig {
                control_plane_url: self.base_url.clone(),
                node_id: self.node_id,
                ca_cert: None,
                storage_roots: vec![voom_node_agent::config::StorageRootBinding {
                    storage_root_id: 1,
                    provider_locator: self.media_root.clone(),
                }],
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
                    dependencies: voom_node_agent::config::WorkerDependencyPaths::default(),
                    accelerator: None,
                    max_parallel: 1,
                }],
            },
            node_token: SecretString::from(self.token.expose_secret().to_owned()),
        }
    }

    async fn ready_probe_ticket(&self) -> TicketId {
        let now = OffsetDateTime::now_utc();
        // A real file on the bound root: the echo worker echoes the expected
        // facts back, so the executor's post-dispatch observation agrees.
        let media_path = self.media_root.join("lifecycle.mkv");
        std::fs::write(&media_path, b"lifecycle-media-bytes").unwrap();
        let content_hash = format!("blake3:{}", blake3::hash(b"lifecycle-media-bytes"));
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: TicketOperation::new("probe_file").unwrap(),
                priority: 0,
                payload: json!({
                    "media_dispatch": {
                        "operation": "probe",
                        "schema": voom_core::PROTOCOL_VERSION,
                        "source": {
                            "storage_root_id": 1,
                            "provider_relative_locator": "lifecycle.mkv",
                        },
                        "expected": {
                            "size_bytes": 21,
                            "content_hash": content_hash,
                            "modified_at": null,
                            "local_file_key": null,
                        },
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
    complete_count: AtomicUsize,
    fail_count: AtomicUsize,
    first_response_committed: Notify,
    release_response: Notify,
    replay_acquire_started: Notify,
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
        delay.replay_acquire_started.notify_one();
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
    delay.first_response_committed.notify_one();
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

async fn wait_for_graceful_shutdown(
    cp: &ControlPlane,
    node_id: NodeId,
    agent: &mut JoinHandle<Result<(), voom_core::VoomError>>,
    requests: &Mutex<Vec<String>>,
) -> Result<Vec<voom_store::repo::execution::node_incarnations::NodeIncarnation>, String> {
    let task_exit_observed = AtomicBool::new(false);
    let durable_retirement_observed = AtomicBool::new(false);
    let result = tokio::time::timeout(HANG_GUARD, async {
        let task_exit = async {
            let result = agent.await;
            task_exit_observed.store(true, Ordering::SeqCst);
            result.unwrap().unwrap();
        };
        let durable_retirement = async {
            let history =
                poll_for_incarnation_status(cp, node_id, NodeIncarnationStatus::Retired).await;
            durable_retirement_observed.store(true, Ordering::SeqCst);
            history
        };
        let ((), history) = tokio::join!(task_exit, durable_retirement);
        history
    })
    .await;
    match result {
        Ok(history) => Ok(history),
        Err(error) => {
            let request_paths = requests.try_lock().map_or_else(
                |_| "request log busy".to_owned(),
                |requests| format!("{requests:?}"),
            );
            Err(format!(
                "{error}; task_exit_observed={}; durable_retirement_observed={}; \
                 requests={request_paths}",
                task_exit_observed.load(Ordering::SeqCst),
                durable_retirement_observed.load(Ordering::SeqCst),
            ))
        }
    }
}

async fn poll_for_incarnation_status(
    cp: &ControlPlane,
    node_id: NodeId,
    status: NodeIncarnationStatus,
) -> Vec<voom_store::repo::execution::node_incarnations::NodeIncarnation> {
    loop {
        let history = cp.list_node_incarnations(node_id, 10).await.unwrap();
        if history.first().is_some_and(|row| row.status == status) {
            return history;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
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

async fn wait_for_acquire_transition<T: Debug>(
    milestone: &Notify,
    agent: &mut JoinHandle<T>,
    transition: &'static str,
) -> Result<(), String> {
    tokio::select! {
        biased;
        result = agent => Err(format!("agent exited before {transition}: {result:?}")),
        () = milestone.notified() => Ok(()),
        () = tokio::time::sleep(HANG_GUARD) => {
            Err(format!("live agent never reached {transition}"))
        }
    }
}
