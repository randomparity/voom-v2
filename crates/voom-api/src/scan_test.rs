#![expect(
    clippy::unwrap_used,
    reason = "route tests use unwrap for fallible fixture and HTTP request construction"
)]

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, Method, Request, Response, StatusCode};
use http_body::{Body as HttpBody, Frame};
use http_body_util::BodyExt;
use secrecy::ExposeSecret;
use serde_json::Value;
use serde_json::json;
use tower::ServiceExt;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{
    ArtifactAccessMode, LibraryId, NodeId, NodeIncarnationId, OperationKind, ProviderLocator,
    ProviderRelativeLocator, StorageProviderKind, StorageRootId,
};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;

use super::{
    BatchRequest, CompleteRequest, FailRequest, ScanObservationRequest, StartRequest,
    stable_request_hash,
};
use crate::config::ServerLimits;
use crate::server::bounded_router;
use crate::{router, router_with_control_plane};

const INCARNATION: &str = "0123456789abcdef0123456789abcdef";
const OTHER_INCARNATION: &str = "fedcba9876543210fedcba9876543210";

struct ScanApiFixture {
    _database: TempDatabase,
    pool: sqlx::SqlitePool,
    app: axum::Router,
    cp: ControlPlane,
    node_id: NodeId,
    token: String,
    incarnation_id: NodeIncarnationId,
    other_node_id: NodeId,
    other_token: String,
    other_incarnation_id: NodeIncarnationId,
    root_id: StorageRootId,
}

struct ScanRouteCase {
    method: Method,
    path: String,
    body: Option<Value>,
    command: &'static str,
}

impl ScanApiFixture {
    async fn request_session(&self) -> voom_control_plane::scan::ScanSession {
        self.cp
            .request_scan_session(self.root_id, 300)
            .await
            .unwrap()
    }

    async fn post(&self, path: &str, key: &str, body: Value) -> Response<Body> {
        self.post_as(path, key, self.node_id, &self.token, body)
            .await
    }

    async fn post_as(
        &self,
        path: &str,
        key: &str,
        node_id: NodeId,
        token: &str,
        body: Value,
    ) -> Response<Body> {
        let path = path.replace("{node_id}", &node_id.0.to_string());
        self.app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .header("x-voom-idempotency-key", key)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post_raw(&self, path: &str, key: &str, body: &str) -> Response<Body> {
        let path = path.replace("{node_id}", &self.node_id.0.to_string());
        self.app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(AUTHORIZATION, format!("Bearer {}", self.token))
                    .header("content-type", "application/json")
                    .header("x-voom-idempotency-key", key)
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn get(&self, path: &str) -> Response<Body> {
        self.get_as(path, self.node_id, &self.token).await
    }

    async fn get_as(&self, path: &str, node_id: NodeId, token: &str) -> Response<Body> {
        let path = path.replace("{node_id}", &node_id.0.to_string());
        self.app
            .clone()
            .oneshot(
                Request::get(path)
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn scan_session_routes_are_registered_at_the_approved_paths() {
    let database = TempDatabase::new().unwrap();
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let app = router(HealthPlane::open(&url).await.unwrap());

    for (method, path) in [
        ("POST", "/v1/scan/node/1/session/1/start"),
        ("POST", "/v1/scan/node/1/session/1/batch/0"),
        ("POST", "/v1/scan/node/1/session/1/complete"),
        ("POST", "/v1/scan/node/1/session/1/fail"),
        (
            "GET",
            "/v1/scan/node/1/session/1?incarnation_id=0123456789abcdef0123456789abcdef",
        ),
        (
            "GET",
            "/v1/scan/node/1/session/1/reconciliation?incarnation_id=0123456789abcdef0123456789abcdef",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[tokio::test]
async fn scan_routes_reject_credentials_before_revealing_unknown_session_ids() {
    let app = unconfigured_app().await;
    for (method, path) in [
        ("POST", "/v1/scan/node/9/session/999/start"),
        ("POST", "/v1/scan/node/9/session/999/batch/7"),
        ("POST", "/v1/scan/node/9/session/999/complete"),
        ("POST", "/v1/scan/node/9/session/999/fail"),
        (
            "GET",
            "/v1/scan/node/9/session/999?incarnation_id=0123456789abcdef0123456789abcdef",
        ),
        (
            "GET",
            "/v1/scan/node/9/session/999/reconciliation?incarnation_id=0123456789abcdef0123456789abcdef",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            response.headers().get(WWW_AUTHENTICATE),
            Some(&HeaderValue::from_static("Bearer realm=\"voom\"")),
            "{path}",
        );
        let body = response_body(response).await;
        assert_eq!(body["error"]["code"], "UNAUTHORIZED", "{path}");
        assert_eq!(
            body["error"]["message"], "unauthorized: remote node authentication failed",
            "{path}",
        );
        assert!(!body.to_string().contains("999"), "{path}");
    }
}

#[tokio::test]
async fn scan_mutations_require_idempotency_but_inspection_does_not() {
    let app = unconfigured_app().await;
    for path in [
        "/v1/scan/node/1/session/1/start",
        "/v1/scan/node/1/session/1/batch/0",
        "/v1/scan/node/1/session/1/complete",
        "/v1/scan/node/1/session/1/fail",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(AUTHORIZATION, "Bearer syntactically-valid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(
            response_body(response).await["error"]["code"],
            "BAD_ARGS",
            "{path}",
        );
    }

    for path in [
        "/v1/scan/node/1/session/1?incarnation_id=0123456789abcdef0123456789abcdef",
        "/v1/scan/node/1/session/1/reconciliation?incarnation_id=0123456789abcdef0123456789abcdef",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(path)
                    .header(AUTHORIZATION, "Bearer syntactically-valid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
}

#[tokio::test]
async fn scan_routes_reject_unknown_fields_and_malformed_path_or_query() {
    let fixture = scan_fixture().await;
    assert_eq!(fixture.incarnation_id.to_string(), INCARNATION);
    let session = fixture.request_session().await;
    let cases = [
        (
            format!("/v1/scan/node/{{node_id}}/session/{}/start", session.id.0),
            json!({"incarnation_id": INCARNATION, "unknown": true}),
        ),
        (
            format!("/v1/scan/node/{{node_id}}/session/{}/batch/0", session.id.0),
            json!({"incarnation_id": INCARNATION, "observations": [], "unknown": true}),
        ),
        (
            format!(
                "/v1/scan/node/{{node_id}}/session/{}/complete",
                session.id.0
            ),
            json!({
                "incarnation_id": INCARNATION,
                "last_sequence": null,
                "observation_count": 0,
                "unknown": true
            }),
        ),
        (
            format!("/v1/scan/node/{{node_id}}/session/{}/fail", session.id.0),
            json!({"incarnation_id": INCARNATION, "reason": "failed", "unknown": true}),
        ),
    ];
    for (index, (path, body)) in cases.into_iter().enumerate() {
        let response = fixture.post(&path, &format!("strict-{index}"), body).await;
        assert_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;
    }

    let response = fixture
        .post(
            "/v1/scan/node/{node_id}/session/not-a-session/start",
            "bad-path",
            json!({"incarnation_id": INCARNATION}),
        )
        .await;
    assert_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;

    for query in [
        "incarnation_id=not-an-incarnation".to_owned(),
        format!("incarnation_id={INCARNATION}&limit=101"),
        format!("incarnation_id={INCARNATION}&limit=0"),
        format!("incarnation_id={INCARNATION}&after_id=not-an-id"),
        format!("incarnation_id={INCARNATION}&unknown=true"),
    ] {
        let response = fixture
            .get(&format!(
                "/v1/scan/node/{{node_id}}/session/{}/reconciliation?{query}",
                session.id.0
            ))
            .await;
        assert_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;
    }

    for query in [
        "incarnation_id=not-an-incarnation",
        "incarnation_id=0123456789abcdef0123456789abcdef&unknown=true",
    ] {
        let response = fixture
            .get(&format!(
                "/v1/scan/node/{{node_id}}/session/{}?{query}",
                session.id.0
            ))
            .await;
        assert_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;
    }
}

#[tokio::test]
async fn every_scan_route_maps_malformed_paths_to_bad_args() {
    let fixture = scan_fixture().await;
    let session = fixture.request_session().await;
    for (index, mut case) in scan_route_cases(session.id.0, INCARNATION)
        .into_iter()
        .enumerate()
    {
        case.path = case.path.replace("{node_id}", "not-a-node");
        let command = case.command;
        let response = request_route_case(
            &fixture,
            case,
            fixture.node_id,
            &fixture.token,
            &format!("malformed-path-{index}"),
        )
        .await;
        assert_route_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS", command).await;
    }
}

#[tokio::test]
async fn scan_batch_accepts_only_literal_iso8601_timestamp_strings() {
    let fixture = scan_fixture().await;
    let accepted = fixture.request_session().await;
    start_session(&fixture, accepted.id.0, "iso-start").await;
    let accepted_response = fixture
        .post(
            &format!(
                "/v1/scan/node/{{node_id}}/session/{}/batch/0",
                accepted.id.0
            ),
            "iso-batch",
            json!({
                "incarnation_id": INCARNATION,
                "observations": [wire_observation("literal-iso.mkv", "literal-iso")]
            }),
        )
        .await;
    assert_eq!(accepted_response.status(), StatusCode::OK);

    let fixture = scan_fixture().await;
    let rejected = fixture.request_session().await;
    start_session(&fixture, rejected.id.0, "tuple-start").await;
    let tuple_response = fixture
        .post(
            &format!(
                "/v1/scan/node/{{node_id}}/session/{}/batch/0",
                rejected.id.0
            ),
            "tuple-batch",
            json!({
                "incarnation_id": INCARNATION,
                "observations": [observation("tuple.mkv", "tuple")]
            }),
        )
        .await;
    assert_error(tuple_response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;
}

#[tokio::test]
async fn scan_batch_client_bounds_use_bad_args_envelopes() {
    let fixture = scan_fixture().await;
    let session = fixture.request_session().await;
    let path = format!("/v1/scan/node/{{node_id}}/session/{}/batch/0", session.id.0);
    let base = wire_observation("bounded.mkv", "bounded-object");
    let mut oversized_identity = base.clone();
    oversized_identity["provider_object_identity"] = json!("x".repeat(4097));
    let mut nul_identity = base.clone();
    nul_identity["provider_object_identity"] = json!("bad\0identity");
    let mut oversized_size = base.clone();
    oversized_size["size_bytes"] = json!(u64::MAX);
    let mut invalid_locator = base.clone();
    invalid_locator["provider_relative_locator"] = json!("../escape.mkv");
    let mut oversized_locator = base.clone();
    oversized_locator["provider_relative_locator"] = json!("x".repeat(4_097));
    let mut malformed_time = base.clone();
    malformed_time["modified_at"] = json!("not-iso8601");
    let mut unknown_observation_field = base.clone();
    unknown_observation_field["unknown"] = json!(true);
    let mut reversed = wire_observation("reversed.mkv", "reversed");
    reversed["stability_started_at"] = json!("1970-01-01T00:00:01Z");
    let cases = vec![
        Vec::new(),
        vec![base.clone(); 1_001],
        vec![wire_observation("empty-identity.mkv", "")],
        vec![oversized_identity],
        vec![nul_identity],
        vec![oversized_size],
        vec![invalid_locator],
        vec![oversized_locator],
        vec![malformed_time],
        vec![unknown_observation_field],
        vec![reversed],
    ];
    for (index, observations) in cases.into_iter().enumerate() {
        let response = fixture
            .post(
                &path,
                &format!("bad-batch-bound-{index}"),
                json!({"incarnation_id": INCARNATION, "observations": observations}),
            )
            .await;
        assert_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;
    }
}

#[tokio::test]
async fn scan_completion_client_bounds_use_bad_args_envelopes() {
    let fixture = scan_fixture().await;
    let session = fixture.request_session().await;
    let path = format!(
        "/v1/scan/node/{{node_id}}/session/{}/complete",
        session.id.0
    );
    for (index, body) in [
        json!({
            "incarnation_id": INCARNATION,
            "last_sequence": u64::MAX,
            "observation_count": 0
        }),
        json!({
            "incarnation_id": INCARNATION,
            "last_sequence": null,
            "observation_count": u64::MAX
        }),
    ]
    .into_iter()
    .enumerate()
    {
        let response = fixture
            .post(&path, &format!("bad-complete-bound-{index}"), body)
            .await;
        assert_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;
    }
}

#[tokio::test]
async fn scan_routes_keep_runtime_authentication_generic_and_map_session_errors() {
    let fixture = scan_fixture().await;
    let unknown = 999_999;
    let start = format!("/v1/scan/node/{{node_id}}/session/{unknown}/start");
    let response = fixture
        .post_as(
            &start,
            "invalid-token",
            fixture.node_id,
            "not-the-token",
            json!({"incarnation_id": INCARNATION}),
        )
        .await;
    assert_generic_unauthorized(response, unknown).await;

    let inspect =
        format!("/v1/scan/node/{{node_id}}/session/{unknown}?incarnation_id={INCARNATION}");
    let response = fixture
        .get_as(&inspect, fixture.node_id, "not-the-token")
        .await;
    assert_generic_unauthorized(response, unknown).await;

    let response = fixture
        .get(&format!(
            "/v1/scan/node/{{node_id}}/session/{unknown}?incarnation_id={INCARNATION}"
        ))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "NOT_FOUND").await;

    let session = fixture.request_session().await;
    let response = fixture
        .get_as(
            &format!(
                "/v1/scan/node/{{node_id}}/session/{}?incarnation_id={}",
                session.id.0, fixture.other_incarnation_id
            ),
            fixture.other_node_id,
            &fixture.other_token,
        )
        .await;
    assert_error(response, StatusCode::CONFLICT, "CONFLICT").await;

    let response = fixture
        .get(&format!(
            "/v1/scan/node/{{node_id}}/session/{}?incarnation_id={OTHER_INCARNATION}",
            session.id.0
        ))
        .await;
    assert_error(response, StatusCode::CONFLICT, "CONFLICT").await;
}

#[tokio::test]
async fn all_scan_routes_pin_runtime_auth_and_session_error_envelopes() {
    let fixture = scan_fixture().await;
    let unknown = 999_999;
    for (index, case) in scan_route_cases(unknown, INCARNATION)
        .into_iter()
        .enumerate()
    {
        let command = case.command;
        let response = request_route_case(
            &fixture,
            case,
            fixture.node_id,
            "invalid-token-private",
            &format!("invalid-token-{index}"),
        )
        .await;
        assert_generic_unauthorized_with_secrets(
            response,
            unknown,
            &["invalid-token-private", "private-locator", "private-object"],
            Some(command),
        )
        .await;
    }

    for (index, case) in scan_route_cases(unknown, INCARNATION)
        .into_iter()
        .enumerate()
    {
        let command = case.command;
        let response = request_route_case(
            &fixture,
            case,
            fixture.node_id,
            &fixture.token,
            &format!("missing-session-{index}"),
        )
        .await;
        assert_route_error(response, StatusCode::NOT_FOUND, "NOT_FOUND", command).await;
    }

    let session = fixture.request_session().await;
    for (index, case) in scan_route_cases(session.id.0, OTHER_INCARNATION)
        .into_iter()
        .enumerate()
    {
        let command = case.command;
        let response = request_route_case(
            &fixture,
            case,
            fixture.other_node_id,
            &fixture.other_token,
            &format!("non-owner-{index}"),
        )
        .await;
        assert_route_error(response, StatusCode::CONFLICT, "CONFLICT", command).await;
    }

    for (index, case) in scan_route_cases(session.id.0, OTHER_INCARNATION)
        .into_iter()
        .enumerate()
    {
        let command = case.command;
        let response = request_route_case(
            &fixture,
            case,
            fixture.node_id,
            &fixture.token,
            &format!("stale-incarnation-{index}"),
        )
        .await;
        assert_route_error(response, StatusCode::CONFLICT, "CONFLICT", command).await;
    }
}

#[tokio::test]
async fn scan_mutations_replay_exactly_and_inspection_omits_private_observation_facts() {
    let fixture = scan_fixture().await;
    let observed =
        seed_rooted_location(&fixture.pool, fixture.root_id, "observed-private.mkv").await;
    let absent = seed_rooted_location(&fixture.pool, fixture.root_id, "absent-private.mkv").await;
    let session = fixture.request_session().await;
    let start_path = format!("/v1/scan/node/{{node_id}}/session/{}/start", session.id.0);
    let start_body = json!({"incarnation_id": INCARNATION});
    let first = assert_post_replay(&fixture, &start_path, "start-replay", start_body).await;
    assert!(first["data"]["progress_deadline_at"].is_string());
    assert!(!first.to_string().contains(&fixture.token));
    assert!(!first.to_string().contains("start-replay"));

    let observation = wire_observation("observed-private.mkv", "object-identity-private");
    let batch_body = json!({
        "incarnation_id": INCARNATION,
        "observations": [observation]
    });
    let batch_path = format!("/v1/scan/node/{{node_id}}/session/{}/batch/0", session.id.0);
    let first = assert_post_replay(&fixture, &batch_path, "batch-replay", batch_body).await;
    let stored_hash: String = sqlx::query_scalar(
        "SELECT request_hash FROM scan_observation_batches WHERE scan_session_id = ? AND sequence = 0",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert!(!first.to_string().contains(&stored_hash));
    assert!(!first.to_string().contains("batch-replay"));

    let inspect = fixture
        .get(&format!(
            "/v1/scan/node/{{node_id}}/session/{}?incarnation_id={INCARNATION}",
            session.id.0
        ))
        .await;
    assert_eq!(inspect.status(), StatusCode::OK);
    let inspect = response_body(inspect).await;
    assert_eq!(inspect["data"]["next_sequence"], 1);
    assert_eq!(inspect["data"]["observation_count"], 1);
    assert_private_facts_absent(&inspect);

    let complete_path = format!(
        "/v1/scan/node/{{node_id}}/session/{}/complete",
        session.id.0
    );
    let complete_body = json!({
        "incarnation_id": INCARNATION,
        "last_sequence": 0,
        "observation_count": 1
    });
    let first =
        assert_post_replay(&fixture, &complete_path, "complete-replay", complete_body).await;
    assert_eq!(first["data"]["retired_location_count"], 1);
    assert!(!first.to_string().contains("complete-replay"));

    let reconciliation = fixture
        .get(&format!(
            "/v1/scan/node/{{node_id}}/session/{}/reconciliation?incarnation_id={INCARNATION}",
            session.id.0
        ))
        .await;
    assert_eq!(reconciliation.status(), StatusCode::OK);
    let reconciliation = response_body(reconciliation).await;
    assert_eq!(
        reconciliation["data"]["items"][0]["file_location_id"],
        absent
    );
    assert_ne!(
        reconciliation["data"]["items"][0]["file_location_id"],
        observed
    );
    assert_private_facts_absent(&reconciliation);
}

#[tokio::test]
async fn scan_session_capacity_maps_to_http_409_without_leaking_observation_data() {
    let fixture = scan_fixture().await;
    let session = fixture.request_session().await;
    let start = fixture
        .post(
            &format!("/v1/scan/node/{{node_id}}/session/{}/start", session.id.0),
            "capacity-api-start",
            json!({"incarnation_id": INCARNATION}),
        )
        .await;
    assert_eq!(start.status(), StatusCode::OK);
    sqlx::query("DROP TRIGGER IF EXISTS scan_observation_batches_validate_parent_frontier")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "WITH RECURSIVE numbers(value) AS (\
             SELECT 0 UNION ALL SELECT value + 1 FROM numbers WHERE value < 99\
         )\
         INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
             request_hash, observation_count, accepted_at, cumulative_observation_count)\
         SELECT ?, value, CASE WHEN value = 0 THEN NULL ELSE value - 1 END, \
             printf('%064x', value), 1000, '1970-01-01T00:00:00Z', (value + 1) * 1000 \
         FROM numbers ORDER BY value ASC",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "WITH RECURSIVE numbers(value) AS (\
             SELECT 0 UNION ALL SELECT value + 1 FROM numbers WHERE value < 99999\
         )\
         INSERT INTO scan_observations (scan_session_id, batch_sequence, ordinal, \
             provider_relative_locator, provider_object_identity, size_bytes, modified_at, \
             stability_started_at, stability_confirmed_at)\
         SELECT ?, value / 1000, value % 1000, 'capacity/' || value, 'object-' || value, 1, \
             '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
             '1970-01-01T00:00:00Z' FROM numbers ORDER BY value ASC",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = 100, batch_count = 100, \
         observation_count = 100000 WHERE id = ?",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let private_locator = "capacity/private-name.mkv";
    let response = fixture
        .post(
            &format!(
                "/v1/scan/node/{{node_id}}/session/{}/batch/100",
                session.id.0
            ),
            "capacity-api-crossing",
            json!({
                "incarnation_id": INCARNATION,
                "observations": [wire_observation(private_locator, "private-object-identity")]
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = response_body(response).await;
    assert_eq!(body["error"]["code"], "CONFLICT");
    let text = body.to_string();
    assert!(text.contains("maximum 100000"));
    assert!(text.contains("current 100000"));
    assert!(text.contains("incoming 1"));
    assert!(!text.contains(private_locator));
    assert!(!text.contains("private-object-identity"));
}

#[tokio::test]
async fn scan_failure_replays_and_hashes_the_full_body() {
    let fixture = scan_fixture().await;
    let failed = fixture.request_session().await;
    let failed_start = format!("/v1/scan/node/{{node_id}}/session/{}/start", failed.id.0);
    let started = fixture
        .post(
            &failed_start,
            "start-failed-session",
            json!({"incarnation_id": INCARNATION}),
        )
        .await;
    assert_eq!(started.status(), StatusCode::OK);
    let fail_path = format!("/v1/scan/node/{{node_id}}/session/{}/fail", failed.id.0);
    let fail_body = json!({"incarnation_id": INCARNATION, "reason": "scanner failed"});
    let first = assert_post_replay(&fixture, &fail_path, "fail-replay", fail_body).await;
    assert!(first["data"]["terminal_at"].is_string());
    assert_eq!(first["data"]["terminal_reason"], "scanner failed");
    assert!(!first.to_string().contains(&fixture.token));
    assert!(!first.to_string().contains("fail-replay"));
    let conflict = fixture
        .post(
            &fail_path,
            "fail-replay",
            json!({"incarnation_id": INCARNATION, "reason": "different failure"}),
        )
        .await;
    assert_error(conflict, StatusCode::CONFLICT, "CONFLICT").await;
}

#[tokio::test]
async fn scan_reconciliation_uses_default_maximum_and_exclusive_cursor_bounds() {
    let fixture = scan_fixture().await;
    for index in 0..51 {
        seed_rooted_location(
            &fixture.pool,
            fixture.root_id,
            &format!("pagination/private-{index:03}.mkv"),
        )
        .await;
    }
    let session = fixture.request_session().await;
    let start = fixture
        .post(
            &format!("/v1/scan/node/{{node_id}}/session/{}/start", session.id.0),
            "pagination-start",
            json!({"incarnation_id": INCARNATION}),
        )
        .await;
    assert_eq!(start.status(), StatusCode::OK);
    let complete = fixture
        .post(
            &format!(
                "/v1/scan/node/{{node_id}}/session/{}/complete",
                session.id.0
            ),
            "pagination-complete",
            json!({
                "incarnation_id": INCARNATION,
                "last_sequence": null,
                "observation_count": 0
            }),
        )
        .await;
    assert_eq!(complete.status(), StatusCode::OK);

    let base = format!(
        "/v1/scan/node/{{node_id}}/session/{}/reconciliation?incarnation_id={INCARNATION}",
        session.id.0
    );
    let first = fixture.get(&base).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = response_body(first).await;
    assert_eq!(first["data"]["items"].as_array().unwrap().len(), 50);
    let cursor = first["data"]["next_after_id"].as_u64().unwrap();
    assert_eq!(first["data"]["items"][49]["file_location_id"], cursor);
    assert_private_facts_absent(&first);

    let second = fixture.get(&format!("{base}&after_id={cursor}")).await;
    assert_eq!(second.status(), StatusCode::OK);
    let second = response_body(second).await;
    assert_eq!(second["data"]["items"].as_array().unwrap().len(), 1);
    assert!(
        second["data"]["items"][0]["file_location_id"]
            .as_u64()
            .unwrap()
            > cursor
    );
    assert!(second["data"]["next_after_id"].is_null());

    let maximum = fixture.get(&format!("{base}&limit=100")).await;
    assert_eq!(maximum.status(), StatusCode::OK);
    assert_eq!(
        response_body(maximum).await["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        51
    );
}

#[tokio::test]
async fn scan_failure_reason_bounds_and_malformed_json_fail_before_mutation() {
    let fixture = scan_fixture().await;
    let session = fixture.request_session().await;
    let fail_path = format!("/v1/scan/node/{{node_id}}/session/{}/fail", session.id.0);
    for (index, reason) in [String::new(), "x".repeat(1025), "bad\0reason".to_owned()]
        .into_iter()
        .enumerate()
    {
        let response = fixture
            .post(
                &fail_path,
                &format!("bad-reason-{index}"),
                json!({"incarnation_id": INCARNATION, "reason": reason}),
            )
            .await;
        assert_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;
    }
    let malformed = fixture.post_raw(&fail_path, "malformed-json", "{").await;
    assert_error(malformed, StatusCode::BAD_REQUEST, "BAD_ARGS").await;
    assert_eq!(
        fixture.cp.scan_session(session.id).await.unwrap().status,
        voom_core::ScanSessionStatus::Requested
    );
}

#[tokio::test]
async fn scan_mutations_share_the_one_mib_server_body_boundary() {
    let fixture = scan_fixture().await;
    let session = fixture.request_session().await;
    let app = bounded_router(fixture.app.clone(), ServerLimits::default());
    for (index, suffix) in ["start", "batch/0", "complete", "fail"]
        .into_iter()
        .enumerate()
    {
        let response = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/v1/scan/node/{}/session/{}/{suffix}",
                    fixture.node_id.0, session.id.0
                ))
                .header(AUTHORIZATION, format!("Bearer {}", fixture.token))
                .header("content-type", "application/json")
                .header("x-voom-idempotency-key", format!("oversized-scan-{index}"))
                .body(Body::from(vec![b'x'; 1024 * 1024 + 1]))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_error(response, StatusCode::PAYLOAD_TOO_LARGE, "PAYLOAD_TOO_LARGE").await;
    }
}

#[tokio::test]
async fn scan_route_body_processing_uses_the_shared_request_deadline_envelope() {
    let fixture = scan_fixture().await;
    let session = fixture.request_session().await;
    let limits = ServerLimits::new_for_test(
        1024 * 1024,
        Duration::from_secs(30),
        Duration::from_secs(30),
        Duration::from_millis(10),
        Duration::from_secs(90),
        Duration::from_secs(30),
    )
    .unwrap();
    let app = bounded_router(fixture.app, limits);
    let response = app
        .oneshot(
            Request::post(format!(
                "/v1/scan/node/{}/session/{}/start",
                fixture.node_id.0, session.id.0
            ))
            .header(AUTHORIZATION, format!("Bearer {}", fixture.token))
            .header("content-type", "application/json")
            .header("x-voom-idempotency-key", "pending-scan")
            .body(Body::new(OneFrameThenPending::new()))
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        response_body(response).await,
        json!({
            "schema_version": "0",
            "command": "api.request",
            "status": "error",
            "data": null,
            "warnings": [],
            "error": {
                "code": "REQUEST_TIMEOUT",
                "message": "request processing exceeded the 30-second deadline",
                "hint": "Retry a mutation with the same idempotency key if its outcome is unknown"
            }
        })
    );
}

#[test]
fn scan_start_hash_binds_method_node_session_and_body() {
    let request = StartRequest {
        incarnation_id: INCARNATION.parse().unwrap(),
    };
    let first = stable_request_hash("POST", "/v1/scan/node/1/session/7/start", &request).unwrap();
    let replay = stable_request_hash("POST", "/v1/scan/node/1/session/7/start", &request).unwrap();
    let other_session =
        stable_request_hash("POST", "/v1/scan/node/1/session/8/start", &request).unwrap();
    assert_eq!(first, replay);
    assert_eq!(
        first,
        "34d3a59fbad336d532afb240f1ebb20e71080957745765346b1de0dbc2490409"
    );
    assert_ne!(first, other_session);

    let other_node =
        stable_request_hash("POST", "/v1/scan/node/2/session/7/start", &request).unwrap();
    let other_method =
        stable_request_hash("PUT", "/v1/scan/node/1/session/7/start", &request).unwrap();
    let other_body = StartRequest {
        incarnation_id: OTHER_INCARNATION.parse().unwrap(),
    };
    let other_body =
        stable_request_hash("POST", "/v1/scan/node/1/session/7/start", &other_body).unwrap();
    assert_ne!(first, other_node);
    assert_ne!(first, other_method);
    assert_ne!(first, other_body);
}

#[test]
fn scan_fail_hash_binds_node_session_and_every_body_field() {
    let failure_a = FailRequest {
        incarnation_id: INCARNATION.parse().unwrap(),
        reason: "failure a".to_owned(),
    };
    let failure_b = FailRequest {
        incarnation_id: INCARNATION.parse().unwrap(),
        reason: "failure b".to_owned(),
    };
    let first = stable_request_hash("POST", "/v1/scan/node/1/session/7/fail", &failure_a).unwrap();
    let other_body =
        stable_request_hash("POST", "/v1/scan/node/1/session/7/fail", &failure_b).unwrap();
    let other_incarnation = FailRequest {
        incarnation_id: OTHER_INCARNATION.parse().unwrap(),
        reason: "failure a".to_owned(),
    };
    let other_incarnation =
        stable_request_hash("POST", "/v1/scan/node/1/session/7/fail", &other_incarnation).unwrap();
    let other_node =
        stable_request_hash("POST", "/v1/scan/node/2/session/7/fail", &failure_a).unwrap();
    let other_session =
        stable_request_hash("POST", "/v1/scan/node/1/session/8/fail", &failure_a).unwrap();
    assert_ne!(first, other_body);
    assert_ne!(first, other_incarnation);
    assert_ne!(first, other_node);
    assert_ne!(first, other_session);
}

#[test]
fn scan_complete_hash_binds_node_session_and_every_body_field() {
    let request = CompleteRequest {
        incarnation_id: INCARNATION.parse().unwrap(),
        last_sequence: Some(3),
        observation_count: 4,
    };
    let first =
        stable_request_hash("POST", "/v1/scan/node/1/session/7/complete", &request).unwrap();
    let cases = [
        stable_request_hash("POST", "/v1/scan/node/2/session/7/complete", &request).unwrap(),
        stable_request_hash("POST", "/v1/scan/node/1/session/8/complete", &request).unwrap(),
        stable_request_hash(
            "POST",
            "/v1/scan/node/1/session/7/complete",
            &CompleteRequest {
                incarnation_id: OTHER_INCARNATION.parse().unwrap(),
                last_sequence: Some(3),
                observation_count: 4,
            },
        )
        .unwrap(),
        stable_request_hash(
            "POST",
            "/v1/scan/node/1/session/7/complete",
            &CompleteRequest {
                incarnation_id: INCARNATION.parse().unwrap(),
                last_sequence: None,
                observation_count: 4,
            },
        )
        .unwrap(),
        stable_request_hash(
            "POST",
            "/v1/scan/node/1/session/7/complete",
            &CompleteRequest {
                incarnation_id: INCARNATION.parse().unwrap(),
                last_sequence: Some(3),
                observation_count: 5,
            },
        )
        .unwrap(),
    ];
    assert!(cases.into_iter().all(|hash| hash != first));
}

#[test]
fn scan_batch_hash_binds_node_session_sequence_and_every_body_field() {
    let request = batch_hash_request(INCARNATION, "bound.mkv", "object", 1, 0, 0, 0);
    let first = stable_request_hash("POST", "/v1/scan/node/1/session/7/batch/3", &request).unwrap();
    let routes = [
        "/v1/scan/node/2/session/7/batch/3",
        "/v1/scan/node/1/session/8/batch/3",
        "/v1/scan/node/1/session/7/batch/4",
    ];
    assert!(
        routes
            .into_iter()
            .all(|route| { stable_request_hash("POST", route, &request).unwrap() != first })
    );
    for changed in [
        batch_hash_request(OTHER_INCARNATION, "bound.mkv", "object", 1, 0, 0, 0),
        batch_hash_request(INCARNATION, "other.mkv", "object", 1, 0, 0, 0),
        batch_hash_request(INCARNATION, "bound.mkv", "other", 1, 0, 0, 0),
        batch_hash_request(INCARNATION, "bound.mkv", "object", 2, 0, 0, 0),
        batch_hash_request(INCARNATION, "bound.mkv", "object", 1, 1, 0, 0),
        batch_hash_request(INCARNATION, "bound.mkv", "object", 1, 0, 1, 0),
        batch_hash_request(INCARNATION, "bound.mkv", "object", 1, 0, 0, 1),
    ] {
        assert_ne!(
            stable_request_hash("POST", "/v1/scan/node/1/session/7/batch/3", &changed).unwrap(),
            first
        );
    }
}

fn batch_hash_request(
    incarnation_id: &str,
    locator: &str,
    identity: &str,
    size_bytes: u64,
    modified_seconds: i64,
    started_seconds: i64,
    confirmed_seconds: i64,
) -> BatchRequest {
    BatchRequest {
        incarnation_id: incarnation_id.parse().unwrap(),
        observations: vec![ScanObservationRequest {
            provider_relative_locator: ProviderRelativeLocator::new(locator.to_owned()).unwrap(),
            provider_object_identity: identity.to_owned(),
            size_bytes,
            modified_at: iso_second(modified_seconds),
            stability_started_at: iso_second(started_seconds),
            stability_confirmed_at: iso_second(confirmed_seconds),
            evidence: None,
        }],
    }
}

fn iso_second(seconds: i64) -> String {
    voom_core::format_iso8601(time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(seconds))
}

async fn unconfigured_app() -> axum::Router {
    let database = TempDatabase::new().unwrap();
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    router(HealthPlane::open(&url).await.unwrap())
}

async fn response_body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn assert_error(response: Response<Body>, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    let body = response_body(response).await;
    assert_eq!(body["schema_version"], "0");
    assert_eq!(body["status"], "error");
    assert!(body["data"].is_null());
    assert_eq!(body["warnings"], json!([]));
    assert_eq!(body["error"]["code"], code);
    assert!(body["error"]["message"].is_string());
}

async fn assert_route_error(
    response: Response<Body>,
    status: StatusCode,
    code: &str,
    command: &str,
) {
    assert_eq!(response.status(), status);
    let body = response_body(response).await;
    assert_eq!(body["schema_version"], "0");
    assert_eq!(body["command"], command);
    assert_eq!(body["status"], "error");
    assert!(body["data"].is_null());
    assert_eq!(body["warnings"], json!([]));
    assert_eq!(body["error"]["code"], code);
    assert!(body["error"]["message"].is_string());
}

async fn assert_generic_unauthorized(response: Response<Body>, hidden_id: u64) {
    assert_generic_unauthorized_with_secrets(response, hidden_id, &[], None).await;
}

async fn assert_generic_unauthorized_with_secrets(
    response: Response<Body>,
    hidden_id: u64,
    secrets: &[&str],
    command: Option<&str>,
) {
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(WWW_AUTHENTICATE),
        Some(&HeaderValue::from_static("Bearer realm=\"voom\"")),
    );
    let body = response_body(response).await;
    if let Some(command) = command {
        assert_eq!(body["command"], command);
    }
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    assert_eq!(
        body["error"]["message"],
        "unauthorized: remote node authentication failed"
    );
    let text = body.to_string();
    assert!(!text.contains(&hidden_id.to_string()));
    for secret in secrets {
        assert!(!text.contains(secret));
    }
}

fn scan_route_cases(session_id: u64, incarnation_id: &str) -> Vec<ScanRouteCase> {
    vec![
        mutation_case(
            format!("/v1/scan/node/{{node_id}}/session/{session_id}/start"),
            json!({"incarnation_id": incarnation_id}),
            "scan.start",
        ),
        mutation_case(
            format!("/v1/scan/node/{{node_id}}/session/{session_id}/batch/0"),
            json!({
                "incarnation_id": incarnation_id,
                "observations": [wire_observation("private-locator.mkv", "private-object")]
            }),
            "scan.batch",
        ),
        mutation_case(
            format!("/v1/scan/node/{{node_id}}/session/{session_id}/complete"),
            json!({
                "incarnation_id": incarnation_id,
                "last_sequence": null,
                "observation_count": 0
            }),
            "scan.complete",
        ),
        mutation_case(
            format!("/v1/scan/node/{{node_id}}/session/{session_id}/fail"),
            json!({"incarnation_id": incarnation_id, "reason": "private-failure"}),
            "scan.fail",
        ),
        ScanRouteCase {
            method: Method::GET,
            path: format!(
                "/v1/scan/node/{{node_id}}/session/{session_id}?incarnation_id={incarnation_id}"
            ),
            body: None,
            command: "scan.inspect",
        },
        ScanRouteCase {
            method: Method::GET,
            path: format!(
                "/v1/scan/node/{{node_id}}/session/{session_id}/reconciliation?incarnation_id={incarnation_id}"
            ),
            body: None,
            command: "scan.reconciliation",
        },
    ]
}

fn mutation_case(path: String, body: Value, command: &'static str) -> ScanRouteCase {
    ScanRouteCase {
        method: Method::POST,
        path,
        body: Some(body),
        command,
    }
}

async fn request_route_case(
    fixture: &ScanApiFixture,
    case: ScanRouteCase,
    node_id: NodeId,
    token: &str,
    key: &str,
) -> Response<Body> {
    let path = case.path.replace("{node_id}", &node_id.0.to_string());
    let mut request = Request::builder()
        .method(&case.method)
        .uri(path)
        .header(AUTHORIZATION, format!("Bearer {token}"));
    if case.method == Method::POST {
        request = request
            .header("content-type", "application/json")
            .header("x-voom-idempotency-key", key);
    }
    fixture
        .app
        .clone()
        .oneshot(
            request
                .body(
                    case.body
                        .map_or_else(Body::empty, |body| Body::from(body.to_string())),
                )
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn scan_fixture() -> ScanApiFixture {
    let database = TempDatabase::new().unwrap();
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();
    let incarnation_id = INCARNATION.parse().unwrap();
    let (node_id, token) = activate_node(&cp, "scan-api-owner", incarnation_id).await;
    let other_incarnation_id = OTHER_INCARNATION.parse().unwrap();
    let (other_node_id, other_token) =
        activate_node(&cp, "scan-api-other", other_incarnation_id).await;
    let root_id = create_root(&cp, node_id).await;
    let health = HealthPlane::open(&url).await.unwrap();
    let app = router_with_control_plane(health, cp.clone());
    ScanApiFixture {
        _database: database,
        pool,
        app,
        cp,
        node_id,
        token,
        incarnation_id,
        other_node_id,
        other_token,
        other_incarnation_id,
        root_id,
    }
}

async fn activate_node(
    cp: &ControlPlane,
    name: &str,
    incarnation_id: NodeIncarnationId,
) -> (NodeId, String) {
    let registered = cp
        .register_node(RegisterNodeInput {
            name: name.to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 600,
            metadata: json!({}),
        })
        .await
        .unwrap();
    cp.remote_activate(RemoteActivateInput {
        node_id: registered.node.id,
        token: registered.token.clone(),
        idempotency_key: format!("activate-{name}"),
        request_hash: format!("activate-{name}-body"),
        incarnation_id,
        workers: vec![RemoteWorkerDeclaration {
            logical_name: "scan".to_owned(),
            operations: vec![OperationKind::ProbeFile],
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            max_parallel: 1,
        }],
    })
    .await
    .unwrap();
    (
        registered.node.id,
        registered.token.expose_secret().to_owned(),
    )
}

async fn create_root(cp: &ControlPlane, owner_node_id: NodeId) -> StorageRootId {
    let library = cp
        .create_library(NewLibrary {
            slug: "scan-api".to_owned(),
            display_name: "Scan API".to_owned(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = cp
        .create_library_root(new_root(library.id, owner_node_id))
        .await
        .unwrap();
    cp.activate_library_root(root.id, "scan-api-root".to_owned())
        .await
        .unwrap();
    root.id
}

fn new_root(library_id: LibraryId, owner_node_id: NodeId) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new("/scan-api".to_owned()).unwrap(),
        display_locator: "/scan-api".to_owned(),
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
    }
}

fn observation(locator: &str, object_identity: &str) -> Value {
    serde_json::to_value(voom_control_plane::scan::ScanObservation {
        provider_relative_locator: ProviderRelativeLocator::new(locator.to_owned()).unwrap(),
        provider_object_identity: object_identity.to_owned(),
        size_bytes: 1,
        modified_at: time::OffsetDateTime::UNIX_EPOCH,
        stability_started_at: time::OffsetDateTime::UNIX_EPOCH,
        stability_confirmed_at: time::OffsetDateTime::UNIX_EPOCH,
        evidence: None,
    })
    .unwrap()
}

fn wire_observation(locator: &str, object_identity: &str) -> Value {
    json!({
        "provider_relative_locator": locator,
        "provider_object_identity": object_identity,
        "size_bytes": 1,
        "modified_at": "1970-01-01T00:00:00Z",
        "stability_started_at": "1970-01-01T00:00:00Z",
        "stability_confirmed_at": "1970-01-01T00:00:00Z"
    })
}

async fn start_session(fixture: &ScanApiFixture, session_id: u64, key: &str) {
    let response = fixture
        .post(
            &format!("/v1/scan/node/{{node_id}}/session/{session_id}/start"),
            key,
            json!({"incarnation_id": INCARNATION}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

async fn seed_rooted_location(
    pool: &sqlx::SqlitePool,
    root_id: StorageRootId,
    locator: &str,
) -> u64 {
    let asset = sqlx::query("INSERT INTO file_assets (created_at, epoch) VALUES (?, 0)")
        .bind("1970-01-01T00:00:00Z")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let version = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at, retired_at, epoch) \
         VALUES (?, ?, 1, 'ingest', NULL, ?, NULL, 0)",
    )
    .bind(asset)
    .bind(format!("scan-api-hash-{asset}"))
    .bind("1970-01-01T00:00:00Z")
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    u64::try_from(
        sqlx::query(
            "INSERT INTO file_locations (file_version_id, address_state, storage_root_id, \
             provider_relative_locator, observed_at, epoch) \
             VALUES (?, 'rooted', ?, ?, ?, 0)",
        )
        .bind(version)
        .bind(i64::try_from(root_id.0).unwrap())
        .bind(locator)
        .bind("1970-01-01T00:00:00Z")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid(),
    )
    .unwrap()
}

async fn table_count(pool: &sqlx::SqlitePool, table: &str) -> i64 {
    let sql = match table {
        "events" => "SELECT COUNT(*) FROM events",
        "scan_observations" => "SELECT COUNT(*) FROM scan_observations",
        _ => unreachable!("test requested an unsupported table"),
    };
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

async fn assert_post_replay(fixture: &ScanApiFixture, path: &str, key: &str, body: Value) -> Value {
    let first = fixture.post(path, key, body.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = response_body(first).await;
    let observations = table_count(&fixture.pool, "scan_observations").await;
    let events = table_count(&fixture.pool, "events").await;
    let replay = fixture.post(path, key, body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(response_body(replay).await, first);
    assert_eq!(
        table_count(&fixture.pool, "scan_observations").await,
        observations
    );
    assert_eq!(table_count(&fixture.pool, "events").await, events);
    first
}

fn assert_private_facts_absent(body: &Value) {
    let text = body.to_string();
    assert!(!text.contains("object-identity-private"));
    assert!(!text.contains("observed-private.mkv"));
    assert!(!text.contains("absent-private.mkv"));
    assert!(!text.contains("pagination/private"));
    assert!(!text.contains("provider_object_identity"));
    assert!(!text.contains("provider_relative_locator"));
}

struct OneFrameThenPending {
    yielded: bool,
}

impl OneFrameThenPending {
    const fn new() -> Self {
        Self { yielded: false }
    }
}

impl HttpBody for OneFrameThenPending {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.yielded {
            Poll::Pending
        } else {
            self.yielded = true;
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"{")))))
        }
    }
}
