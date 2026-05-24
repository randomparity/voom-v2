use secrecy::ExposeSecret;
use serde_json::json;
use tempfile::NamedTempFile;
use voom_api::router_with_control_plane;
use voom_control_plane::cases::nodes::RegisterNodeInput;
use voom_control_plane::cases::workers::{
    NewWorkerCapabilityDraft, NewWorkerGrantDraft, RegisterWorkerForNodeInput,
};
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{NodeId, TicketId, WorkerId};
use voom_store::repo::nodes::NodeKind;
use voom_store::repo::tickets::{NewTicket, SqliteTicketRepo, TicketRepo, TicketState};
use voom_store::repo::workers::WorkerKind;
use voom_store::test_support::sqlite_url_for;

use super::{RemoteRunnerConfig, RemoteSyntheticRunner};

const OP: &str = "transcode_video";

#[tokio::test]
async fn runner_polls_acquires_dispatches_heartbeats_and_completes() {
    let fixture = RemoteRunnerFixture::new().await;
    let ticket_id = fixture
        .ready_ticket(json!({
            "path": "/library/movie.mkv",
            "target_codec": "h265",
            "worker_artifact_access": ["shared_mount"],
            "artifact_access": {
                "inputs": ["handle:input:test"],
                "outputs": ["handle:output:test"]
            }
        }))
        .await;

    let summary = RemoteSyntheticRunner::new(fixture.config())
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
async fn runner_fails_lease_when_artifact_access_is_incompatible() {
    let fixture = RemoteRunnerFixture::new().await;
    let ticket_id = fixture
        .ready_ticket(json!({
            "path": "/library/movie.mkv",
            "target_codec": "h265",
            "worker_artifact_access": ["control_plane_placeholder"],
            "artifact_access": {
                "inputs": ["handle:input:test"],
                "outputs": ["handle:output:test"]
            }
        }))
        .await;

    let summary = RemoteSyntheticRunner::new(fixture.config())
        .run_once_to_completion()
        .await
        .unwrap();

    assert_eq!(summary.acquired, 1);
    assert_eq!(summary.completed, 0);
    assert_eq!(summary.failed, 1);
    assert_eq!(fixture.ticket_state(ticket_id).await, TicketState::Ready);
}

struct RemoteRunnerFixture {
    _tmp: NamedTempFile,
    url: String,
    base_url: String,
    cp: ControlPlane,
    server: tokio::task::JoinHandle<()>,
    node_id: NodeId,
    token: secrecy::SecretString,
    worker_id: WorkerId,
}

impl RemoteRunnerFixture {
    async fn new() -> Self {
        let tmp = NamedTempFile::new().unwrap();
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
        let worker = cp
            .register_worker_for_node(RegisterWorkerForNodeInput {
                node_id: registered.node.id,
                token: registered.token.clone(),
                name: "remote-worker".to_owned(),
                kind: WorkerKind::Remote,
                capabilities: vec![NewWorkerCapabilityDraft {
                    operation: OP.to_owned(),
                    codecs: vec!["json".to_owned()],
                    hardware: Vec::new(),
                    artifact_access: vec!["shared_mount".to_owned()],
                    extra: json!({}),
                }],
                grants: vec![NewWorkerGrantDraft {
                    can_execute: vec![OP.to_owned()],
                    can_access_read: Vec::new(),
                    can_access_write: Vec::new(),
                    denies: Vec::new(),
                    max_parallel: json!({"limit": 1}),
                }],
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
            worker_id: worker.id,
        }
    }

    fn config(&self) -> RemoteRunnerConfig {
        RemoteRunnerConfig {
            base_url: self.base_url.clone(),
            node_id: self.node_id,
            token: self.token.expose_secret().to_owned().into(),
            worker_id: self.worker_id,
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
                kind: OP.to_owned(),
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
