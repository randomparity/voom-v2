use secrecy::ExposeSecret;
use serde_json::json;
use voom_api::router_with_control_plane;
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{NodeId, OperationKind, TicketId, TicketOperation};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::execution::tickets::{NewTicket, SqliteTicketRepo, TicketState};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;

use super::{RemoteRunnerConfig, RemoteSyntheticRunner};

const OP: &str = "transcode_video";

fn ticket_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

fn transcode_video_ticket(input_path: &str, output_name: &str) -> serde_json::Value {
    let output_path = std::env::temp_dir().join(format!(
        "voom-remote-runner-{}-{output_name}",
        std::process::id()
    ));
    json!({
        "input": {
            "path": input_path,
            "expected": {
                "size_bytes": 5_u64,
                "content_hash": "blake3:input"
            }
        },
        "output": {
            "staging_root": output_path.parent().unwrap().to_string_lossy().into_owned(),
            "path": output_path.to_string_lossy().into_owned(),
            "container": "mkv",
            "video_codec": "hevc",
            "overwrite": true
        },
        "profile": {
            "name": "default-hevc",
            "target_codec": "hevc",
            "encoder": "libx265",
            "crf": 23_u8,
            "preset": "medium"
        },
        "artifact_access": {
            "inputs": ["handle:input:test"],
            "outputs": ["handle:output:test"]
        }
    })
}

#[tokio::test]
async fn runner_polls_acquires_dispatches_heartbeats_and_completes() {
    let fixture = RemoteRunnerFixture::new().await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/movie.mkv",
            "runner-primary.mkv",
        ))
        .await;

    let mut config = fixture.config();
    config.base_url.push('/');
    let summary = RemoteSyntheticRunner::new(config)
        .run_once_to_completion()
        .await
        .unwrap();

    assert_eq!(summary.acquired, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.idle_polls, 0);
    assert_eq!(
        fixture.ticket_state(ticket_id).await,
        TicketState::Succeeded
    );
}

#[tokio::test]
async fn runner_uses_fresh_idempotency_keys_for_each_run() {
    let fixture = RemoteRunnerFixture::new().await;
    let first_ticket = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/movie.mkv",
            "runner-first.mkv",
        ))
        .await;
    let runner = RemoteSyntheticRunner::new(fixture.config());

    let first = runner.run_once_to_completion().await.unwrap();
    let second_ticket = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/second.mkv",
            "runner-second.mkv",
        ))
        .await;
    let second = runner.run_once_to_completion().await.unwrap();

    assert_eq!(first.completed, 1);
    assert_eq!(second.completed, 1);
    assert_eq!(
        fixture.ticket_state(first_ticket).await,
        TicketState::Succeeded
    );
    assert_eq!(
        fixture.ticket_state(second_ticket).await,
        TicketState::Succeeded
    );
}

#[tokio::test]
async fn runner_instances_use_random_idempotency_run_ids() {
    let first = super::new_run_id();
    let second = super::new_run_id();

    assert_eq!(first.len(), 32);
    assert_ne!(first, second);
}

#[tokio::test]
async fn runner_activation_declares_configured_artifact_access() {
    let fixture = RemoteRunnerFixture::new().await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/movie.mkv",
            "runner-failure.mkv",
        ))
        .await;

    let mut config = fixture.config();
    config.artifact_access = vec!["control_plane_placeholder".to_owned()];
    let summary = RemoteSyntheticRunner::new(config)
        .run_once_to_completion()
        .await
        .unwrap();

    assert_eq!(summary.acquired, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        fixture.ticket_state(ticket_id).await,
        TicketState::Succeeded
    );
}

struct RemoteRunnerFixture {
    _tmp: TempDatabase,
    url: String,
    base_url: String,
    cp: ControlPlane,
    server: tokio::task::JoinHandle<()>,
    node_id: NodeId,
    token: secrecy::SecretString,
}

impl RemoteRunnerFixture {
    async fn new() -> Self {
        let tmp = TempDatabase::new().unwrap();
        let url = sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        let cp = ControlPlane::open(&url).await.unwrap();
        let registered = cp
            .register_node(RegisterNodeInput {
                name: "remote-node".to_owned(),
                kind: NodeKind::Remote,
                heartbeat_ttl_seconds: 60,
                metadata: json!({}),
            })
            .await
            .unwrap();
        let health = HealthPlane::open(&url).await.unwrap();
        let app = router_with_control_plane(health, cp.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            _tmp: tmp,
            url,
            base_url: format!("http://{addr}"),
            cp,
            server,
            node_id: registered.node.id,
            token: registered.token,
        }
    }

    fn config(&self) -> RemoteRunnerConfig {
        RemoteRunnerConfig {
            base_url: self.base_url.clone(),
            node_id: self.node_id,
            token: self.token.expose_secret().to_owned().into(),
            worker_logical_name: "remote-worker".to_owned(),
            operations: vec![OperationKind::TranscodeVideo],
            artifact_access: vec!["shared_mount".to_owned()],
            max_parallel: 1,
            max_polls: 3,
            idle_timeout: std::time::Duration::from_millis(100),
            lease_heartbeat_interval: std::time::Duration::from_millis(10),
        }
    }

    async fn ready_ticket(&self, payload: serde_json::Value) -> TicketId {
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: ticket_op(OP),
                priority: 0,
                payload,
                max_attempts: 2,
                created_at: time::OffsetDateTime::UNIX_EPOCH,
            })
            .await
            .unwrap();
        self.cp
            .mark_ready_if_unblocked(ticket.id, time::OffsetDateTime::UNIX_EPOCH)
            .await
            .unwrap();
        ticket.id
    }

    async fn ticket_state(&self, ticket_id: TicketId) -> TicketState {
        let pool = voom_store::connect(&self.url).await.unwrap();
        SqliteTicketRepo::new(pool)
            .get(ticket_id)
            .await
            .unwrap()
            .unwrap()
            .state
    }
}

impl Drop for RemoteRunnerFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Issue #478: the synthetic runner consumes the acquire response's dispatch
/// contract end to end — the leased byte-work operation (already normalized
/// by the control plane before dispatch) executes and completes, and the
/// durable records bind one lease to its access plan and selected decision.
///
/// The activation surface declares closed bare `OperationKind` tokens only
/// (ADR 0071/#423), so this e2e exercises the canonical encoding; the
/// namespaced-encoding equivalence and owner-local evidence are covered at
/// the control-plane and API surfaces where worker registration is explicit.
#[tokio::test]
async fn runner_completes_acquired_byte_work_with_bound_plan_and_decision() {
    let fixture = RemoteRunnerFixture::new().await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/movie.mkv",
            "runner-plan-bind.mkv",
        ))
        .await;

    let summary = RemoteSyntheticRunner::new(fixture.config())
        .run_once_to_completion()
        .await
        .unwrap();
    assert_eq!(summary.acquired, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        fixture.ticket_state(ticket_id).await,
        TicketState::Succeeded
    );

    // Exactly one lease, exactly one plan bound to it, and the selected
    // decision names that same lease.
    let pool = voom_store::connect(&fixture.url).await.unwrap();
    use voom_store::repo::execution::leases::{LeaseFilter, SqliteLeaseRepo};
    use voom_store::repo::execution::scheduler_decisions::{
        SchedulerDecisionFilter, SchedulerDecisionOutcome,
    };
    // The completed lease has left `held`, so list without a state filter.
    let leases = SqliteLeaseRepo::new(pool.clone())
        .list(LeaseFilter { state: None }, None, 10)
        .await
        .unwrap();
    assert_eq!(
        leases.len(),
        1,
        "exactly one lease was acquired for the byte-work ticket"
    );
    let plans = voom_store::repo::media::artifact_access_plans::SqliteArtifactAccessPlanRepo::new(
        pool.clone(),
    )
    .list_by_ticket(ticket_id)
    .await
    .unwrap();
    assert_eq!(plans.len(), 1, "exactly one plan binds the ticket's lease");
    let decisions =
        voom_store::repo::execution::scheduler_decisions::SqliteSchedulerDecisionRepo::new(pool)
            .list(SchedulerDecisionFilter {
                ticket_id: Some(ticket_id),
                outcome: Some(SchedulerDecisionOutcome::Selected),
                limit: 10,
                ..SchedulerDecisionFilter::default()
            })
            .await
            .unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0].selected_lease_id,
        Some(plans[0].lease_id),
        "the selected decision and the plan bind the same lease"
    );
}
