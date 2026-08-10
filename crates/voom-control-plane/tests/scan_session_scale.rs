#![expect(
    clippy::print_stdout,
    clippy::unwrap_used,
    reason = "the opt-in release gate prints measured timings and treats fixture failures as fatal"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use time::OffsetDateTime;
use voom_control_plane::ControlPlane;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::scan::sessions::{RemoteScanCompleteInput, RemoteScanStartInput};
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::clock_test_support::ManualClock;
use voom_core::{
    ArtifactAccessMode, LibraryId, NodeId, NodeIncarnationId, OperationKind, ProviderLocator,
    ScanSessionStatus, StorageProviderKind, StorageRootId,
};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};

const LOCATION_COUNT: u64 = 100_000;
const REPETITIONS: usize = 3;
const MAX_COMPLETION: Duration = Duration::from_secs(25);
const INCARNATION: &str = "abcdef0123456789abcdef0123456789";

struct ScaleFixture {
    cp: ControlPlane,
    pool: sqlx::SqlitePool,
    token: secrecy::SecretString,
    node_id: NodeId,
    incarnation_id: NodeIncarnationId,
    root_id: StorageRootId,
    _database: voom_test_support::TempDatabase,
}

#[tokio::test]
#[ignore = "release-only 100,000-location completion budget"]
async fn empty_scan_reconciles_100k_within_api_budget() {
    for repetition in 0..REPETITIONS {
        let fixture = scale_fixture(repetition).await;
        load_locations(&fixture.pool, fixture.root_id).await;
        let requested = fixture
            .cp
            .request_scan_session(fixture.root_id, 300)
            .await
            .unwrap();
        fixture
            .cp
            .start_scan_session(RemoteScanStartInput {
                node_id: fixture.node_id,
                scan_session_id: requested.id,
                incarnation_id: fixture.incarnation_id,
                token: fixture.token.clone(),
                idempotency_key: format!("scale-start-{repetition}"),
                request_hash: format!("scale-start-body-{repetition}"),
            })
            .await
            .unwrap();

        let started = Instant::now();
        let outcome = fixture
            .cp
            .complete_scan_session(RemoteScanCompleteInput {
                node_id: fixture.node_id,
                scan_session_id: requested.id,
                incarnation_id: fixture.incarnation_id,
                token: fixture.token.clone(),
                idempotency_key: format!("scale-complete-{repetition}"),
                request_hash: format!("scale-complete-body-{repetition}"),
                last_sequence: None,
                observation_count: 0,
            })
            .await
            .unwrap();
        let elapsed = started.elapsed();

        println!("scan completion repetition {}: {elapsed:?}", repetition + 1);
        assert!(
            elapsed <= MAX_COMPLETION,
            "{elapsed:?} exceeded {MAX_COMPLETION:?}"
        );
        assert_eq!(outcome.status, ScanSessionStatus::Succeeded);
        assert_eq!(outcome.retired_location_count, LOCATION_COUNT);
        assert_completion_counts(&fixture, requested.id).await;
    }
}

async fn scale_fixture(repetition: usize) -> ScaleFixture {
    let database = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", database.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(OffsetDateTime::UNIX_EPOCH));
    let cp = ControlPlane::open_with_pool(pool.clone(), clock)
        .await
        .unwrap();
    let registered = cp
        .register_node(RegisterNodeInput {
            name: format!("scale-scan-owner-{repetition}"),
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
        idempotency_key: format!("scale-activate-{repetition}"),
        request_hash: format!("scale-activate-body-{repetition}"),
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
    let root_id = create_root(&cp, registered.node.id, repetition).await;
    ScaleFixture {
        cp,
        pool,
        token: registered.token,
        node_id: registered.node.id,
        incarnation_id,
        root_id,
        _database: database,
    }
}

async fn create_root(cp: &ControlPlane, owner: NodeId, repetition: usize) -> StorageRootId {
    let library = cp
        .create_library(NewLibrary {
            slug: format!("scale-scan-{repetition}"),
            display_name: format!("Scale Scan {repetition}"),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = cp
        .create_library_root(scale_root(library.id, owner, repetition))
        .await
        .unwrap();
    cp.activate_library_root(root.id, format!("scale-scan-{repetition}"))
        .await
        .unwrap();
    root.id
}

fn scale_root(library_id: LibraryId, owner: NodeId, repetition: usize) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id: owner,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(format!("/scale-scan/{repetition}")).unwrap(),
        display_locator: format!("/scale-scan/{repetition}"),
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

async fn load_locations(pool: &sqlx::SqlitePool, root_id: StorageRootId) {
    let asset = sqlx::query("INSERT INTO file_assets (created_at, epoch) VALUES (?, 0)")
        .bind("1970-01-01T00:00:00Z")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let version = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         created_at, epoch) VALUES (?, 'scale-hash', 1, 'ingest', ?, 0)",
    )
    .bind(asset)
    .bind("1970-01-01T00:00:00Z")
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "WITH RECURSIVE numbers(value) AS (\
             SELECT 1 UNION ALL SELECT value + 1 FROM numbers WHERE value < ?\
         )\
         INSERT INTO file_locations (file_version_id, address_state, storage_root_id, \
             provider_relative_locator, observed_at, epoch)\
         SELECT ?, 'rooted', ?, 'scale/' || value, '1970-01-01T00:00:00Z', 0 FROM numbers",
    )
    .bind(i64::try_from(LOCATION_COUNT).unwrap())
    .bind(version)
    .bind(i64::try_from(root_id.0).unwrap())
    .execute(pool)
    .await
    .unwrap();
}

async fn assert_completion_counts(fixture: &ScaleFixture, session_id: voom_core::ScanSessionId) {
    let session = fixture.cp.scan_session(session_id).await.unwrap();
    assert_eq!(session.retired_location_count, LOCATION_COUNT);
    let attributed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM file_locations WHERE retired_by_scan_session_id = ?",
    )
    .bind(i64::try_from(session_id.0).unwrap())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM file_locations WHERE storage_root_id = ? AND retired_at IS NULL",
    )
    .bind(i64::try_from(fixture.root_id.0).unwrap())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let root_pointer: Option<i64> =
        sqlx::query_scalar("SELECT last_scan_session_id FROM library_roots WHERE id = ?")
            .bind(i64::try_from(fixture.root_id.0).unwrap())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(attributed, i64::try_from(LOCATION_COUNT).unwrap());
    assert_eq!(live, 0);
    assert_eq!(root_pointer, Some(i64::try_from(session_id.0).unwrap()));
}
