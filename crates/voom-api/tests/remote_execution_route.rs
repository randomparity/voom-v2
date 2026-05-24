#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use tempfile::NamedTempFile;
use tower::ServiceExt;
use voom_api::router_with_control_plane;
use voom_control_plane::cases::{
    nodes::RegisterNodeInput,
    workers::{NewWorkerCapabilityDraft, NewWorkerGrantDraft, RegisterWorkerForNodeInput},
};
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{FailureClass, LeaseId, NodeId, TicketId, WorkerId};
use voom_store::repo::nodes::NodeKind;
use voom_store::repo::tickets::{NewTicket, SqliteTicketRepo, TicketRepo, TicketState};
use voom_store::repo::workers::WorkerKind;
use voom_store::test_support::sqlite_url_for;

const OP: &str = "test.remote";

struct ApiFixture {
    _tmp: NamedTempFile,
    url: String,
    app: axum::Router,
    cp: ControlPlane,
    node_id: NodeId,
    token: String,
    worker_id: WorkerId,
}

impl ApiFixture {
    async fn post_json(&self, path: &str, idempotency_key: &str, body: Value) -> Response<Body> {
        self.post_json_with_token(path, idempotency_key, &self.token, body)
            .await
    }

    async fn post_json_with_token(
        &self,
        path: &str,
        idempotency_key: &str,
        token: &str,
        body: Value,
    ) -> Response<Body> {
        self.app
            .clone()
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {token}"))
                    .header("x-voom-idempotency-key", idempotency_key)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post_raw(&self, path: &str, idempotency_key: &str, body: &str) -> Response<Body> {
        self.app
            .clone()
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", self.token))
                    .header("x-voom-idempotency-key", idempotency_key)
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn ready_ticket(&self) -> TicketId {
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: OP.to_owned(),
                priority: 0,
                payload: json!({
                    "dispatch": {"kind": OP},
                    "artifact_access": {
                        "inputs": ["handle:input:route"],
                        "outputs": ["handle:output:route"]
                    }
                }),
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

    async fn acquire_lease(&self, key: &str) -> (LeaseId, TicketId) {
        self.ready_ticket().await;
        let res = self
            .post_json(
                "/v1/execution/lease/acquire",
                key,
                json!({"node_id": self.node_id.0, "worker_id": self.worker_id.0}),
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = response_json(res).await;
        assert_eq!(json["data"]["outcome"], "leased");
        (
            LeaseId(json["data"]["lease_id"].as_u64().unwrap()),
            TicketId(json["data"]["ticket_id"].as_u64().unwrap()),
        )
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

#[tokio::test]
async fn acquire_requires_bearer_token_and_idempotency_key() {
    let fixture = api_fixture().await;

    let res = fixture
        .app
        .clone()
        .oneshot(
            Request::post("/v1/execution/lease/acquire")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"node_id":1,"worker_id":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let json = response_json(res).await;
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[tokio::test]
async fn acquire_returns_idle_as_success() {
    let fixture = api_fixture().await;

    let res = fixture
        .post_json(
            "/v1/execution/lease/acquire",
            "idle-key",
            json!({"node_id": fixture.node_id.0, "worker_id": fixture.worker_id.0}),
        )
        .await;

    assert_eq!(res.status(), StatusCode::OK);
    let json = response_json(res).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["outcome"], "idle");
    assert!(
        json.get("local").is_none(),
        "API must not include local block"
    );
}

#[tokio::test]
async fn acquire_same_key_replays_and_different_body_conflicts() {
    let fixture = api_fixture().await;
    let body = json!({"node_id": fixture.node_id.0, "worker_id": fixture.worker_id.0});

    let first = fixture
        .post_json("/v1/execution/lease/acquire", "same-key", body.clone())
        .await;
    let replay = fixture
        .post_json("/v1/execution/lease/acquire", "same-key", body)
        .await;
    let conflict = fixture
        .post_json(
            "/v1/execution/lease/acquire",
            "same-key",
            json!({"node_id": fixture.node_id.0, "worker_id": fixture.worker_id.0, "extra": true}),
        )
        .await;

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(response_json(first).await, response_json(replay).await);
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(response_json(conflict).await["error"]["code"], "CONFLICT");
}

#[tokio::test]
async fn node_and_lease_heartbeat_routes_are_idempotent() {
    let fixture = api_fixture().await;

    let node_body = json!({});
    let first = fixture
        .post_json(
            &format!("/v1/execution/node/{}/heartbeat", fixture.node_id.0),
            "node-heartbeat-key",
            node_body.clone(),
        )
        .await;
    let replay = fixture
        .post_json(
            &format!("/v1/execution/node/{}/heartbeat", fixture.node_id.0),
            "node-heartbeat-key",
            node_body,
        )
        .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(response_json(first).await, response_json(replay).await);

    let (lease_id, _) = fixture.acquire_lease("heartbeat-acquire-key").await;
    let lease_body = json!({
        "node_id": fixture.node_id.0,
        "worker_id": fixture.worker_id.0,
        "lease_ttl_seconds": 60
    });
    let first = fixture
        .post_json(
            &format!("/v1/execution/lease/{}/heartbeat", lease_id.0),
            "lease-heartbeat-key",
            lease_body.clone(),
        )
        .await;
    let replay = fixture
        .post_json(
            &format!("/v1/execution/lease/{}/heartbeat", lease_id.0),
            "lease-heartbeat-key",
            lease_body,
        )
        .await;

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(response_json(first).await, response_json(replay).await);
}

#[tokio::test]
async fn complete_route_releases_ticket_consumes_plan_and_replays() {
    let fixture = api_fixture().await;
    let (lease_id, ticket_id) = fixture.acquire_lease("complete-acquire-key").await;
    let body = json!({
        "node_id": fixture.node_id.0,
        "worker_id": fixture.worker_id.0,
        "result": {
            "ok": true,
            "artifact_access": {
                "validated": true,
                "mode": "shared_mount",
                "inputs_consumed": ["handle:input:route"],
                "outputs_declared": ["handle:output:route"]
            }
        }
    });

    let first = fixture
        .post_json(
            &format!("/v1/execution/lease/{}/complete", lease_id.0),
            "complete-key",
            body.clone(),
        )
        .await;
    let replay = fixture
        .post_json(
            &format!("/v1/execution/lease/{}/complete", lease_id.0),
            "complete-key",
            body,
        )
        .await;

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(response_json(first).await, response_json(replay).await);
    assert_eq!(
        fixture.ticket_state(ticket_id).await,
        TicketState::Succeeded
    );
}

#[tokio::test]
async fn fail_route_fails_ticket_rejects_plan_and_replays() {
    let fixture = api_fixture().await;
    let (lease_id, ticket_id) = fixture.acquire_lease("fail-acquire-key").await;
    let body = json!({
        "node_id": fixture.node_id.0,
        "worker_id": fixture.worker_id.0,
        "reason": "artifact access mode shared_mount is not available",
        "class": FailureClass::ArtifactUnavailable,
        "evidence": {"validated": false}
    });

    let first = fixture
        .post_json(
            &format!("/v1/execution/lease/{}/fail", lease_id.0),
            "fail-key",
            body.clone(),
        )
        .await;
    let replay = fixture
        .post_json(
            &format!("/v1/execution/lease/{}/fail", lease_id.0),
            "fail-key",
            body,
        )
        .await;

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(response_json(first).await, response_json(replay).await);
    assert_eq!(fixture.ticket_state(ticket_id).await, TicketState::Ready);
}

#[tokio::test]
async fn lease_routes_reject_worker_node_mismatch() {
    let fixture = api_fixture().await;
    let other = fixture
        .cp
        .register_node(RegisterNodeInput {
            name: "other-remote-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let (lease_id, _) = fixture.acquire_lease("mismatch-acquire-key").await;

    let res = fixture
        .post_json_with_token(
            &format!("/v1/execution/lease/{}/heartbeat", lease_id.0),
            "mismatch-heartbeat-key",
            other.token.expose_secret(),
            json!({
                "node_id": other.node.id.0,
                "worker_id": fixture.worker_id.0,
                "lease_ttl_seconds": 60
            }),
        )
        .await;

    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(response_json(res).await["error"]["code"], "CONFLICT");
}

#[tokio::test]
async fn malformed_json_returns_api_error_envelope() {
    let fixture = api_fixture().await;

    let acquire = fixture
        .post_raw("/v1/execution/lease/acquire", "bad-acquire-json", "{")
        .await;
    assert_bad_args_envelope(acquire, "execution.acquire").await;

    let node_heartbeat = fixture
        .post_raw(
            &format!("/v1/execution/node/{}/heartbeat", fixture.node_id.0),
            "bad-node-heartbeat-json",
            "{",
        )
        .await;
    assert_bad_args_envelope(node_heartbeat, "execution.node_heartbeat").await;

    let lease_heartbeat = fixture
        .post_raw(
            "/v1/execution/lease/1/heartbeat",
            "bad-lease-heartbeat-json",
            "{",
        )
        .await;
    assert_bad_args_envelope(lease_heartbeat, "execution.lease_heartbeat").await;

    let complete = fixture
        .post_raw("/v1/execution/lease/1/complete", "bad-complete-json", "{")
        .await;
    assert_bad_args_envelope(complete, "execution.complete").await;

    let fail = fixture
        .post_raw("/v1/execution/lease/1/fail", "bad-fail-json", "{")
        .await;
    assert_bad_args_envelope(fail, "execution.fail").await;
}

#[tokio::test]
async fn malformed_path_ids_return_api_error_envelope() {
    let fixture = api_fixture().await;

    let node_heartbeat = fixture
        .post_raw(
            "/v1/execution/node/not-a-node/heartbeat",
            "bad-node-path",
            "{}",
        )
        .await;
    assert_bad_args_envelope(node_heartbeat, "execution.node_heartbeat").await;

    let lease_heartbeat = fixture
        .post_raw(
            "/v1/execution/lease/not-a-lease/heartbeat",
            "bad-lease-heartbeat-path",
            "{}",
        )
        .await;
    assert_bad_args_envelope(lease_heartbeat, "execution.lease_heartbeat").await;

    let complete = fixture
        .post_raw(
            "/v1/execution/lease/not-a-lease/complete",
            "bad-complete-path",
            "{}",
        )
        .await;
    assert_bad_args_envelope(complete, "execution.complete").await;

    let fail = fixture
        .post_raw(
            "/v1/execution/lease/not-a-lease/fail",
            "bad-fail-path",
            "{}",
        )
        .await;
    assert_bad_args_envelope(fail, "execution.fail").await;
}

async fn api_fixture() -> ApiFixture {
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
    let hp = HealthPlane::open(&url).await.unwrap();
    let app = router_with_control_plane(hp, cp.clone());
    ApiFixture {
        _tmp: tmp,
        url,
        app,
        cp,
        node_id: registered.node.id,
        token: registered.token.expose_secret().to_owned(),
        worker_id: worker.id,
    }
}

async fn response_json(res: Response<Body>) -> Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

async fn assert_bad_args_envelope(res: Response<Body>, command: &str) {
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let json = response_json(res).await;
    assert_eq!(json["schema_version"], "0");
    assert_eq!(json["command"], command);
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}
