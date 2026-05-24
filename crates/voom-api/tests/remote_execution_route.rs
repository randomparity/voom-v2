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
use voom_core::{NodeId, WorkerId};
use voom_store::repo::nodes::NodeKind;
use voom_store::repo::workers::WorkerKind;
use voom_store::test_support::sqlite_url_for;

const OP: &str = "test.remote";

struct ApiFixture {
    _tmp: NamedTempFile,
    app: axum::Router,
    node_id: NodeId,
    token: String,
    worker_id: WorkerId,
}

impl ApiFixture {
    async fn post_json(&self, path: &str, idempotency_key: &str, body: Value) -> Response<Body> {
        self.app
            .clone()
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", self.token))
                    .header("x-voom-idempotency-key", idempotency_key)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
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
    let app = router_with_control_plane(hp, cp);
    ApiFixture {
        _tmp: tmp,
        app,
        node_id: registered.node.id,
        token: registered.token.expose_secret().to_owned(),
        worker_id: worker.id,
    }
}

async fn response_json(res: Response<Body>) -> Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}
