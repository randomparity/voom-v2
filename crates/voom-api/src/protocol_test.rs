#![expect(
    clippy::unwrap_used,
    reason = "route tests use unwrap for fallible fixture and HTTP request construction"
)]

use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use voom_control_plane::HealthPlane;
use voom_core::PROTOCOL_VERSION;
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;

use super::PROTOCOL_VERSION_HEADER;
use crate::router;

/// Every route a node token can call (ADR 0123). The gate is applied to the merged node
/// router, so a route added to these modules is gated without touching this list.
const NODE_ROUTES: [(Method, &str); 20] = [
    (Method::POST, "/v1/execution/node/7/activate"),
    (Method::POST, "/v1/execution/node/7/deactivate"),
    (Method::POST, "/v1/execution/node/7/heartbeat"),
    (Method::POST, "/v1/execution/node/7/worker/9/readiness"),
    (Method::POST, "/v1/execution/lease/acquire"),
    (Method::POST, "/v1/execution/lease/1/heartbeat"),
    (Method::POST, "/v1/execution/lease/1/complete"),
    (Method::POST, "/v1/execution/lease/1/fail"),
    (Method::POST, "/v1/artifact/commit/open"),
    (Method::POST, "/v1/artifact/commit/1/authorize"),
    (Method::POST, "/v1/artifact/commit/1/applying"),
    (Method::POST, "/v1/artifact/commit/1/outcome"),
    (Method::POST, "/v1/artifact/commit/1/complete"),
    (Method::POST, "/v1/artifact/commit/group/1/complete"),
    (Method::POST, "/v1/scan/node/7/session/9/start"),
    (Method::POST, "/v1/scan/node/7/session/9/batch/1"),
    (Method::POST, "/v1/scan/node/7/session/9/complete"),
    (Method::POST, "/v1/scan/node/7/session/9/fail"),
    (Method::GET, "/v1/scan/node/7/session/9"),
    (Method::GET, "/v1/scan/node/7/session/9/reconciliation"),
];

const ACTIVATE: &str = "/v1/execution/node/7/activate";

async fn app() -> (TempDatabase, axum::Router) {
    let database = TempDatabase::new().unwrap();
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let app = router(HealthPlane::open(&url).await.unwrap());
    (database, app)
}

async fn send(
    app: &axum::Router,
    method: Method,
    path: &str,
    version: Option<&str>,
    bearer: bool,
) -> Response<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-voom-idempotency-key", "protocol-test");
    if let Some(version) = version {
        request = request.header(PROTOCOL_VERSION_HEADER, version);
    }
    if bearer {
        request = request.header("authorization", "Bearer not-a-real-token");
    }
    app.clone()
        .oneshot(request.body(Body::from("{}")).unwrap())
        .await
        .unwrap()
}

fn stamped_version(response: &Response<Body>) -> Option<String> {
    response
        .headers()
        .get(PROTOCOL_VERSION_HEADER)
        .map(|value| value.to_str().unwrap().to_owned())
}

async fn refusal(response: Response<Body>) -> Value {
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        stamped_version(&response),
        Some(PROTOCOL_VERSION.to_string())
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["command"], "api.request");
    assert_eq!(envelope["error"]["code"], "BAD_ARGS");
    assert!(
        envelope["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("ADR 0123")
    );
    envelope
}

fn message(envelope: &Value) -> &str {
    envelope["error"]["message"].as_str().unwrap()
}

#[tokio::test]
async fn every_node_route_refuses_a_request_without_a_version() {
    let (_database, app) = app().await;
    for (method, path) in NODE_ROUTES {
        let envelope = refusal(send(&app, method, path, None, true).await).await;
        assert!(
            message(&envelope).contains(&format!(
                "missing x-voom-protocol-version (control plane speaks protocol {PROTOCOL_VERSION})"
            )),
            "{path}: {envelope}"
        );
    }
}

#[tokio::test]
async fn skewed_versions_are_refused_by_number() {
    let (_database, app) = app().await;
    for offered in [PROTOCOL_VERSION + 1, PROTOCOL_VERSION - 1] {
        let version = offered.to_string();
        let envelope =
            refusal(send(&app, Method::POST, ACTIVATE, Some(&version), true).await).await;
        assert_eq!(
            message(&envelope),
            format!("unsupported protocol version: offered={offered}, expected {PROTOCOL_VERSION}")
        );
    }
}

#[tokio::test]
async fn malformed_versions_are_refused_without_echoing_them() {
    let (_database, app) = app().await;
    for version in ["six", "", "6.0", "+6x"] {
        let envelope = refusal(send(&app, Method::POST, ACTIVATE, Some(version), true).await).await;
        assert_eq!(
            message(&envelope),
            "malformed x-voom-protocol-version; expected a decimal version"
        );
    }
}

#[tokio::test]
async fn the_gate_runs_before_authentication() {
    let (_database, app) = app().await;
    let envelope = refusal(send(&app, Method::POST, ACTIVATE, None, false).await).await;
    assert!(message(&envelope).starts_with("missing x-voom-protocol-version"));
}

#[tokio::test]
async fn a_matching_version_reaches_the_handler_and_is_stamped() {
    let (_database, app) = app().await;
    let version = PROTOCOL_VERSION.to_string();
    let response = send(&app, Method::POST, ACTIVATE, Some(&version), false).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(stamped_version(&response), Some(version));
}

#[tokio::test]
async fn health_is_not_gated() {
    let (_database, app) = app().await;
    let response = send(&app, Method::GET, "/health", None, false).await;
    assert_ne!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(stamped_version(&response), None);
}
