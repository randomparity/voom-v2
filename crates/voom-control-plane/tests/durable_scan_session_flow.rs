#![expect(
    clippy::unwrap_used,
    reason = "integration tests use unwrap for fixture setup and contract assertions"
)]

use std::sync::Arc;

use secrecy::SecretString;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use voom_control_plane::ControlPlane;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::scan::sessions::RemoteScanCompleteInput;
use voom_control_plane::scan::{
    RemoteScanBatchInput, RemoteScanFailInput, RemoteScanStartInput, ScanObservation,
    ScanReconciliationQuery,
};
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::clock_test_support::ManualClock;
use voom_core::{
    ArtifactAccessMode, Clock, ErrorCode, FileLocationId, LibraryId, NodeId, NodeIncarnationId,
    NodeKind, OperationKind, ProviderLocator, ProviderRelativeLocator, ScanSessionId,
    ScanSessionStatus, ScanTerminalReason, StorageProviderKind, StorageRootId,
};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_test_support::TempDatabase;

const INCARNATION: &str = "0123456789abcdef0123456789abcdef";
const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

struct Fixture {
    _database: TempDatabase,
    cp: ControlPlane,
    pool: sqlx::SqlitePool,
    clock: Arc<ManualClock>,
    node_id: NodeId,
    incarnation_id: NodeIncarnationId,
    token: SecretString,
}

#[tokio::test]
async fn two_batch_replay_completion_has_exact_counts_and_provenance() {
    let fixture = fixture().await;
    let root = create_root(&fixture, "two-batch").await;
    let observed = [
        seed_rooted_location(&fixture, root, "observed/one.mkv").await,
        seed_rooted_location(&fixture, root, "observed/two.mkv").await,
    ];
    let absent = [
        seed_rooted_location(&fixture, root, "absent/one.mkv").await,
        seed_rooted_location(&fixture, root, "absent/two.mkv").await,
    ];
    let requested = request_same_root_concurrently(&fixture, root).await;
    start(&fixture, requested.id, "two-batch-start").await;

    let first = batch(
        &fixture,
        requested.id,
        0,
        "batch-zero",
        'a',
        "observed/one.mkv",
    );
    let accepted = fixture
        .cp
        .accept_scan_observation_batch(first.clone())
        .await
        .unwrap();
    assert_eq!(
        fixture
            .cp
            .accept_scan_observation_batch(first)
            .await
            .unwrap(),
        accepted
    );
    let second = batch(
        &fixture,
        requested.id,
        1,
        "batch-one",
        'b',
        "observed/two.mkv",
    );
    let accepted = fixture
        .cp
        .accept_scan_observation_batch(second.clone())
        .await
        .unwrap();
    let mut ledger_replay = second;
    ledger_replay.idempotency_key = "batch-one-ledger-replay".to_owned();
    assert_eq!(
        fixture
            .cp
            .accept_scan_observation_batch(ledger_replay)
            .await
            .unwrap(),
        accepted
    );

    assert_running_progress(&fixture, requested.id, 2, 2).await;
    let completed = fixture
        .cp
        .complete_scan_session(completion(&fixture, requested.id, "complete", Some(1), 2))
        .await
        .unwrap();
    assert_eq!(completed.status, ScanSessionStatus::Succeeded);
    assert_eq!(completed.observation_count, 2);
    assert_eq!(completed.retired_location_count, 2);
    assert_completed_state(&fixture, root, requested.id, &observed, &absent).await;
    assert_scan_event_counts(&fixture, 1, 1, 2, 1).await;
    assert_eq!(routing_counts(&fixture).await, (0, 0));
}

#[tokio::test]
async fn batch_gap_and_replay_are_deterministic() {
    let fixture = fixture().await;
    let first_root = create_root(&fixture, "gap-first").await;
    let second_root = create_root(&fixture, "gap-second").await;
    let first = running(&fixture, first_root, "gap-first").await;
    fixture
        .cp
        .accept_scan_observation_batch(batch(
            &fixture,
            first,
            0,
            "shared-cross-session-key",
            'a',
            "first.mkv",
        ))
        .await
        .unwrap();
    let second = running(&fixture, second_root, "gap-second").await;
    let cross_session = batch(
        &fixture,
        second,
        0,
        "shared-cross-session-key",
        'b',
        "second.mkv",
    );
    assert_eq!(
        fixture
            .cp
            .accept_scan_observation_batch(cross_session)
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::Conflict
    );

    let zero = batch(&fixture, second, 0, "second-zero", 'c', "zero.mkv");
    let zero_outcome = fixture
        .cp
        .accept_scan_observation_batch(zero.clone())
        .await
        .unwrap();
    assert_eq!(
        fixture
            .cp
            .accept_scan_observation_batch(zero)
            .await
            .unwrap(),
        zero_outcome
    );
    fixture
        .cp
        .accept_scan_observation_batch(batch(&fixture, second, 1, "second-one", 'd', "one.mkv"))
        .await
        .unwrap();
    let gap = fixture
        .cp
        .accept_scan_observation_batch(batch(&fixture, second, 3, "second-three", 'e', "three.mkv"))
        .await
        .unwrap_err();
    assert_eq!(gap.error_code(), ErrorCode::Conflict);
    assert!(gap.to_string().contains("batch 2"));
    assert_running_progress(&fixture, second, 2, 2).await;
    assert_eq!(table_count(&fixture, "scan_observations").await, 3);
    assert_eq!(
        scan_event_count(&fixture, "scan_session.observation_batch_accepted").await,
        3
    );
}

#[tokio::test]
async fn failed_session_never_reconciles() {
    let fixture = fixture().await;
    let failed_root = create_root(&fixture, "failed").await;
    let cancelled_root = create_root(&fixture, "cancelled").await;
    let stale_root = create_root(&fixture, "stale").await;
    let failed_location = seed_rooted_location(&fixture, failed_root, "failed.mkv").await;
    let cancelled_location = seed_rooted_location(&fixture, cancelled_root, "cancelled.mkv").await;
    let stale_location = seed_rooted_location(&fixture, stale_root, "stale.mkv").await;

    let failed = running(&fixture, failed_root, "failed").await;
    fixture
        .cp
        .fail_scan_session(failure(&fixture, failed, "scanner failed"))
        .await
        .unwrap();
    let cancelled = running(&fixture, cancelled_root, "cancelled").await;
    fixture
        .cp
        .cancel_scan_session(
            cancelled,
            ScanTerminalReason::new("operator cancelled").unwrap(),
        )
        .await
        .unwrap();
    let stale = running_with_timeout(&fixture, stale_root, "stale", 10).await;
    fixture.clock.advance(Duration::seconds(10));
    let recovered = fixture
        .cp
        .remote_recover(fixture.clock.now())
        .await
        .unwrap();
    assert_eq!(recovered.stale_scan_sessions, vec![stale]);

    for (id, root, location, status) in [
        (
            failed,
            failed_root,
            failed_location,
            ScanSessionStatus::Failed,
        ),
        (
            cancelled,
            cancelled_root,
            cancelled_location,
            ScanSessionStatus::Cancelled,
        ),
        (stale, stale_root, stale_location, ScanSessionStatus::Stale),
    ] {
        let error = fixture
            .cp
            .complete_scan_session(completion(&fixture, id, "terminal-complete", None, 0))
            .await
            .unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::Conflict);
        assert_eq!(fixture.cp.scan_session(id).await.unwrap().status, status);
        assert_location_live(&fixture, location).await;
        assert_eq!(root_pointer(&fixture, root).await, None);
    }
    assert_eq!(scan_event_count(&fixture, "scan_session.failed").await, 1);
    assert_eq!(
        scan_event_count(&fixture, "scan_session.cancelled").await,
        1
    );
    assert_eq!(scan_event_count(&fixture, "scan_session.stale").await, 1);
    assert_eq!(
        scan_event_count(&fixture, "scan_session.succeeded").await,
        0
    );
}

#[tokio::test]
async fn concurrent_location_above_watermark_remains_live() {
    let fixture = fixture().await;
    let root = create_root(&fixture, "high-watermark").await;
    let pre_start = seed_rooted_location(&fixture, root, "before-start.mkv").await;
    let session = running(&fixture, root, "high-watermark").await;
    let concurrent = seed_rooted_location(&fixture, root, "after-start.mkv").await;

    let completed = fixture
        .cp
        .complete_scan_session(completion(&fixture, session, "empty-complete", None, 0))
        .await
        .unwrap();

    assert_eq!(completed.retired_location_count, 1);
    assert_location_retired(&fixture, pre_start, session).await;
    assert_location_live(&fixture, concurrent).await;
    let page = fixture
        .cp
        .scan_reconciliation(ScanReconciliationQuery {
            scan_session_id: session,
            after_id: None,
            limit: 50,
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].file_location_id, FileLocationId(pre_start));
}

async fn fixture() -> Fixture {
    let database = TempDatabase::new().unwrap();
    let url = voom_store::test_support::sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(T0));
    let cp = ControlPlane::open_with_pool(pool.clone(), clock.clone())
        .await
        .unwrap();
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
        node_id: registered.node.id,
        token: registered.token.clone(),
        idempotency_key: "durable-scan-charter-activate".to_owned(),
        request_hash: "durable-scan-charter-activate-body".to_owned(),
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
    Fixture {
        _database: database,
        cp,
        pool,
        clock,
        node_id: registered.node.id,
        incarnation_id,
        token: registered.token,
    }
}

async fn request_same_root_concurrently(
    fixture: &Fixture,
    root: StorageRootId,
) -> voom_control_plane::scan::ScanSession {
    let first = fixture.cp.clone();
    let second = fixture.cp.clone();
    let (first, second) = tokio::join!(
        first.request_scan_session(root, 300),
        second.request_scan_session(root, 300)
    );
    let outcomes = [first, second];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(outcomes.iter().filter(|result| result.is_err()).count(), 1);

    let mut winner = None;
    for outcome in outcomes {
        match outcome {
            Ok(session) => winner = Some(session),
            Err(error) => assert_eq!(error.error_code(), ErrorCode::Conflict),
        }
    }
    let winner = winner.unwrap();
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_sessions WHERE storage_root_id = ? \
         AND status IN ('requested', 'running')",
    )
    .bind(i64::try_from(root.0).unwrap())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(active, 1);
    assert_eq!(scan_event_count(fixture, "scan_session.requested").await, 1);
    winner
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

async fn running(fixture: &Fixture, root: StorageRootId, key: &str) -> ScanSessionId {
    running_with_timeout(fixture, root, key, 300).await
}

async fn running_with_timeout(
    fixture: &Fixture,
    root: StorageRootId,
    key: &str,
    timeout: u32,
) -> ScanSessionId {
    let session = fixture
        .cp
        .request_scan_session(root, timeout)
        .await
        .unwrap();
    start(fixture, session.id, key).await;
    session.id
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

fn batch(
    fixture: &Fixture,
    id: ScanSessionId,
    sequence: u64,
    key: &str,
    hash: char,
    locator: &str,
) -> RemoteScanBatchInput {
    RemoteScanBatchInput {
        node_id: fixture.node_id,
        scan_session_id: id,
        incarnation_id: fixture.incarnation_id,
        token: fixture.token.clone(),
        idempotency_key: key.to_owned(),
        request_hash: hash.to_string().repeat(64),
        sequence,
        observations: vec![ScanObservation {
            provider_relative_locator: ProviderRelativeLocator::new(locator.to_owned()).unwrap(),
            provider_object_identity: format!("object-{hash}"),
            size_bytes: sequence + 1,
            modified_at: T0,
            stability_started_at: T0,
            stability_confirmed_at: T0,
        }],
    }
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

fn failure(fixture: &Fixture, id: ScanSessionId, reason: &str) -> RemoteScanFailInput {
    RemoteScanFailInput {
        node_id: fixture.node_id,
        scan_session_id: id,
        incarnation_id: fixture.incarnation_id,
        token: fixture.token.clone(),
        idempotency_key: format!("fail-{}", id.0),
        request_hash: format!("fail-{}-body", id.0),
        reason: ScanTerminalReason::new(reason).unwrap(),
    }
}

async fn seed_rooted_location(fixture: &Fixture, root: StorageRootId, locator: &str) -> u64 {
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
    .bind(format!("charter-hash-{asset}"))
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
        .bind(i64::try_from(root.0).unwrap())
        .bind(locator)
        .bind("1970-01-01T00:00:00Z")
        .execute(&fixture.pool)
        .await
        .unwrap()
        .last_insert_rowid(),
    )
    .unwrap()
}

async fn assert_running_progress(
    fixture: &Fixture,
    id: ScanSessionId,
    batches: u64,
    observations: u64,
) {
    let session = fixture.cp.scan_session(id).await.unwrap();
    assert_eq!(session.status, ScanSessionStatus::Running);
    assert_eq!(session.next_sequence, batches);
    assert_eq!(session.batch_count, batches);
    assert_eq!(session.observation_count, observations);
    assert_eq!(
        table_count_for_session(fixture, "scan_observation_batches", id).await,
        batches
    );
    assert_eq!(
        table_count_for_session(fixture, "scan_observations", id).await,
        observations
    );
}

async fn assert_completed_state(
    fixture: &Fixture,
    root: StorageRootId,
    session_id: ScanSessionId,
    observed: &[u64; 2],
    absent: &[u64; 2],
) {
    let session = fixture.cp.scan_session(session_id).await.unwrap();
    assert_eq!(session.status, ScanSessionStatus::Succeeded);
    assert_eq!(session.batch_count, 2);
    assert_eq!(session.observation_count, 2);
    assert_eq!(session.retired_location_count, 2);
    assert!(session.terminal_at.is_some());
    assert_eq!(root_pointer(fixture, root).await, Some(session_id));
    for id in observed {
        assert_location_live(fixture, *id).await;
    }
    for id in absent {
        assert_location_retired(fixture, *id, session_id).await;
    }
    let first = reconciliation(fixture, session_id, None, 1).await;
    assert_eq!(first.items[0].file_location_id, FileLocationId(absent[0]));
    assert_eq!(first.items[0].prior_epoch, 0);
    assert_eq!(first.items[0].retired_epoch, 1);
    let second = reconciliation(fixture, session_id, first.next_after_id, 1).await;
    assert_eq!(second.items[0].file_location_id, FileLocationId(absent[1]));
    assert_eq!(second.next_after_id, None);
}

async fn reconciliation(
    fixture: &Fixture,
    id: ScanSessionId,
    after_id: Option<FileLocationId>,
    limit: u32,
) -> voom_control_plane::scan::ScanReconciliationPage {
    fixture
        .cp
        .scan_reconciliation(ScanReconciliationQuery {
            scan_session_id: id,
            after_id,
            limit,
        })
        .await
        .unwrap()
}

async fn assert_scan_event_counts(
    fixture: &Fixture,
    requested: i64,
    started: i64,
    batches: i64,
    succeeded: i64,
) {
    assert_eq!(
        scan_event_count(fixture, "scan_session.requested").await,
        requested
    );
    assert_eq!(
        scan_event_count(fixture, "scan_session.started").await,
        started
    );
    assert_eq!(
        scan_event_count(fixture, "scan_session.observation_batch_accepted").await,
        batches
    );
    assert_eq!(
        scan_event_count(fixture, "scan_session.succeeded").await,
        succeeded
    );
}

async fn scan_event_count(fixture: &Fixture, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?")
        .bind(kind)
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
}

async fn table_count(fixture: &Fixture, table: &str) -> u64 {
    let query = format!("SELECT COUNT(*) FROM {table}");
    let count: i64 = sqlx::query_scalar(&query)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    u64::try_from(count).unwrap()
}

async fn table_count_for_session(fixture: &Fixture, table: &str, id: ScanSessionId) -> u64 {
    let query = format!("SELECT COUNT(*) FROM {table} WHERE scan_session_id = ?");
    let count: i64 = sqlx::query_scalar(&query)
        .bind(i64::try_from(id.0).unwrap())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    u64::try_from(count).unwrap()
}

async fn routing_counts(fixture: &Fixture) -> (i64, i64) {
    let tickets = sqlx::query_scalar("SELECT COUNT(*) FROM tickets")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let leases = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    (tickets, leases)
}

async fn root_pointer(fixture: &Fixture, root: StorageRootId) -> Option<ScanSessionId> {
    let value: Option<i64> =
        sqlx::query_scalar("SELECT last_scan_session_id FROM library_roots WHERE id = ?")
            .bind(i64::try_from(root.0).unwrap())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    value.map(|id| ScanSessionId(u64::try_from(id).unwrap()))
}

async fn assert_location_live(fixture: &Fixture, id: u64) {
    let row: (Option<String>, i64, Option<i64>) = sqlx::query_as(
        "SELECT retired_at, epoch, retired_by_scan_session_id FROM file_locations WHERE id = ?",
    )
    .bind(i64::try_from(id).unwrap())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(row, (None, 0, None));
}

async fn assert_location_retired(fixture: &Fixture, id: u64, session: ScanSessionId) {
    let row: (Option<String>, i64, Option<i64>) = sqlx::query_as(
        "SELECT retired_at, epoch, retired_by_scan_session_id FROM file_locations WHERE id = ?",
    )
    .bind(i64::try_from(id).unwrap())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert!(row.0.is_some());
    assert_eq!(row.1, 1);
    assert_eq!(row.2, Some(i64::try_from(session.0).unwrap()));
}
