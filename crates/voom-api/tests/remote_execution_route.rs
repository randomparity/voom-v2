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
use voom_core::{
    FailureClass, LeaseId, NodeId, NodeIncarnationId, TicketId, TicketOperation, WorkerId,
};
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
    incarnation_id: NodeIncarnationId,
    worker_id: WorkerId,
}

impl ApiFixture {
    async fn post_json(
        &self,
        path: &str,
        idempotency_key: &str,
        mut body: Value,
    ) -> Response<Body> {
        if let Some(body) = body.as_object_mut() {
            body.entry("incarnation_id")
                .or_insert_with(|| Value::String(self.incarnation_id.to_string()));
        }
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
async fn execution_routes_reject_former_incarnation_less_bodies() {
    let fixture = api_fixture().await;
    let cases = [
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

    for (index, (path, command, body)) in cases.into_iter().enumerate() {
        let response = fixture
            .post_json_with_token(
                &path,
                &format!("missing-incarnation-{index}"),
                &fixture.token,
                body,
            )
            .await;
        assert_bad_args_envelope(response, command).await;
    }
}

#[tokio::test]
async fn activation_and_deactivation_routes_own_the_node_lifecycle() {
    let fixture = api_fixture().await;
    let incarnation_id: NodeIncarnationId = "fedcba9876543210fedcba9876543210".parse().unwrap();
    let activate = fixture
        .post_json(
            &format!("/v1/execution/node/{}/activate", fixture.node_id.0),
            "activate-route",
            json!({
                "incarnation_id": incarnation_id,
                "workers": [{
                    "logical_name": "transcode",
                    "operations": ["transcode_video"],
                    "artifact_access": ["shared_mount"],
                    "max_parallel": 2
                }]
            }),
        )
        .await;
    assert_eq!(activate.status(), StatusCode::OK);
    let activated = response_json(activate).await;
    assert_eq!(
        activated["data"]["incarnation_id"],
        incarnation_id.to_string()
    );
    assert_eq!(activated["data"]["workers"].as_array().unwrap().len(), 1);

    let deactivate = fixture
        .post_json(
            &format!("/v1/execution/node/{}/deactivate", fixture.node_id.0),
            "deactivate-route",
            json!({
                "incarnation_id": incarnation_id,
                "reason": "graceful_shutdown"
            }),
        )
        .await;
    assert_eq!(deactivate.status(), StatusCode::OK);
    let deactivated = response_json(deactivate).await;
    assert_eq!(deactivated["data"]["status"], "retired");
    assert_eq!(deactivated["data"]["reason"], "graceful_shutdown");
}

#[tokio::test]
async fn execution_routes_use_one_unauthorized_bearer_response() {
    let fixture = api_fixture().await;
    let routes = [
        (
            "/v1/execution/lease/acquire".to_owned(),
            "execution.acquire",
            json!({
                "node_id": fixture.node_id.0,
                "incarnation_id": fixture.incarnation_id,
                "worker_id": fixture.worker_id.0
            }),
        ),
        (
            format!("/v1/execution/node/{}/heartbeat", fixture.node_id.0),
            "execution.node_heartbeat",
            json!({"incarnation_id": fixture.incarnation_id}),
        ),
        (
            "/v1/execution/lease/1/heartbeat".to_owned(),
            "execution.lease_heartbeat",
            json!({
                "node_id": fixture.node_id.0,
                "incarnation_id": fixture.incarnation_id,
                "worker_id": fixture.worker_id.0
            }),
        ),
        (
            "/v1/execution/lease/1/complete".to_owned(),
            "execution.complete",
            json!({
                "node_id": fixture.node_id.0,
                "incarnation_id": fixture.incarnation_id,
                "worker_id": fixture.worker_id.0,
                "result": {}
            }),
        ),
        (
            "/v1/execution/lease/1/fail".to_owned(),
            "execution.fail",
            json!({
                "node_id": fixture.node_id.0,
                "incarnation_id": fixture.incarnation_id,
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
async fn unconfigured_execution_routes_reject_invalid_bearer_syntax() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let app = router(HealthPlane::open(&url).await.unwrap());
    let routes = [
        (
            "/v1/execution/lease/acquire",
            "execution.acquire",
            json!({"node_id": 1, "worker_id": 1}),
        ),
        (
            "/v1/execution/node/1/heartbeat",
            "execution.node_heartbeat",
            json!({}),
        ),
        (
            "/v1/execution/lease/1/heartbeat",
            "execution.lease_heartbeat",
            json!({"node_id": 1, "worker_id": 1}),
        ),
        (
            "/v1/execution/lease/1/complete",
            "execution.complete",
            json!({"node_id": 1, "worker_id": 1, "result": {}}),
        ),
        (
            "/v1/execution/lease/1/fail",
            "execution.fail",
            json!({
                "node_id": 1,
                "worker_id": 1,
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
    ];

    for (authorization_name, authorization) in authorization_cases {
        for (path, command, body) in &routes {
            let mut request = Request::post(*path)
                .header("content-type", "application/json")
                .header(
                    "x-voom-idempotency-key",
                    format!("unconfigured-{authorization_name}-{command}"),
                )
                .body(Body::from(body.to_string()))
                .unwrap();
            if let Some(authorization) = authorization.clone() {
                request.headers_mut().insert(AUTHORIZATION, authorization);
            }

            let response = app.clone().oneshot(request).await.unwrap();
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
                "incarnation_id": fixture.incarnation_id,
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
    api_fixture_with_capabilities(
        vec![NewWorkerCapabilityDraft {
            operation: ticket_op(OP),
            codecs: vec!["json".to_owned()],
            hardware: Vec::new(),
            artifact_access: vec!["shared_mount".to_owned()],
            extra: json!({}),
        }],
        vec![NewWorkerGrantDraft {
            can_execute: vec![ticket_op(OP)],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({"*": 1}),
        }],
    )
    .await
}

/// The fixture's single worker is registered with exactly the capability and
/// grant drafts given, before the node incarnation is activated.
async fn api_fixture_with_capabilities(
    capabilities: Vec<NewWorkerCapabilityDraft>,
    grants: Vec<NewWorkerGrantDraft>,
) -> ApiFixture {
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
            capabilities,
            grants,
        })
        .await
        .unwrap();
    let incarnation_id: NodeIncarnationId = "0123456789abcdef0123456789abcdef".parse().unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES (?, ?, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(incarnation_id.to_string())
    .bind(i64::try_from(registered.node.id.0).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE nodes SET status = 'active', active_incarnation_id = ? WHERE id = ?")
        .bind(incarnation_id.to_string())
        .bind(i64::try_from(registered.node.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET node_incarnation_id = ? WHERE id = ?")
        .bind(incarnation_id.to_string())
        .bind(i64::try_from(worker.id.0).unwrap())
        .execute(&pool)
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
        incarnation_id,
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
        Some(&HeaderValue::from_static("Bearer realm=\"voom\""))
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

/// One successful owner-local acquisition over the remote API (issue #478):
/// a namespaced byte-work ticket whose canonical declaration resolves to the
/// acquiring node leases, and the leased response dispatches the normalized
/// bare operation together with the plan identity and its owner evidence.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the owner-local fixture seeding and the binding assertions read best inline"
)]
async fn acquire_leased_owner_local_byte_work_dispatches_normalized_operation() {
    use voom_core::StorageProviderKind;
    use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
    use voom_store::repo::library::library_roots::{
        HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
    };
    let fixture = api_fixture_with_capabilities(
        vec![
            NewWorkerCapabilityDraft {
                operation: ticket_op("synthetic.workflow.operation.transcode_video"),
                codecs: vec!["json".to_owned()],
                hardware: Vec::new(),
                artifact_access: vec!["shared_mount".to_owned()],
                extra: json!({}),
            },
            NewWorkerCapabilityDraft {
                operation: ticket_op("transcode_video"),
                codecs: vec!["json".to_owned()],
                hardware: Vec::new(),
                artifact_access: vec!["shared_mount".to_owned()],
                extra: json!({}),
            },
        ],
        vec![NewWorkerGrantDraft {
            can_execute: vec![
                ticket_op("synthetic.workflow.operation.transcode_video"),
                ticket_op("transcode_video"),
            ],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({"*": 1}),
        }],
    )
    .await;
    // A live root owned by the acquiring node plus one rooted location, so the
    // declaration resolves owner-local.
    let library = fixture
        .cp
        .create_library(NewLibrary {
            slug: "owner-local-api".to_owned(),
            display_name: "Owner local api".to_owned(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = fixture
        .cp
        .create_library_root(NewLibraryRoot {
            library_id: library.id,
            owner_node_id: fixture.node_id,
            provider_kind: StorageProviderKind::LocalFilesystem,
            provider_locator: voom_core::ProviderLocator::new("/owner-local-api".to_owned())
                .unwrap(),
            display_locator: "/owner-local-api".to_owned(),
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extension_allowlist: Vec::new(),
            scan_mode: LibraryScanMode::ManualRecursive,
            symlink_policy: SymlinkPolicy::Reject,
            hidden_file_policy: HiddenFilePolicy::Ignore,
            max_depth: None,
            stability_seconds: 0,
            debounce_seconds: 0,
            default_output_root_id: None,
            default_staging_root_id: None,
            default_backup_root_id: None,
            enabled: true,
        })
        .await
        .unwrap();
    fixture
        .cp
        .activate_library_root(root.id, "owner-local-api".to_owned())
        .await
        .unwrap();
    let pool = voom_store::connect(&fixture.url).await.unwrap();
    let asset_id = sqlx::query("INSERT INTO file_assets (created_at, epoch) VALUES (?, 0)")
        .bind("1970-01-01T00:00:00Z")
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let version_id = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at, retired_at, epoch) \
         VALUES (?, ?, 1, 'ingest', NULL, '1970-01-01T00:00:00Z', NULL, 0)",
    )
    .bind(asset_id)
    .bind("owner-local-api")
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let location_id = sqlx::query(
        "INSERT INTO file_locations (file_version_id, address_state, storage_root_id, \
         provider_relative_locator, observed_at, epoch) \
         VALUES (?, 'rooted', ?, ?, '1970-01-01T00:00:00Z', 0)",
    )
    .bind(version_id)
    .bind(i64::try_from(root.id.0).unwrap())
    .bind("movie.mkv")
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();

    // The workflow payload is frozen to the canonical encoding ADR 0069 pins:
    // storage_root write entry first, then the file_location read entry.
    let payload = json!({
        "workflow_id": "wf-api",
        "plan_id": "plan-api",
        "node_id": "node-api",
        "branch_id": "branch-api",
        "operation": "transcode_video",
        "rendered_payload": {
            "operation": "transcode_video",
            "source_storage_root_id": root.id.0,
            "source_location_id": location_id,
        },
        "timing": {"duration_ms": 25, "progress_interval_ms": 10},
        "declared_artifact_access": [
            {"target": {"kind": "storage_root", "storage_root_id": root.id.0}, "rights": ["write"]},
            {"target": {"kind": "file_location", "storage_root_id": root.id.0,
                        "file_location_id": location_id}, "rights": ["read"]}
        ],
    });
    let ticket = SqliteTicketRepo::new(pool.clone())
        .create(NewTicket {
            job_id: None,
            kind: ticket_op("synthetic.workflow.operation.transcode_video"),
            priority: 0,
            payload,
            max_attempts: 2,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    fixture
        .cp
        .mark_ready_if_unblocked(ticket.id, time::OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();

    let res = fixture
        .post_json(
            "/v1/execution/lease/acquire",
            "owner-local-acquire",
            json!({"node_id": fixture.node_id.0, "worker_id": fixture.worker_id.0}),
        )
        .await;
    let status = res.status();
    let json = response_json(res).await;
    assert_eq!(status, StatusCode::OK, "body: {json}");
    assert_eq!(json["data"]["outcome"], "leased");
    // Criterion 4: normalized before dispatch — never the namespaced token.
    assert_eq!(json["data"]["operation"], "transcode_video");
    let plan = &json["data"]["artifact_access_plan"];
    assert!(plan["id"].as_u64().unwrap() > 0);
    assert_eq!(plan["owner_node_id"], json!(fixture.node_id.0));
    let evidence = &plan["access_evidence"];
    assert!(
        evidence.is_object()
            && evidence["declaration"].is_array()
            && !evidence["declaration"].as_array().unwrap().is_empty()
            && evidence["root_epochs"].is_array(),
        "the plan carries the canonical access evidence: {evidence}"
    );

    // The decision row binds decision, lease, ticket, worker, owner, evidence.
    let decision_id = json["data"]["scheduler_decision_id"].as_u64().unwrap();
    let lease_id = json["data"]["lease_id"].as_u64().unwrap();
    let row: (Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT selected_lease_id, access_evidence FROM scheduler_decisions WHERE id = ?",
    )
    .bind(i64::try_from(decision_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, Some(i64::try_from(lease_id).unwrap()));
    assert!(row.1.is_some());
}
