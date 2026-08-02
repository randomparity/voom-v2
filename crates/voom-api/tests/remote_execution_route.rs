#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::BodyExt;
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use tower::ServiceExt;
use voom_api::{router, router_with_control_plane};
use voom_control_plane::workers::{
    NewWorkerCapabilityDraft, NewWorkerGrantDraft, RegisterNodeInput, RegisterWorkerForNodeInput,
};
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{FailureClass, LeaseId, NodeId, TicketId, TicketOperation, WorkerId};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::execution::tickets::{NewTicket, SqliteTicketRepo, TicketState};
use voom_store::repo::execution::workers::WorkerKind;
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;

const OP: &str = "test.remote";

fn ticket_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

struct ApiFixture {
    _tmp: TempDatabase,
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
        self.post_json_with_authorization(
            path,
            idempotency_key,
            Some(HeaderValue::from_str(&format!("Bearer {token}")).unwrap()),
            body,
        )
        .await
    }

    async fn post_json_with_authorization(
        &self,
        path: &str,
        idempotency_key: &str,
        authorization: Option<HeaderValue>,
        body: Value,
    ) -> Response<Body> {
        let mut request = Request::post(path)
            .header("content-type", "application/json")
            .header("x-voom-idempotency-key", idempotency_key)
            .body(Body::from(body.to_string()))
            .unwrap();
        if let Some(authorization) = authorization {
            request.headers_mut().insert(AUTHORIZATION, authorization);
        }
        self.app.clone().oneshot(request).await.unwrap()
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
                kind: ticket_op(OP),
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
        assert!(json["data"]["scheduler_decision_id"].as_u64().unwrap() > 0);
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
async fn acquire_requires_idempotency_key() {
    let fixture = api_fixture().await;

    let res = fixture
        .app
        .clone()
        .oneshot(
            Request::post("/v1/execution/lease/acquire")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", fixture.token))
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
async fn execution_routes_use_one_unauthorized_bearer_response() {
    let fixture = api_fixture().await;
    let routes = [
        (
            "/v1/execution/lease/acquire".to_owned(),
            "execution.acquire",
            json!({"node_id": fixture.node_id.0, "worker_id": fixture.worker_id.0}),
        ),
        (
            format!("/v1/execution/node/{}/heartbeat", fixture.node_id.0),
            "execution.node_heartbeat",
            json!({}),
        ),
        (
            "/v1/execution/lease/1/heartbeat".to_owned(),
            "execution.lease_heartbeat",
            json!({"node_id": fixture.node_id.0, "worker_id": fixture.worker_id.0}),
        ),
        (
            "/v1/execution/lease/1/complete".to_owned(),
            "execution.complete",
            json!({
                "node_id": fixture.node_id.0,
                "worker_id": fixture.worker_id.0,
                "result": {}
            }),
        ),
        (
            "/v1/execution/lease/1/fail".to_owned(),
            "execution.fail",
            json!({
                "node_id": fixture.node_id.0,
                "worker_id": fixture.worker_id.0,
                "reason": "timed out",
                "class": FailureClass::WorkerTimeout
            }),
        ),
    ];
    let authorization_cases = [
        ("missing", None),
        ("non-utf8", Some(HeaderValue::from_bytes(&[0xff]).unwrap())),
        (
            "wrong-scheme",
            Some(HeaderValue::from_static("Basic token")),
        ),
        ("empty", Some(HeaderValue::from_static("Bearer "))),
        (
            "incorrect",
            Some(HeaderValue::from_static("Bearer incorrect-token")),
        ),
    ];

    for (authorization_name, authorization) in authorization_cases {
        for (path, command, body) in &routes {
            let response = fixture
                .post_json_with_authorization(
                    path,
                    &format!("{authorization_name}-{command}"),
                    authorization.clone(),
                    body.clone(),
                )
                .await;
            assert_unauthorized_envelope(response, command).await;
        }
    }
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
    assert!(json["data"]["scheduler_decision_id"].as_u64().unwrap() > 0);
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
            json!({
                "node_id": fixture.node_id.0,
                "worker_id": fixture.worker_id.0,
                "lease_ttl_seconds": 61
            }),
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
async fn execution_routes_reject_unknown_body_fields() {
    let fixture = api_fixture().await;

    let cases = [
        (
            "/v1/execution/lease/acquire".to_owned(),
            "acquire-unknown-body",
            "execution.acquire",
            json!({
                "node_id": fixture.node_id.0,
                "worker_id": fixture.worker_id.0,
                "unknown": true
            }),
        ),
        (
            format!("/v1/execution/node/{}/heartbeat", fixture.node_id.0),
            "node-heartbeat-unknown-body",
            "execution.node_heartbeat",
            json!({"unknown": true}),
        ),
        (
            "/v1/execution/lease/1/heartbeat".to_owned(),
            "lease-heartbeat-unknown-body",
            "execution.lease_heartbeat",
            json!({
                "node_id": fixture.node_id.0,
                "worker_id": fixture.worker_id.0,
                "unknown": true
            }),
        ),
        (
            "/v1/execution/lease/1/complete".to_owned(),
            "complete-unknown-body",
            "execution.complete",
            json!({
                "node_id": fixture.node_id.0,
                "worker_id": fixture.worker_id.0,
                "result": {},
                "unknown": true
            }),
        ),
        (
            "/v1/execution/lease/1/fail".to_owned(),
            "fail-unknown-body",
            "execution.fail",
            json!({
                "node_id": fixture.node_id.0,
                "worker_id": fixture.worker_id.0,
                "reason": "timed out",
                "class": FailureClass::WorkerTimeout,
                "unknown": true
            }),
        ),
    ];

    for (path, key, command, body) in cases {
        let response = fixture.post_json(&path, key, body).await;
        assert_bad_args_envelope(response, command).await;
    }
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

#[tokio::test]
async fn unconfigured_remote_execution_route_returns_api_error_envelope() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let app = router(HealthPlane::open(&url).await.unwrap());

    let res = app
        .oneshot(
            Request::post("/v1/execution/lease/acquire")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-token")
                .header("x-voom-idempotency-key", "unconfigured-acquire")
                .body(Body::from(r#"{"node_id":1,"worker_id":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let json = response_json(res).await;
    assert_eq!(json["schema_version"], "0");
    assert_eq!(json["command"], "execution.acquire");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "NOT_FOUND");
}

async fn api_fixture() -> ApiFixture {
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
    let worker = cp
        .register_worker_for_node(RegisterWorkerForNodeInput {
            node_id: registered.node.id,
            token: registered.token.clone(),
            name: "remote-worker".to_owned(),
            kind: WorkerKind::Remote,
            capabilities: vec![NewWorkerCapabilityDraft {
                operation: ticket_op(OP),
                codecs: vec!["json".to_owned()],
                hardware: Vec::new(),
                artifact_access: vec!["shared_mount".to_owned()],
                extra: json!({}),
            }],
            grants: vec![NewWorkerGrantDraft {
                can_execute: vec![ticket_op(OP)],
                can_access_read: Vec::new(),
                can_access_write: Vec::new(),
                denies: Vec::new(),
                max_parallel: json!({"*": 1}),
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

async fn assert_unauthorized_envelope(res: Response<Body>, command: &str) {
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers().get(WWW_AUTHENTICATE),
        Some(&HeaderValue::from_static("Bearer"))
    );
    let json = response_json(res).await;
    assert_eq!(json["schema_version"], "0");
    assert_eq!(json["command"], command);
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
    assert_eq!(
        json["error"]["message"],
        "unauthorized: remote node authentication failed"
    );
}
