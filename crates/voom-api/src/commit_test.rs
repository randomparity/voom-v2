#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "route tests use unwrap and panic for fallible fixture construction and assertions"
)]

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use secrecy::ExposeSecret;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use tower::ServiceExt;
use voom_control_plane::artifact_commit::{
    CommitArtifactCommandError, CommitArtifactInput, CommitArtifactReport,
};
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::ArtifactHandleId;
use voom_core::ids::ArtifactCommitIntentId;
use voom_store::repo::media::artifacts::ArtifactCommitState;
use voom_store::repo::media::identity::{DiscoveredFile, IngestOutcome};
use voom_store::test_support::{TEST_STORAGE_ROOT_ID, sqlite_url_for, test_relative_locator};
use voom_test_support::TempDatabase;
use voom_test_support::commit_node::SimulatedOwnerNode;

use super::{
    APPLYING_COMMAND, AUTHORIZE_COMMAND, ApplyingRequest, AuthorizeRequest, COMPLETE_COMMAND,
    CompleteRequest, OPEN_COMMAND, OUTCOME_COMMAND, OpenRequest, OutcomeRequest,
    stable_request_hash,
};
use crate::{router, router_with_control_plane};

const INCARNATION: &str = "0123456789abcdef0123456789abcdef";

struct CommitApiFixture {
    _database: TempDatabase,
    pool: sqlx::SqlitePool,
    app: axum::Router,
    cp: ControlPlane,
    dir: tempfile::TempDir,
    node: SimulatedOwnerNode,
}

async fn commit_fixture() -> CommitApiFixture {
    let database = TempDatabase::new().unwrap();
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    // The shared active root (owned by node 9000001) that commit scope
    // resolution needs; `ControlPlane::open` seeds it only inside its own
    // crate's tests.
    voom_store::test_support::seed_test_storage_root(&pool)
        .await
        .unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();
    let cp = cp.with_local_node_id(Some(voom_core::NodeId(9_000_001)));
    let node = SimulatedOwnerNode::new().unwrap();
    node.install(&pool).await.unwrap();
    let health = HealthPlane::open(&url).await.unwrap();
    let app = router_with_control_plane(health, cp.clone());
    let dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
    CommitApiFixture {
        _database: database,
        pool,
        app,
        cp,
        dir,
        node,
    }
}

impl CommitApiFixture {
    fn incarnation(&self) -> String {
        self.node.incarnation_id.to_string()
    }

    fn node_id(&self) -> u64 {
        self.node.node_id.0
    }

    async fn post(&self, path: &str, key: &str, body: Value) -> Response<Body> {
        self.post_raw(
            path,
            Some(self.node.token.expose_secret()),
            Some(key),
            &body.to_string(),
        )
        .await
    }

    async fn post_raw(
        &self,
        path: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: &str,
    ) -> Response<Body> {
        let mut builder = Request::post(path).header(CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(key) = key {
            builder = builder.header("x-voom-idempotency-key", key);
        }
        self.app
            .clone()
            .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap()
    }
}

/// Resolve a pinned rooted address to a real filesystem path via the
/// storage root's configured locator (the node agent does the same).
async fn rooted_path(fixture: &CommitApiFixture, storage_root_id: u64, locator: &str) -> PathBuf {
    let root_locator: String =
        sqlx::query_scalar("SELECT provider_locator FROM library_roots WHERE id = ?")
            .bind(i64::try_from(storage_root_id).unwrap())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    std::path::Path::new(&root_locator).join(locator)
}

async fn response_body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn assert_route_error(response: Response<Body>, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    match (code, response.headers().get(WWW_AUTHENTICATE)) {
        ("UNAUTHORIZED", Some(value)) => {
            assert_eq!(value, "Bearer realm=\"voom\"");
        }
        ("UNAUTHORIZED", None) => {
            panic!("401 must carry WWW-Authenticate");
        }
        (_, extra) => {
            assert!(
                extra.is_none(),
                "non-401 must not carry WWW-Authenticate: {extra:?}"
            );
        }
    }
    let body = response_body(response).await;
    assert_eq!(body["schema_version"], "0");
    assert_eq!(body["status"], "error");
    assert!(body["data"].is_null());
    assert_eq!(body["warnings"], json!([]));
    assert_eq!(body["error"]["code"], code);
    assert!(body["error"]["message"].is_string());
}

fn identity_body(node_id: u64) -> Value {
    json!({"node_id": node_id, "incarnation_id": INCARNATION})
}

struct VerifiedStaging {
    artifact_handle_id: ArtifactHandleId,
}

/// Seed a staged artifact whose bytes carry a successful verification row so
/// `commit_artifact` can prepare a fenced intent. The bundled verify worker
/// binary is not available next to the API test binary, so the staging rows
/// and the verification row are persisted directly with matching facts.
async fn seed_verified_staging(fixture: &CommitApiFixture, bytes: &[u8]) -> VerifiedStaging {
    let source = fixture.dir.path().join("source.bin");
    std::fs::write(&source, bytes).unwrap();
    let outcome = fixture
        .cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: TEST_STORAGE_ROOT_ID,
                provider_relative_locator: test_relative_locator(&source.display().to_string()),
                content_hash: format!("blake3:{}", blake3::hash(bytes).to_hex()),
                size_bytes: bytes.len() as u64,
                observed_at: time::OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = outcome
    else {
        panic!("seeded source should create a new file asset");
    };
    let staging_path = fixture.dir.path().join("staged.bin");
    std::fs::write(&staging_path, bytes).unwrap();
    let staged = voom_test_support::staging_seed::seed_staged_artifact(
        &fixture.pool,
        file_version_id,
        &staging_path,
    )
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO workers (name, kind, status, registered_at, last_seen_at, node_id) \
         VALUES ('api-commit-verify-worker', 'local', 'active', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', 9000001)",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let worker_id: i64 =
        sqlx::query_scalar("SELECT id FROM workers WHERE name = 'api-commit-verify-worker'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO artifact_verifications \
         (artifact_handle_id, artifact_location_id, path, worker_id, status, \
          expected_size_bytes, expected_checksum, observed_size_bytes, observed_checksum, \
          report, started_at, finished_at) \
         VALUES (?, ?, ?, ?, 'succeeded', ?, ?, ?, ?, '{}', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(i64::try_from(staged.artifact_handle_id.0).unwrap())
    .bind(i64::try_from(staged.artifact_location_id.0).unwrap())
    .bind(staging_path.display().to_string())
    .bind(worker_id)
    .bind(i64::try_from(staged.size_bytes).unwrap())
    .bind(&staged.checksum)
    .bind(i64::try_from(staged.size_bytes).unwrap())
    .bind(&staged.checksum)
    .execute(&fixture.pool)
    .await
    .unwrap();

    VerifiedStaging {
        artifact_handle_id: staged.artifact_handle_id,
    }
}

fn spawn_commit_task(
    fixture: &CommitApiFixture,
    handle: ArtifactHandleId,
    target: PathBuf,
) -> tokio::task::JoinHandle<Result<CommitArtifactReport, CommitArtifactCommandError>> {
    let cp = fixture.cp.clone();
    tokio::spawn(async move {
        cp.commit_artifact(CommitArtifactInput {
            artifact_handle_id: handle,
            target_path: target,
        })
        .await
    })
}

async fn wait_pending_intent_id(
    fixture: &CommitApiFixture,
    handle: ArtifactHandleId,
) -> ArtifactCommitIntentId {
    for _ in 0..200 {
        let pending: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM artifact_commit_intents \
             WHERE artifact_handle_id = ? AND state = 'pending' ORDER BY id DESC LIMIT 1",
        )
        .bind(i64::try_from(handle.0).unwrap())
        .fetch_optional(&fixture.pool)
        .await
        .unwrap();
        if let Some(id) = pending {
            return ArtifactCommitIntentId(u64::try_from(id).unwrap());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no pending commit intent appeared for handle {handle}");
}

#[tokio::test]
async fn commit_intent_routes_are_registered_at_the_approved_paths() {
    let database = TempDatabase::new().unwrap();
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let app = router(HealthPlane::open(&url).await.unwrap());

    for path in [
        "/v1/artifact/commit/open",
        "/v1/artifact/commit/7/authorize",
        "/v1/artifact/commit/7/applying",
        "/v1/artifact/commit/7/outcome",
        "/v1/artifact/commit/7/complete",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn commit_intent_routes_report_not_configured_without_a_control_plane() {
    let database = TempDatabase::new().unwrap();
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let app = router(HealthPlane::open(&url).await.unwrap());

    let cases: Vec<(&'static str, &'static str, Value)> = vec![
        ("/v1/artifact/commit/open", OPEN_COMMAND, identity_body(1)),
        (
            "/v1/artifact/commit/7/authorize",
            AUTHORIZE_COMMAND,
            identity_body(1),
        ),
        (
            "/v1/artifact/commit/7/applying",
            APPLYING_COMMAND,
            identity_body(1),
        ),
        (
            "/v1/artifact/commit/7/outcome",
            OUTCOME_COMMAND,
            json!({
                "node_id": 1,
                "incarnation_id": INCARNATION,
                "evidence": {"kind": "outcome_unknown", "reason": "power lost"}
            }),
        ),
        (
            "/v1/artifact/commit/7/complete",
            COMPLETE_COMMAND,
            json!({
                "node_id": 1,
                "incarnation_id": INCARNATION,
                "fence_hex": "00"
            }),
        ),
    ];
    for (path, expected_command, body) in cases {
        let response = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(CONTENT_TYPE, "application/json")
                    .header(AUTHORIZATION, "Bearer syntactically-valid")
                    .header("x-voom-idempotency-key", "not-configured-key")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        let body = response_body(response).await;
        assert_eq!(body["command"], expected_command, "{path}");
        assert_eq!(body["error"]["code"], "NOT_FOUND", "{path}");
        assert_eq!(
            body["error"]["message"], "remote commit intent routes are not configured",
            "{path}"
        );
    }
}

#[tokio::test]
async fn commit_intent_routes_reject_bad_credentials_and_bad_bodies() {
    let fixture = commit_fixture().await;

    // Missing bearer token.
    let response = fixture
        .post_raw("/v1/artifact/commit/open", None, Some("key"), "{}")
        .await;
    assert_route_error(response, StatusCode::UNAUTHORIZED, "UNAUTHORIZED").await;

    // Empty bearer token.
    let response = fixture
        .post_raw("/v1/artifact/commit/open", Some(""), Some("key"), "{}")
        .await;
    assert_route_error(response, StatusCode::UNAUTHORIZED, "UNAUTHORIZED").await;

    // Missing idempotency key.
    let response = fixture
        .post_raw(
            "/v1/artifact/commit/open",
            Some("syntactically-valid"),
            None,
            &identity_body(1).to_string(),
        )
        .await;
    assert_route_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;

    // Malformed JSON.
    let response = fixture
        .post_raw(
            "/v1/artifact/commit/open",
            Some("syntactically-valid"),
            Some("key"),
            "{not json",
        )
        .await;
    assert_route_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;

    // Unknown field.
    let mut unknown = identity_body(1);
    unknown["surprise"] = json!(true);
    let response = fixture
        .post_raw(
            "/v1/artifact/commit/open",
            Some("syntactically-valid"),
            Some("key"),
            &unknown.to_string(),
        )
        .await;
    assert_route_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;

    // Missing incarnation fence on an intent route.
    let response = fixture
        .post_raw(
            "/v1/artifact/commit/7/authorize",
            Some("syntactically-valid"),
            Some("key"),
            &json!({"node_id": 7}).to_string(),
        )
        .await;
    assert_route_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;

    // Non-numeric intent id in the path.
    let response = fixture
        .post_raw(
            "/v1/artifact/commit/not-a-number/authorize",
            Some("syntactically-valid"),
            Some("key"),
            &identity_body(1).to_string(),
        )
        .await;
    assert_route_error(response, StatusCode::BAD_REQUEST, "BAD_ARGS").await;
}
#[expect(
    clippy::too_many_lines,
    reason = "the replay/conflict/resume drive reads linearly; splitting scatters the protocol"
)]
#[tokio::test]
async fn authorize_replays_identically_then_conflicts_on_a_fresh_key() {
    let fixture = commit_fixture().await;
    let staged = seed_verified_staging(&fixture, b"source bytes").await;
    let driver = spawn_commit_task(
        &fixture,
        staged.artifact_handle_id,
        fixture.dir.path().join("target.bin"),
    );
    let intent_id = wait_pending_intent_id(&fixture, staged.artifact_handle_id).await;
    let path = format!("/v1/artifact/commit/{}/authorize", intent_id.0);
    let body = json!({
        "node_id": fixture.node_id(),
        "incarnation_id": fixture.incarnation()
    });

    let first = fixture.post(&path, "authorize-key", body.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = response_body(first).await;
    assert_eq!(first["schema_version"], "0");
    assert_eq!(first["command"], AUTHORIZE_COMMAND);
    assert_eq!(first["status"], "ok");
    let fence_hex = first["data"]["fence_hex"].as_str().unwrap();
    assert_eq!(fence_hex.len(), 64);
    assert!(
        fence_hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
        "fence must travel as lowercase hex: {fence_hex}"
    );
    assert!(first["data"]["staging_storage_root_id"].is_u64());
    assert!(first["data"]["target_provider_relative_locator"].is_string());

    // Same key replays the stored outcome byte-for-byte.
    let replay = fixture.post(&path, "authorize-key", body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(response_body(replay).await, first);

    // A fresh key against the now-authorized intent is a conflict.
    let conflict = fixture
        .post(
            &path,
            "authorize-other-key",
            json!({
                "node_id": fixture.node_id(),
                "incarnation_id": fixture.incarnation()
            }),
        )
        .await;
    assert_route_error(conflict, StatusCode::CONFLICT, "CONFLICT").await;

    // A duplicate authorize against the live authorized fence is not drift
    // (G2): the intent stays authorized and advertised.
    let listing = response_body(
        fixture
            .post(
                "/v1/artifact/commit/open",
                "open-after-conflict",
                json!({
                    "node_id": fixture.node_id(),
                    "incarnation_id": fixture.incarnation()
                }),
            )
            .await,
    )
    .await;
    let intents = listing["data"]["intents"].as_array().unwrap();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0]["state"], "authorized");

    // Finish the drive so the waiting prepare leg converges cleanly.
    let staged_path = rooted_path(
        &fixture,
        first["data"]["staging_storage_root_id"].as_u64().unwrap(),
        first["data"]["staging_provider_relative_locator"]
            .as_str()
            .unwrap(),
    )
    .await;
    let target = fixture.dir.path().join("target.bin");
    std::fs::copy(&staged_path, &target).unwrap();
    let promoted = std::fs::read(&staged_path).unwrap();

    let applying = fixture
        .post(
            &format!("/v1/artifact/commit/{}/applying", intent_id.0),
            "conflict-applying",
            json!({
                "node_id": fixture.node_id(),
                "incarnation_id": fixture.incarnation()
            }),
        )
        .await;
    assert_eq!(applying.status(), StatusCode::OK);

    let outcome = fixture
        .post(
            &format!("/v1/artifact/commit/{}/outcome", intent_id.0),
            "conflict-outcome",
            json!({
                "node_id": fixture.node_id(),
                "incarnation_id": fixture.incarnation(),
                "evidence": {
                    "kind": "applied",
                    "observed": {
                        "size_bytes": promoted.len(),
                        "content_hash": format!("blake3:{}", blake3::hash(&promoted).to_hex())
                    }
                }
            }),
        )
        .await;
    assert_eq!(outcome.status(), StatusCode::OK);

    let complete = fixture
        .post(
            &format!("/v1/artifact/commit/{}/complete", intent_id.0),
            "conflict-complete",
            json!({
                "node_id": fixture.node_id(),
                "incarnation_id": fixture.incarnation(),
                "fence_hex": fence_hex
            }),
        )
        .await;
    assert_eq!(complete.status(), StatusCode::OK);

    let report = driver.await.unwrap().unwrap();
    assert_eq!(report.state, ArtifactCommitState::Committed);

    // Completed intents leave the open listing.
    let listing = response_body(
        fixture
            .post(
                "/v1/artifact/commit/open",
                "open-after-completion",
                json!({
                    "node_id": fixture.node_id(),
                    "incarnation_id": fixture.incarnation()
                }),
            )
            .await,
    )
    .await;
    assert_eq!(listing["data"]["intents"].as_array().unwrap().len(), 0);
}
#[expect(
    clippy::too_many_lines,
    reason = "the end-to-end HTTP drive reads linearly; splitting would scatter the sequence"
)]
#[tokio::test]
async fn http_drive_converges_a_fenced_commit_end_to_end() {
    let fixture = commit_fixture().await;
    let staged = seed_verified_staging(&fixture, b"commit me").await;
    let target = fixture.dir.path().join("target.bin");
    let driver = spawn_commit_task(&fixture, staged.artifact_handle_id, target.clone());
    let intent_id = wait_pending_intent_id(&fixture, staged.artifact_handle_id).await;
    let open_body = json!({
        "node_id": fixture.node_id(),
        "incarnation_id": fixture.incarnation()
    });
    let listing = response_body(
        fixture
            .post("/v1/artifact/commit/open", "drive-open", open_body.clone())
            .await,
    )
    .await;
    assert_eq!(listing["command"], OPEN_COMMAND);
    assert_eq!(listing["status"], "ok");
    let intents = listing["data"]["intents"].as_array().unwrap();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0]["id"], intent_id.0);
    assert_eq!(intents[0]["state"], "pending");
    assert_eq!(
        intents[0]["artifact_handle_id"],
        staged.artifact_handle_id.0
    );
    assert!(intents[0]["staging_storage_root_id"].is_u64());
    assert!(intents[0]["staging_provider_relative_locator"].is_string());
    assert!(intents[0]["target_storage_root_id"].is_u64());
    assert!(!listing.to_string().contains("fence"));

    // authorize mints the one-time fence.
    let authorize_path = format!("/v1/artifact/commit/{}/authorize", intent_id.0);
    let authorize = response_body(
        fixture
            .post(
                &authorize_path,
                "drive-authorize",
                json!({
                    "node_id": fixture.node_id(),
                    "incarnation_id": fixture.incarnation()
                }),
            )
            .await,
    )
    .await;
    let fence_hex = authorize["data"]["fence_hex"].as_str().unwrap().to_owned();
    let staging_locator = authorize["data"]["staging_provider_relative_locator"]
        .as_str()
        .unwrap()
        .to_owned();
    let staging_root_id = authorize["data"]["staging_storage_root_id"]
        .as_u64()
        .unwrap();
    let staged_path = rooted_path(&fixture, staging_root_id, &staging_locator).await;

    // The listing now advertises the authorized state.
    let listing = response_body(
        fixture
            .post(
                "/v1/artifact/commit/open",
                "drive-open-2",
                open_body.clone(),
            )
            .await,
    )
    .await;
    assert_eq!(listing["data"]["intents"][0]["state"], "authorized");

    // applying journals before any byte mutation.
    let applying_path = format!("/v1/artifact/commit/{}/applying", intent_id.0);
    let applying = response_body(
        fixture
            .post(
                &applying_path,
                "drive-applying",
                json!({
                    "node_id": fixture.node_id(),
                    "incarnation_id": fixture.incarnation()
                }),
            )
            .await,
    )
    .await;
    assert_eq!(applying["data"]["intent_id"], intent_id.0);

    // Promote the staged bytes and report applied evidence.
    std::fs::copy(&staged_path, &target).unwrap();
    let promoted = std::fs::read(&staged_path).unwrap();
    let outcome_path = format!("/v1/artifact/commit/{}/outcome", intent_id.0);
    let outcome = response_body(
        fixture
            .post(
                &outcome_path,
                "drive-outcome",
                json!({
                    "node_id": fixture.node_id(),
                    "incarnation_id": fixture.incarnation(),
                    "evidence": {
                        "kind": "applied",
                        "observed": {
                            "size_bytes": promoted.len(),
                            "content_hash": format!(
                                "blake3:{}",
                                blake3::hash(&promoted).to_hex()
                            )
                        }
                    }
                }),
            )
            .await,
    )
    .await;
    assert_eq!(outcome["data"]["kind"], "applied");

    // Complete converges the intent with the exact fence.
    let complete_path = format!("/v1/artifact/commit/{}/complete", intent_id.0);
    let complete = response_body(
        fixture
            .post(
                &complete_path,
                "drive-complete",
                json!({
                    "node_id": fixture.node_id(),
                    "incarnation_id": fixture.incarnation(),
                    "fence_hex": fence_hex
                }),
            )
            .await,
    )
    .await;
    assert_eq!(complete["data"]["intent_id"], intent_id.0);
    assert!(complete["data"]["commit_record_id"].is_u64());
    assert!(complete["data"]["result_file_version_id"].is_u64());
    assert!(complete["data"]["result_file_location_id"].is_u64());
    assert!(target.exists());

    let report = driver.await.unwrap().unwrap();
    assert_eq!(report.state, ArtifactCommitState::Committed);

    // Completed intents drop out of the open listing.
    let listing = response_body(
        fixture
            .post("/v1/artifact/commit/open", "drive-open-3", open_body)
            .await,
    )
    .await;
    assert_eq!(listing["data"]["intents"].as_array().unwrap().len(), 0);
}

#[test]
fn commit_request_hash_includes_route_instance() {
    let body = json!({"node_id": 1, "incarnation_id": INCARNATION, "fence_hex": "ab"});

    let a = stable_request_hash("POST", "/v1/artifact/commit/1/complete", &body).unwrap();
    let b = stable_request_hash("POST", "/v1/artifact/commit/2/complete", &body).unwrap();

    assert_ne!(a, b);
}

#[test]
fn commit_request_dtos_reject_unknown_fields() {
    assert_unknown_field_rejected::<OpenRequest>(json!({
        "node_id": 1,
        "incarnation_id": INCARNATION,
        "unknown": true
    }));
    assert_unknown_field_rejected::<AuthorizeRequest>(json!({
        "node_id": 1,
        "incarnation_id": INCARNATION,
        "unknown": true
    }));
    assert_unknown_field_rejected::<ApplyingRequest>(json!({
        "node_id": 1,
        "incarnation_id": INCARNATION,
        "unknown": true
    }));
    assert_unknown_field_rejected::<OutcomeRequest>(json!({
        "node_id": 1,
        "incarnation_id": INCARNATION,
        "evidence": {"kind": "outcome_unknown", "reason": "r"},
        "unknown": true
    }));
    assert_unknown_field_rejected::<CompleteRequest>(json!({
        "node_id": 1,
        "incarnation_id": INCARNATION,
        "fence_hex": "ab",
        "unknown": true
    }));
}

#[test]
fn commit_request_dtos_require_incarnation_fences() {
    assert_missing_incarnation_rejected::<OpenRequest>(json!({"node_id": 1}));
    assert_missing_incarnation_rejected::<AuthorizeRequest>(json!({"node_id": 1}));
    assert_missing_incarnation_rejected::<ApplyingRequest>(json!({"node_id": 1}));
    assert_missing_incarnation_rejected::<OutcomeRequest>(json!({
        "node_id": 1,
        "evidence": {"kind": "outcome_unknown", "reason": "r"}
    }));
    assert_missing_incarnation_rejected::<CompleteRequest>(json!({
        "node_id": 1,
        "fence_hex": "ab"
    }));
}

fn assert_unknown_field_rejected<T: DeserializeOwned>(value: Value) {
    let Ok(_) = serde_json::from_value::<T>(value) else {
        return;
    };
    panic!("expected unknown-field rejection");
}

fn assert_missing_incarnation_rejected<T: DeserializeOwned>(value: Value) {
    let Ok(_) = serde_json::from_value::<T>(value) else {
        return;
    };
    panic!("expected missing-incarnation rejection");
}
#[test]
fn complete_request_debug_redacts_fence_hex() {
    let request = CompleteRequest {
        node_id: 1,
        incarnation_id: INCARNATION.parse().unwrap(),
        fence_hex: "deadbeef".to_owned(),
    };
    // The one-time fence is capability material: its Debug rendering must
    // never leak it into a log or telemetry surface.
    let rendered = format!("{request:?}");
    assert!(!rendered.contains("deadbeef"), "{rendered}");
    assert!(rendered.contains("[REDACTED]"), "{rendered}");
}
