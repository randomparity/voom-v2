#![expect(
    clippy::unwrap_used,
    reason = "integration tests use unwrap for fixture setup and contract assertions"
)]
//! Research spike (#841): per-file publication at batch acceptance.

use std::sync::Arc;

use secrecy::SecretString;
use serde_json::json;
use time::OffsetDateTime;
use voom_control_plane::ControlPlane;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::scan::sessions::RemoteScanCompleteInput;
use voom_control_plane::scan::{RemoteScanBatchInput, RemoteScanStartInput, ScanObservation};
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::clock_test_support::ManualClock;
use voom_core::{
    ArtifactAccessMode, LibraryId, NodeId, NodeIncarnationId, NodeKind, OperationKind,
    ProviderLocator, ProviderRelativeLocator, ScanObservationEvidence, ScanSessionId,
    ScanSessionStatus, StorageProviderKind, StorageRootId,
};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_test_support::TempDatabase;

const INCARNATION: &str = "0123456789abcdef0123456789abcdef";
const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

struct Fixture {
    #[expect(dead_code, reason = "keeps the temporary database alive")]
    database: TempDatabase,
    cp: ControlPlane,
    pool: sqlx::SqlitePool,
    #[expect(dead_code, reason = "shared fixture shape")]
    clock: Arc<ManualClock>,
    node_id: NodeId,
    incarnation_id: NodeIncarnationId,
    token: SecretString,
}

#[tokio::test]
async fn per_file_publication_publishes_at_batch_acceptance() {
    let fixture = fixture(true).await;
    let root = create_root(&fixture, "per-file").await;
    let session = running(&fixture, root).await;
    accept(&fixture, session, 0, "a", "one.mkv").await;
    assert_eq!(live_locations(&fixture, root).await, 1);
    accept(&fixture, session, 1, "b", "two.mkv").await;
    assert_eq!(live_locations(&fixture, root).await, 2);
    assert_eq!(scan_status(&fixture, session).await, "running");
}

#[tokio::test]
async fn per_file_publication_completion_does_not_republish() {
    let fixture = fixture(true).await;
    let root = create_root(&fixture, "per-file-complete").await;
    let session = running(&fixture, root).await;
    accept(&fixture, session, 0, "a", "one.mkv").await;
    accept(&fixture, session, 1, "b", "two.mkv").await;
    let completed = fixture
        .cp
        .complete_scan_session(completion(&fixture, session, "complete", Some(1), 2))
        .await
        .unwrap();
    assert_eq!(completed.status, ScanSessionStatus::Succeeded);
    assert_eq!(live_locations(&fixture, root).await, 2);
    assert_eq!(table_count(&fixture, "media_snapshots").await, 2);
    assert_eq!(table_count(&fixture, "file_versions").await, 2);
}

#[tokio::test]
async fn completion_publication_publishes_nothing_at_batch_acceptance() {
    let fixture = fixture(false).await;
    let root = create_root(&fixture, "completion").await;
    let session = running(&fixture, root).await;
    accept(&fixture, session, 0, "a", "one.mkv").await;
    assert_eq!(live_locations(&fixture, root).await, 0);
    fixture
        .cp
        .complete_scan_session(completion(&fixture, session, "complete", Some(0), 1))
        .await
        .unwrap();
    assert_eq!(live_locations(&fixture, root).await, 1);
    assert_eq!(table_count(&fixture, "media_snapshots").await, 1);
}

async fn running(fixture: &Fixture, root: StorageRootId) -> ScanSessionId {
    let session = fixture.cp.request_scan_session(root, 300).await.unwrap();
    start(fixture, session.id, "per-file").await;
    session.id
}

async fn accept(fixture: &Fixture, id: ScanSessionId, sequence: u64, hash: &str, locator: &str) {
    fixture
        .cp
        .accept_scan_observation_batch(RemoteScanBatchInput {
            node_id: fixture.node_id,
            scan_session_id: id,
            incarnation_id: fixture.incarnation_id,
            token: fixture.token.clone(),
            idempotency_key: format!("batch-{sequence}"),
            request_hash: hash.repeat(64),
            sequence,
            observations: vec![ScanObservation {
                provider_relative_locator: ProviderRelativeLocator::new(locator.to_owned())
                    .unwrap(),
                provider_object_identity: format!("object-{hash}"),
                size_bytes: 123,
                modified_at: T0,
                stability_started_at: T0,
                stability_confirmed_at: T0,
                evidence: Some(evidence(&format!("blake3:{}", hash.repeat(64)))),
            }],
        })
        .await
        .unwrap();
}

fn evidence(content_hash: &str) -> ScanObservationEvidence {
    ScanObservationEvidence {
        content_hash: content_hash.to_owned(),
        size_bytes: 123,
        modified_at: "1970-01-01T00:00:00Z".to_owned(),
        file_key: None,
        sidecars: Vec::new(),
        probe_snapshot: json!({
            "format": "publish-v1",
            "streams": [{ "index": 0, "codec_name": "h264", "disposition": { "default": true } }],
        }),
        hash_source: None,
    }
}

async fn live_locations(fixture: &Fixture, root: StorageRootId) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM file_locations WHERE retired_at IS NULL AND storage_root_id = ?",
    )
    .bind(i64::try_from(root.0).unwrap())
    .fetch_one(&fixture.pool)
    .await
    .unwrap()
}

async fn scan_status(fixture: &Fixture, id: ScanSessionId) -> String {
    sqlx::query_scalar("SELECT status FROM scan_sessions WHERE id = ?")
        .bind(i64::try_from(id.0).unwrap())
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
}

async fn table_count(fixture: &Fixture, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
}

async fn fixture(per_file: bool) -> Fixture {
    let database = TempDatabase::new().unwrap();
    let url = voom_store::test_support::sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(T0));
    let cp = ControlPlane::open_with_pool(pool.clone(), clock.clone())
        .await
        .unwrap()
        .with_scan_per_file_publication(per_file);
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "durable-scan-charter-owner".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 600,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let incarnation_id = INCARNATION.parse().unwrap();
    cp.remote_activate(RemoteActivateInput {
        storage_roots: Vec::new(),
        node_id: registered.node.id,
        token: registered.token.clone(),
        idempotency_key: "durable-scan-charter-activate".to_owned(),
        request_hash: "durable-scan-charter-activate-body".to_owned(),
        incarnation_id,
        workers: vec![RemoteWorkerDeclaration {
            logical_name: "scan".to_owned(),
            operations: vec![OperationKind::ProbeFile],
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            accelerator: None,
            max_parallel: 1,
        }],
    })
    .await
    .unwrap();
    Fixture {
        database,
        cp,
        pool,
        clock,
        node_id: registered.node.id,
        incarnation_id,
        token: registered.token,
    }
}

async fn create_root(fixture: &Fixture, suffix: &str) -> StorageRootId {
    let library = fixture
        .cp
        .create_library(NewLibrary {
            slug: format!("charter-{suffix}"),
            display_name: format!("Charter {suffix}"),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = fixture
        .cp
        .create_library_root(new_root(library.id, fixture.node_id, suffix))
        .await
        .unwrap();
    fixture
        .cp
        .activate_library_root(root.id, format!("charter-{suffix}"))
        .await
        .unwrap();
    root.id
}

fn new_root(library_id: LibraryId, owner: NodeId, suffix: &str) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id: owner,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(format!("/charter/{suffix}")).unwrap(),
        display_locator: format!("/charter/{suffix}"),
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

async fn start(fixture: &Fixture, id: ScanSessionId, key: &str) {
    fixture
        .cp
        .start_scan_session(RemoteScanStartInput {
            node_id: fixture.node_id,
            scan_session_id: id,
            incarnation_id: fixture.incarnation_id,
            token: fixture.token.clone(),
            idempotency_key: format!("start-{key}"),
            request_hash: format!("start-{key}-body"),
        })
        .await
        .unwrap();
}

fn completion(
    fixture: &Fixture,
    id: ScanSessionId,
    key: &str,
    last_sequence: Option<u64>,
    observation_count: u64,
) -> RemoteScanCompleteInput {
    RemoteScanCompleteInput {
        node_id: fixture.node_id,
        scan_session_id: id,
        incarnation_id: fixture.incarnation_id,
        token: fixture.token.clone(),
        idempotency_key: format!("{key}-{}", id.0),
        request_hash: format!("{key}-{}-body", id.0),
        last_sequence,
        observation_count,
    }
}
