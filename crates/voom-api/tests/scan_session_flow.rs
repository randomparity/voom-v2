#![expect(
    clippy::unwrap_used,
    reason = "integration tests use unwrap for fixture setup and HTTP assertions"
)]

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::BodyExt;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt;
use voom_api::router_with_control_plane;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{
    ArtifactAccessMode, LibraryId, NodeId, NodeIncarnationId, NodeKind, OperationKind,
    ProviderLocator, StorageProviderKind, StorageRootId,
};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_test_support::TempDatabase;

const INCARNATION: &str = "0123456789abcdef0123456789abcdef";
const PRIVATE_LOCATOR: &str = "private/observed-charter.mkv";
const PRIVATE_OBJECT: &str = "private-object-identity-charter";

struct Fixture {
    _database: TempDatabase,
    app: axum::Router,
    cp: ControlPlane,
    pool: sqlx::SqlitePool,
    node_id: NodeId,
    incarnation_id: NodeIncarnationId,
    token: String,
    root_id: StorageRootId,
}

impl Fixture {
    async fn post(&self, path: &str, key: &str, body: Value) -> Response<Body> {
        self.app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(AUTHORIZATION, format!("Bearer {}", self.token))
                    .header("content-type", "application/json")
                    .header("x-voom-idempotency-key", key)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn get(&self, path: &str) -> Response<Body> {
        self.app
            .clone()
            .oneshot(
                Request::get(path)
                    .header(AUTHORIZATION, format!("Bearer {}", self.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn router_authentication_precedes_scan_session_disclosure() {
    let fixture = fixture().await;
    let hidden_id = 999_419_u64;
    let path = format!(
        "/v1/scan/node/{}/session/{hidden_id}/start",
        fixture.node_id.0
    );
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .header("x-voom-idempotency-key", "unauthorized-charter")
                .body(Body::from(
                    json!({"incarnation_id": INCARNATION}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(WWW_AUTHENTICATE),
        Some(&HeaderValue::from_static("Bearer realm=\"voom\""))
    );
    let body = response_json(response).await;
    assert_eq!(body["command"], "scan.start");
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    assert!(!body.to_string().contains(&hidden_id.to_string()));
}

#[tokio::test]
async fn router_flow_proves_wire_replay_progress_terminal_and_pagination() {
    let fixture = fixture().await;
    let observed = seed_rooted_location(&fixture, PRIVATE_LOCATOR).await;
    let absent = [
        seed_rooted_location(&fixture, "absent/one.mkv").await,
        seed_rooted_location(&fixture, "absent/two.mkv").await,
        seed_rooted_location(&fixture, "absent/three.mkv").await,
    ];
    let session = fixture
        .cp
        .request_scan_session(fixture.root_id, 300)
        .await
        .unwrap();
    let base = format!(
        "/v1/scan/node/{}/session/{}",
        fixture.node_id.0, session.id.0
    );

    let start_body = json!({"incarnation_id": fixture.incarnation_id});
    let start = replay_post(&fixture, &format!("{base}/start"), "start", start_body).await;
    assert_ok_envelope(&start, "scan.start");
    assert_keys(
        &start["data"],
        &[
            "location_high_watermark_id",
            "owner_incarnation_id",
            "progress_deadline_at",
            "scan_session_id",
            "status",
        ],
    );
    assert_eq!(start["data"]["status"], "running");

    let batch_body = json!({
        "incarnation_id": fixture.incarnation_id,
        "observations": [wire_observation(PRIVATE_LOCATOR, PRIVATE_OBJECT)]
    });
    let batch = replay_post(&fixture, &format!("{base}/batch/0"), "batch", batch_body).await;
    assert_ok_envelope(&batch, "scan.batch");
    assert_keys(
        &batch["data"],
        &[
            "accepted_observation_count",
            "cumulative_observation_count",
            "scan_session_id",
            "sequence",
        ],
    );
    assert_replay_counts(&fixture, session.id.0).await;

    let inspect_path = format!("{base}?incarnation_id={INCARNATION}");
    let running = response_json(fixture.get(&inspect_path).await).await;
    assert_ok_envelope(&running, "scan.inspect");
    assert_eq!(running["data"]["status"], "running");
    assert_eq!(running["data"]["next_sequence"], 1);
    assert_eq!(running["data"]["batch_count"], 1);
    assert_eq!(running["data"]["observation_count"], 1);

    let complete = replay_post(
        &fixture,
        &format!("{base}/complete"),
        "complete",
        json!({
            "incarnation_id": fixture.incarnation_id,
            "last_sequence": 0,
            "observation_count": 1
        }),
    )
    .await;
    assert_ok_envelope(&complete, "scan.complete");
    assert_keys(
        &complete["data"],
        &[
            "observation_count",
            "retired_location_count",
            "scan_session_id",
            "status",
        ],
    );
    assert_eq!(complete["data"]["status"], "succeeded");
    assert_eq!(complete["data"]["retired_location_count"], 3);

    let terminal = response_json(fixture.get(&inspect_path).await).await;
    assert_eq!(terminal["data"]["status"], "succeeded");
    assert_eq!(terminal["data"]["retired_location_count"], 3);
    assert!(terminal["data"]["terminal_at"].is_string());
    assert_reconciliation_pages(&fixture, &base, observed, absent).await;
    assert_private_facts_absent(&[start, batch, running, complete, terminal]);
}

#[tokio::test]
async fn inspect_rejects_a_mismatched_attributed_retirement_count() {
    let fixture = fixture().await;
    seed_rooted_location(&fixture, "corrupt-retired-count.mkv").await;
    let session = fixture
        .cp
        .request_scan_session(fixture.root_id, 300)
        .await
        .unwrap();
    let base = format!(
        "/v1/scan/node/{}/session/{}",
        fixture.node_id.0, session.id.0
    );
    let start = replay_post(
        &fixture,
        &format!("{base}/start"),
        "corrupt-count-start",
        json!({"incarnation_id": fixture.incarnation_id}),
    )
    .await;
    assert_ok_envelope(&start, "scan.start");
    let complete = replay_post(
        &fixture,
        &format!("{base}/complete"),
        "corrupt-count-complete",
        json!({
            "incarnation_id": fixture.incarnation_id,
            "last_sequence": null,
            "observation_count": 0
        }),
    )
    .await;
    assert_ok_envelope(&complete, "scan.complete");
    assert_eq!(complete["data"]["retired_location_count"], 1);

    sqlx::query("UPDATE scan_sessions SET retired_location_count = 2 WHERE id = ?")
        .bind(i64::try_from(session.id.0).unwrap())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let path = format!("{base}?incarnation_id={INCARNATION}");
    let response = fixture.get(&path).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response_json(response).await;
    assert_eq!(body["command"], "scan.inspect");
    assert_eq!(body["error"]["code"], "DB_UNREACHABLE");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not match 1 attributed locations")
    );
}

async fn fixture() -> Fixture {
    let database = TempDatabase::new().unwrap();
    let url = voom_store::test_support::sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "scan-api-charter-owner".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 600,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let incarnation_id = INCARNATION.parse().unwrap();
    cp.remote_activate(RemoteActivateInput {
        node_id: registered.node.id,
        token: registered.token.clone(),
        idempotency_key: "scan-api-charter-activate".to_owned(),
        request_hash: "scan-api-charter-activate-body".to_owned(),
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
    let root_id = create_root(&cp, registered.node.id).await;
    let health = HealthPlane::open(&url).await.unwrap();
    let app = router_with_control_plane(health, cp.clone());
    Fixture {
        _database: database,
        app,
        cp,
        pool,
        node_id: registered.node.id,
        incarnation_id,
        token: registered.token.expose_secret().to_owned(),
        root_id,
    }
}

async fn create_root(cp: &ControlPlane, owner: NodeId) -> StorageRootId {
    let library = cp
        .create_library(NewLibrary {
            slug: "scan-api-charter".to_owned(),
            display_name: "Scan API charter".to_owned(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = cp
        .create_library_root(new_root(library.id, owner))
        .await
        .unwrap();
    cp.activate_library_root(root.id, "scan-api-charter".to_owned())
        .await
        .unwrap();
    root.id
}

fn new_root(library_id: LibraryId, owner: NodeId) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id: owner,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new("/scan-api-charter".to_owned()).unwrap(),
        display_locator: "/scan-api-charter".to_owned(),
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

async fn replay_post(fixture: &Fixture, path: &str, key: &str, body: Value) -> Value {
    let first = fixture.post(path, key, body.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = response_json(first).await;
    let replay = fixture.post(path, key, body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(response_json(replay).await, first);
    first
}

fn wire_observation(locator: &str, identity: &str) -> Value {
    json!({
        "provider_relative_locator": locator,
        "provider_object_identity": identity,
        "size_bytes": 419,
        "modified_at": "1970-01-01T00:00:00Z",
        "stability_started_at": "1970-01-01T00:00:00Z",
        "stability_confirmed_at": "1970-01-01T00:00:00Z"
    })
}

async fn seed_rooted_location(fixture: &Fixture, locator: &str) -> u64 {
    let asset = sqlx::query("INSERT INTO file_assets (created_at, epoch) VALUES (?, 0)")
        .bind("1970-01-01T00:00:00Z")
        .execute(&fixture.pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let version = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at, retired_at, epoch) \
         VALUES (?, ?, 1, 'ingest', NULL, ?, NULL, 0)",
    )
    .bind(asset)
    .bind(format!("scan-api-charter-hash-{asset}"))
    .bind("1970-01-01T00:00:00Z")
    .execute(&fixture.pool)
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
        .bind(i64::try_from(fixture.root_id.0).unwrap())
        .bind(locator)
        .bind("1970-01-01T00:00:00Z")
        .execute(&fixture.pool)
        .await
        .unwrap()
        .last_insert_rowid(),
    )
    .unwrap()
}

async fn assert_replay_counts(fixture: &Fixture, session_id: u64) {
    let observations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_observations WHERE scan_session_id = ?")
            .bind(i64::try_from(session_id).unwrap())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE kind = 'scan_session.observation_batch_accepted'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(observations, 1);
    assert_eq!(events, 1);
}

async fn assert_reconciliation_pages(
    fixture: &Fixture,
    base: &str,
    observed: u64,
    absent: [u64; 3],
) {
    let first_path = format!("{base}/reconciliation?incarnation_id={INCARNATION}&limit=2");
    let first = response_json(fixture.get(&first_path).await).await;
    assert_ok_envelope(&first, "scan.reconciliation");
    assert_eq!(first["data"]["items"].as_array().unwrap().len(), 2);
    let cursor = first["data"]["next_after_id"].as_u64().unwrap();
    assert_eq!(first["data"]["items"][0]["file_location_id"], absent[0]);
    assert_eq!(first["data"]["items"][1]["file_location_id"], absent[1]);
    let second_path =
        format!("{base}/reconciliation?incarnation_id={INCARNATION}&after_id={cursor}&limit=2");
    let second = response_json(fixture.get(&second_path).await).await;
    assert_eq!(second["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["data"]["items"][0]["file_location_id"], absent[2]);
    assert!(second["data"]["next_after_id"].is_null());
    for page in [&first, &second] {
        for item in page["data"]["items"].as_array().unwrap() {
            assert_ne!(item["file_location_id"], observed);
        }
    }
    assert_private_facts_absent(&[first, second]);
}

fn assert_ok_envelope(value: &Value, command: &str) {
    assert_keys(
        value,
        &[
            "command",
            "data",
            "error",
            "schema_version",
            "status",
            "warnings",
        ],
    );
    assert_eq!(value["schema_version"], "0");
    assert_eq!(value["command"], command);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["warnings"], json!([]));
    assert!(value["error"].is_null());
}

fn assert_keys(value: &Value, expected: &[&str]) {
    let actual = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
}

fn assert_private_facts_absent(values: &[Value]) {
    for value in values {
        let text = value.to_string();
        assert!(!text.contains(PRIVATE_LOCATOR));
        assert!(!text.contains(PRIVATE_OBJECT));
    }
}

async fn response_json(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
