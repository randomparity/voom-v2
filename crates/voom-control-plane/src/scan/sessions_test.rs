use std::sync::{Arc, Mutex};

use secrecy::SecretString;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use voom_core::{
    ArtifactAccessMode, ErrorCode, LibraryId, NodeId, NodeIncarnationId, OperationKind,
    ProviderLocator, ProviderRelativeLocator, ScanSessionId, ScanSessionStatus, ScanTerminalReason,
    StorageProviderKind, StorageRootId, clock_test_support::ManualClock,
    rng_test_support::FrozenRng,
};
use voom_events::EventKind;
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_store::repo::scan::sessions::ScanObservation;

use super::{
    RemoteScanBatchInput, RemoteScanBatchOutcome, RemoteScanFailInput, RemoteScanInspectInput,
    RemoteScanReconciliationInput, RemoteScanStartInput, RemoteScanStartOutcome,
    RemoteScanTerminalOutcome,
};
use crate::cases::execution::remote_execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use crate::cases::workers::nodes::RegisterNodeInput;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
const INCARNATION: &str = "0123456789abcdef0123456789abcdef";

struct Fixture {
    cp: crate::ControlPlane,
    clock: Arc<ManualClock>,
    token: SecretString,
    node_id: NodeId,
    incarnation_id: NodeIncarnationId,
    root_id: StorageRootId,
    _database: voom_test_support::TempDatabase,
}

#[derive(Debug, Clone, Copy)]
enum RootCase {
    LibraryDisabled,
    Unavailable,
    Unassigned,
    Retired,
    OwnerRetired,
}

#[derive(Debug, Clone, Copy)]
enum RollbackTrigger {
    ReplayCompletion,
    SessionUpdate,
    ObservationInsert,
    BatchEvent,
}

#[tokio::test]
async fn request_initializes_deadline_and_emits_only_a_session_fact() {
    let fixture = fixture().await;
    let before = routing_counts(&fixture.cp).await;

    let session = fixture
        .cp
        .request_scan_session(fixture.root_id, 30)
        .await
        .unwrap();

    assert_eq!(session.status, ScanSessionStatus::Requested);
    assert_eq!(session.owner_node_id, fixture.node_id);
    assert_eq!(session.progress_deadline_at, T0 + Duration::seconds(30));
    assert_eq!(routing_counts(&fixture.cp).await, before);
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionRequested).await,
        1
    );
}

#[tokio::test]
async fn request_samples_clock_after_acquiring_the_writer_lock() {
    let fixture = fixture().await;
    let holding_tx = crate::cases::begin_immediate_tx(fixture.cp.pool_for_test())
        .await
        .unwrap();
    let waiting_cp = fixture.cp.clone();
    let root_id = fixture.root_id;
    let waiting = tokio::spawn(async move { waiting_cp.request_scan_session(root_id, 30).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    fixture.clock.advance(Duration::seconds(5));
    holding_tx.commit().await.unwrap();

    let requested = waiting.await.unwrap().unwrap();

    assert_eq!(requested.requested_at, T0 + Duration::seconds(5));
    assert_eq!(requested.progress_deadline_at, T0 + Duration::seconds(35));
}

#[tokio::test]
async fn request_rejects_missing_disabled_and_active_root_without_side_effects() {
    let fixture = fixture().await;
    let missing = fixture
        .cp
        .request_scan_session(StorageRootId(4_242), 30)
        .await
        .unwrap_err();
    assert_eq!(missing.error_code(), ErrorCode::NotFound);

    fixture
        .cp
        .set_library_root_enabled(fixture.root_id, false)
        .await
        .unwrap();
    let disabled = fixture
        .cp
        .request_scan_session(fixture.root_id, 30)
        .await
        .unwrap_err();
    assert_eq!(disabled.error_code(), ErrorCode::ConfigInvalid);
    fixture
        .cp
        .set_library_root_enabled(fixture.root_id, true)
        .await
        .unwrap();

    let first = fixture
        .cp
        .request_scan_session(fixture.root_id, 30)
        .await
        .unwrap();
    let conflict = fixture
        .cp
        .request_scan_session(fixture.root_id, 30)
        .await
        .unwrap_err();
    assert_eq!(conflict.error_code(), ErrorCode::Conflict);
    assert!(conflict.to_string().contains(&first.id.to_string()));
    assert_eq!(session_count(&fixture.cp).await, 1);
}

#[tokio::test]
async fn request_fails_closed_for_every_unavailable_or_corrupt_root_shape() {
    for root_case in [
        RootCase::LibraryDisabled,
        RootCase::Unavailable,
        RootCase::Unassigned,
        RootCase::Retired,
        RootCase::OwnerRetired,
    ] {
        let fixture = fixture().await;
        apply_root_case(&fixture, root_case).await;
        let error = fixture
            .cp
            .request_scan_session(fixture.root_id, 30)
            .await
            .unwrap_err();
        assert_eq!(
            error.error_code(),
            ErrorCode::ConfigInvalid,
            "{root_case:?}"
        );
        assert_eq!(session_count(&fixture.cp).await, 0, "{root_case:?}");
    }

    let fixture = fixture().await;
    corrupt_root_epoch(&fixture.cp, fixture.root_id).await;
    let corrupt = fixture
        .cp
        .request_scan_session(fixture.root_id, 30)
        .await
        .unwrap_err();
    assert_eq!(corrupt.error_code(), ErrorCode::DbUnreachable);
    assert_eq!(session_count(&fixture.cp).await, 0);
}

#[tokio::test]
async fn request_recovers_expired_session_before_creating_its_successor() {
    let fixture = fixture().await;
    let expired = fixture
        .cp
        .request_scan_session(fixture.root_id, 10)
        .await
        .unwrap();
    fixture.clock.advance(Duration::seconds(10));

    let successor = fixture
        .cp
        .request_scan_session(fixture.root_id, 10)
        .await
        .unwrap();

    assert_ne!(expired.id, successor.id);
    assert_eq!(
        fixture.cp.scan_session(expired.id).await.unwrap().status,
        ScanSessionStatus::Stale
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStale).await,
        1
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionRequested).await,
        2
    );
}

#[tokio::test]
async fn failed_request_still_commits_the_expired_session_transition() {
    let fixture = fixture().await;
    let expired = fixture
        .cp
        .request_scan_session(fixture.root_id, 10)
        .await
        .unwrap();
    fixture.clock.advance(Duration::seconds(10));
    fixture
        .cp
        .set_library_root_enabled(fixture.root_id, false)
        .await
        .unwrap();

    let error = fixture
        .cp
        .request_scan_session(fixture.root_id, 10)
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        fixture.cp.scan_session(expired.id).await.unwrap().status,
        ScanSessionStatus::Stale
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStale).await,
        1
    );
}

#[tokio::test]
async fn start_binds_authority_captures_high_water_and_replays_exactly() {
    let fixture = fixture().await;
    let location_id = seed_rooted_location(&fixture.cp, fixture.root_id, "old.mkv").await;
    let session = request(&fixture).await;
    let input = start_input(&fixture, session.id, "start-key");

    let first = fixture.cp.start_scan_session(input.clone()).await.unwrap();
    fixture.clock.advance(Duration::seconds(30));
    let replay = fixture.cp.start_scan_session(input).await.unwrap();

    assert_eq!(first, replay);
    assert_eq!(first.status, ScanSessionStatus::Running);
    assert_eq!(first.owner_incarnation_id, fixture.incarnation_id);
    assert_eq!(
        first.location_high_watermark_id.map(|id| id.0),
        Some(location_id)
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStarted).await,
        1
    );
    assert_eq!(
        fixture.cp.scan_session(session.id).await.unwrap().status,
        ScanSessionStatus::Running
    );
    assert_eq!(completed_replay_count(&fixture.cp).await, 2);
}

#[tokio::test]
async fn start_rejects_bad_credentials_before_revealing_the_session() {
    let fixture = fixture().await;
    let mut input = start_input(&fixture, ScanSessionId(u64::MAX), "bad-auth");
    input.token = SecretString::from("wrong-token".to_owned());

    let error = fixture.cp.start_scan_session(input).await.unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::Unauthorized);
    assert_eq!(completed_replay_count(&fixture.cp).await, 1);
}

#[tokio::test]
async fn exact_batch_replays_precede_deadline_and_new_batch_persists_stale_once() {
    let fixture = fixture().await;
    let session = running_session(&fixture, 10).await;
    let first_input = batch_input(&fixture, session.id, 0, "batch-one", 'a');
    let first = fixture
        .cp
        .accept_scan_observation_batch(first_input.clone())
        .await
        .unwrap();
    fixture.clock.advance(Duration::seconds(10));
    let replay = fixture
        .cp
        .accept_scan_observation_batch(first_input)
        .await
        .unwrap();
    let mut ledger_replay = batch_input(&fixture, session.id, 0, "batch-two", 'a');
    ledger_replay.request_hash = "a".repeat(64);
    let replay_by_sequence = fixture
        .cp
        .accept_scan_observation_batch(ledger_replay)
        .await
        .unwrap();

    assert_eq!(first, replay);
    assert_eq!(first, replay_by_sequence);
    let after_replays = fixture.cp.scan_session(session.id).await.unwrap();
    assert_eq!(after_replays.status, ScanSessionStatus::Running);
    assert_eq!(
        after_replays.progress_deadline_at,
        T0 + Duration::seconds(10)
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanObservationBatchAccepted).await,
        1
    );
    let new_error = fixture
        .cp
        .accept_scan_observation_batch(batch_input(&fixture, session.id, 1, "batch-three", 'b'))
        .await
        .unwrap_err();
    assert_eq!(new_error.error_code(), ErrorCode::Conflict);
    let replayed_error = fixture
        .cp
        .accept_scan_observation_batch(batch_input(&fixture, session.id, 1, "batch-three", 'b'))
        .await
        .unwrap_err();
    assert_eq!(replayed_error.error_code(), ErrorCode::Conflict);
    let stored = fixture.cp.scan_session(session.id).await.unwrap();
    assert_eq!(stored.status, ScanSessionStatus::Stale);
    assert_eq!(stored.observation_count, 1);
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStale).await,
        1
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanObservationBatchAccepted).await,
        1
    );
}

#[tokio::test]
async fn deadline_boundary_makes_new_start_failure_and_cancel_stale() {
    let fixture = fixture().await;
    let start_root = fixture.root_id;
    let fail_root = create_root(&fixture.cp, fixture.node_id, "deadline-fail").await;
    let cancel_root = create_root(&fixture.cp, fixture.node_id, "deadline-cancel").await;
    let to_start = fixture
        .cp
        .request_scan_session(start_root, 10)
        .await
        .unwrap();
    let to_fail = fixture
        .cp
        .request_scan_session(fail_root, 10)
        .await
        .unwrap();
    fixture
        .cp
        .start_scan_session(start_input(&fixture, to_fail.id, "deadline-running"))
        .await
        .unwrap();
    let to_cancel = fixture
        .cp
        .request_scan_session(cancel_root, 10)
        .await
        .unwrap();
    fixture.clock.advance(Duration::seconds(10));

    let start_error = fixture
        .cp
        .start_scan_session(start_input(&fixture, to_start.id, "deadline-start"))
        .await
        .unwrap_err();
    let fail_error = fixture
        .cp
        .fail_scan_session(RemoteScanFailInput {
            node_id: fixture.node_id,
            scan_session_id: to_fail.id,
            incarnation_id: fixture.incarnation_id,
            token: fixture.token.clone(),
            idempotency_key: "deadline-fail".to_owned(),
            request_hash: "deadline-fail-route".to_owned(),
            reason: ScanTerminalReason::new("failed at deadline").unwrap(),
        })
        .await
        .unwrap_err();
    let cancel_error = fixture
        .cp
        .cancel_scan_session(
            to_cancel.id,
            ScanTerminalReason::new("cancelled at deadline").unwrap(),
        )
        .await
        .unwrap_err();

    for error in [start_error, fail_error, cancel_error] {
        assert_eq!(error.error_code(), ErrorCode::Conflict);
    }
    for id in [to_start.id, to_fail.id, to_cancel.id] {
        assert_eq!(
            fixture.cp.scan_session(id).await.unwrap().status,
            ScanSessionStatus::Stale
        );
    }
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStale).await,
        3
    );
}

#[tokio::test]
async fn root_epoch_drift_stales_new_batch_without_observations_or_pointer_changes() {
    let fixture = fixture().await;
    let session = running_session(&fixture, 30).await;
    sqlx::query("UPDATE library_roots SET root_epoch = root_epoch + 1 WHERE id = ?")
        .bind(i64::try_from(fixture.root_id.0).unwrap())
        .execute(fixture.cp.pool_for_test())
        .await
        .unwrap();

    let error = fixture
        .cp
        .accept_scan_observation_batch(batch_input(&fixture, session.id, 0, "epoch-drift", 'c'))
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::Conflict);
    assert_eq!(observation_count(&fixture.cp, session.id).await, 0);
    assert_eq!(
        reconciliation_pointer(&fixture.cp, fixture.root_id).await,
        None
    );
    assert_eq!(
        fixture.cp.scan_session(session.id).await.unwrap().status,
        ScanSessionStatus::Stale
    );
}

#[tokio::test]
async fn cross_session_idempotency_key_conflicts_before_second_session_effects() {
    let fixture = fixture().await;
    let second_root = create_root(&fixture.cp, fixture.node_id, "two").await;
    let first = running_session(&fixture, 30).await;
    let second_requested = fixture
        .cp
        .request_scan_session(second_root, 30)
        .await
        .unwrap();
    let second = fixture
        .cp
        .start_scan_session(start_input(&fixture, second_requested.id, "start-two"))
        .await
        .unwrap();
    let shared_key = "shared-batch-key";
    fixture
        .cp
        .accept_scan_observation_batch(batch_input(&fixture, first.id, 0, shared_key, 'a'))
        .await
        .unwrap();

    let error = fixture
        .cp
        .accept_scan_observation_batch(batch_input(
            &fixture,
            second.scan_session_id,
            0,
            shared_key,
            'b',
        ))
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::Conflict);
    assert_eq!(
        observation_count(&fixture.cp, second.scan_session_id).await,
        0
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanObservationBatchAccepted).await,
        1
    );
}

#[tokio::test]
async fn failure_and_cancellation_are_terminal_without_reconciliation() {
    let fixture = fixture().await;
    let running = running_session(&fixture, 30).await;
    let fail_input = RemoteScanFailInput {
        node_id: fixture.node_id,
        scan_session_id: running.id,
        incarnation_id: fixture.incarnation_id,
        token: fixture.token.clone(),
        idempotency_key: "fail-key".to_owned(),
        request_hash: "fail-hash".to_owned(),
        reason: ScanTerminalReason::new("scanner failed").unwrap(),
    };
    let failure = fixture
        .cp
        .fail_scan_session(fail_input.clone())
        .await
        .unwrap();
    assert_eq!(failure.status, ScanSessionStatus::Failed);
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionFailed).await,
        1
    );
    sqlx::query("UPDATE library_roots SET root_epoch = root_epoch + 1 WHERE id = ?")
        .bind(i64::try_from(fixture.root_id.0).unwrap())
        .execute(fixture.cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(
        fixture.cp.fail_scan_session(fail_input).await.unwrap(),
        failure
    );

    let requested = fixture
        .cp
        .request_scan_session(fixture.root_id, 30)
        .await
        .unwrap();
    let cancelled = fixture
        .cp
        .cancel_scan_session(
            requested.id,
            ScanTerminalReason::new("operator cancelled").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status, ScanSessionStatus::Cancelled);
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionCancelled).await,
        1
    );
    assert_eq!(
        reconciliation_pointer(&fixture.cp, fixture.root_id).await,
        None
    );
}

#[tokio::test]
async fn remote_inspection_requires_current_owner_without_idempotency_rows() {
    let fixture = fixture().await;
    let session = running_session(&fixture, 30).await;
    let before = completed_replay_count(&fixture.cp).await;
    let inspected = fixture
        .cp
        .inspect_remote_scan_session(RemoteScanInspectInput {
            node_id: fixture.node_id,
            scan_session_id: session.id,
            incarnation_id: fixture.incarnation_id,
            token: fixture.token.clone(),
        })
        .await
        .unwrap();
    let page = fixture
        .cp
        .inspect_remote_scan_reconciliation(RemoteScanReconciliationInput {
            auth: RemoteScanInspectInput {
                node_id: fixture.node_id,
                scan_session_id: session.id,
                incarnation_id: fixture.incarnation_id,
                token: fixture.token.clone(),
            },
            after_id: None,
            limit: 50,
        })
        .await
        .unwrap_err();

    assert_eq!(inspected.id, session.id);
    assert_eq!(page.error_code(), ErrorCode::Conflict);
    assert_eq!(completed_replay_count(&fixture.cp).await, before);
}

#[tokio::test]
async fn event_abort_rolls_back_request_and_start_replay() {
    let fixture = fixture().await;
    install_abort_trigger(&fixture.cp, "events", "reject_scan_events").await;
    let request_error = fixture
        .cp
        .request_scan_session(fixture.root_id, 30)
        .await
        .unwrap_err();
    assert_eq!(request_error.error_code(), ErrorCode::DbUnreachable);
    assert_eq!(session_count(&fixture.cp).await, 0);
    drop_trigger(&fixture.cp, "reject_scan_events").await;

    let requested = request(&fixture).await;
    install_abort_trigger(&fixture.cp, "events", "reject_scan_start_event").await;
    let start_error = fixture
        .cp
        .start_scan_session(start_input(&fixture, requested.id, "rollback-start"))
        .await
        .unwrap_err();
    assert_eq!(start_error.error_code(), ErrorCode::DbUnreachable);
    assert_eq!(
        fixture.cp.scan_session(requested.id).await.unwrap().status,
        ScanSessionStatus::Requested
    );
    assert_eq!(replay_rows_for_key(&fixture.cp, "rollback-start").await, 0);
}

#[tokio::test]
async fn replay_completion_and_session_update_aborts_roll_back_start() {
    for trigger in [
        RollbackTrigger::ReplayCompletion,
        RollbackTrigger::SessionUpdate,
    ] {
        let fixture = fixture().await;
        let requested = request(&fixture).await;
        install_start_rollback_trigger(&fixture.cp, trigger).await;

        let error = fixture
            .cp
            .start_scan_session(start_input(&fixture, requested.id, "rollback-mutator"))
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::DbUnreachable, "{trigger:?}");
        assert_eq!(
            fixture.cp.scan_session(requested.id).await.unwrap().status,
            ScanSessionStatus::Requested
        );
        assert_eq!(
            event_count(&fixture.cp, EventKind::ScanSessionStarted).await,
            0
        );
        assert_eq!(
            replay_rows_for_key(&fixture.cp, "rollback-mutator").await,
            0
        );
    }
}

#[tokio::test]
async fn observation_and_batch_event_aborts_roll_back_the_whole_batch() {
    for trigger in [
        RollbackTrigger::ObservationInsert,
        RollbackTrigger::BatchEvent,
    ] {
        let fixture = fixture().await;
        let session = running_session(&fixture, 30).await;
        install_batch_rollback_trigger(&fixture.cp, trigger).await;

        let error = fixture
            .cp
            .accept_scan_observation_batch(batch_input(
                &fixture,
                session.id,
                0,
                "rollback-batch",
                'd',
            ))
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::DbUnreachable, "{trigger:?}");
        let stored = fixture.cp.scan_session(session.id).await.unwrap();
        assert_eq!(stored.next_sequence, 0);
        assert_eq!(stored.observation_count, 0);
        assert_eq!(observation_count(&fixture.cp, session.id).await, 0);
        assert_eq!(
            event_count(&fixture.cp, EventKind::ScanObservationBatchAccepted).await,
            0
        );
        assert_eq!(replay_rows_for_key(&fixture.cp, "rollback-batch").await, 0);
    }
}

#[test]
fn replay_outcomes_reject_unknown_fields() {
    let incarnation_id = INCARNATION.parse().unwrap();
    assert_unknown_rejected(RemoteScanStartOutcome {
        scan_session_id: ScanSessionId(1),
        status: ScanSessionStatus::Running,
        owner_incarnation_id: incarnation_id,
        location_high_watermark_id: None,
        progress_deadline_at: T0,
    });
    assert_unknown_rejected(RemoteScanBatchOutcome {
        scan_session_id: ScanSessionId(1),
        sequence: 0,
        accepted_observation_count: 1,
        cumulative_observation_count: 1,
    });
    assert_unknown_rejected(RemoteScanTerminalOutcome {
        scan_session_id: ScanSessionId(1),
        status: ScanSessionStatus::Failed,
        terminal_at: T0,
        terminal_reason: ScanTerminalReason::new("failed").unwrap(),
    });
}

async fn fixture() -> Fixture {
    let database = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", database.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(T0));
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        clock.clone(),
        Arc::new(Mutex::new(FrozenRng::new(419))),
    )
    .await
    .unwrap();
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "scan-owner".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let incarnation_id = INCARNATION.parse().unwrap();
    cp.remote_activate(RemoteActivateInput {
        node_id: registered.node.id,
        token: registered.token.clone(),
        idempotency_key: "activate-scan-owner".to_owned(),
        request_hash: "activate-scan-owner-body".to_owned(),
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
    let root_id = create_root(&cp, registered.node.id, "one").await;
    Fixture {
        cp,
        clock,
        token: registered.token,
        node_id: registered.node.id,
        incarnation_id,
        root_id,
        _database: database,
    }
}

async fn create_root(cp: &crate::ControlPlane, owner: NodeId, suffix: &str) -> StorageRootId {
    let library = cp
        .create_library(NewLibrary {
            slug: format!("scan-{suffix}"),
            display_name: format!("Scan {suffix}"),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = cp
        .create_library_root(new_root(library.id, owner, suffix))
        .await
        .unwrap();
    cp.activate_library_root(root.id, format!("scan-{suffix}"))
        .await
        .unwrap();
    root.id
}

fn new_root(library_id: LibraryId, owner: NodeId, suffix: &str) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id: owner,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(format!("/scan/{suffix}")).unwrap(),
        display_locator: format!("/scan/{suffix}"),
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

async fn request(fixture: &Fixture) -> voom_store::repo::scan::sessions::ScanSession {
    fixture
        .cp
        .request_scan_session(fixture.root_id, 30)
        .await
        .unwrap()
}

async fn running_session(
    fixture: &Fixture,
    idle_timeout_seconds: u32,
) -> voom_store::repo::scan::sessions::ScanSession {
    let requested = fixture
        .cp
        .request_scan_session(fixture.root_id, idle_timeout_seconds)
        .await
        .unwrap();
    fixture
        .cp
        .start_scan_session(start_input(fixture, requested.id, "start-running"))
        .await
        .unwrap();
    fixture.cp.scan_session(requested.id).await.unwrap()
}

fn start_input(fixture: &Fixture, id: ScanSessionId, key: &str) -> RemoteScanStartInput {
    RemoteScanStartInput {
        node_id: fixture.node_id,
        scan_session_id: id,
        incarnation_id: fixture.incarnation_id,
        token: fixture.token.clone(),
        idempotency_key: key.to_owned(),
        request_hash: format!("{key}-route-instance"),
    }
}

fn batch_input(
    fixture: &Fixture,
    id: ScanSessionId,
    sequence: u64,
    key: &str,
    hash: char,
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
            provider_relative_locator: ProviderRelativeLocator::new(format!(
                "batch/{sequence}-{hash}.mkv"
            ))
            .unwrap(),
            provider_object_identity: format!("object-{sequence}-{hash}"),
            size_bytes: 1,
            modified_at: T0,
            stability_started_at: T0,
            stability_confirmed_at: T0,
        }],
    }
}

async fn seed_rooted_location(
    cp: &crate::ControlPlane,
    root_id: StorageRootId,
    locator: &str,
) -> u64 {
    let asset = sqlx::query("INSERT INTO file_assets (created_at, epoch) VALUES (?, 0)")
        .bind("1970-01-01T00:00:00Z")
        .execute(cp.pool_for_test())
        .await
        .unwrap()
        .last_insert_rowid();
    let version = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at, retired_at, epoch) \
         VALUES (?, 'scan-hash', 1, 'ingest', NULL, ?, NULL, 0)",
    )
    .bind(asset)
    .bind("1970-01-01T00:00:00Z")
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    let location = sqlx::query(
        "INSERT INTO file_locations (file_version_id, address_state, storage_root_id, \
         provider_relative_locator, observed_at, epoch) VALUES (?, 'rooted', ?, ?, ?, 0)",
    )
    .bind(version)
    .bind(i64::try_from(root_id.0).unwrap())
    .bind(locator)
    .bind("1970-01-01T00:00:00Z")
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    u64::try_from(location).unwrap()
}

async fn apply_root_case(fixture: &Fixture, root_case: RootCase) {
    match root_case {
        RootCase::LibraryDisabled => {
            let library_id: i64 =
                sqlx::query_scalar("SELECT library_id FROM library_roots WHERE id = ?")
                    .bind(i64::try_from(fixture.root_id.0).unwrap())
                    .fetch_one(fixture.cp.pool_for_test())
                    .await
                    .unwrap();
            sqlx::query("UPDATE libraries SET enabled = 0 WHERE id = ?")
                .bind(library_id)
                .execute(fixture.cp.pool_for_test())
                .await
                .unwrap();
        }
        RootCase::Unavailable => {
            sqlx::query("UPDATE library_roots SET state = 'unavailable' WHERE id = ?")
                .bind(i64::try_from(fixture.root_id.0).unwrap())
                .execute(fixture.cp.pool_for_test())
                .await
                .unwrap();
        }
        RootCase::Unassigned => {
            sqlx::query(
                "UPDATE library_roots SET state = 'unassigned', owner_node_id = NULL, \
                 activation_identity = NULL WHERE id = ?",
            )
            .bind(i64::try_from(fixture.root_id.0).unwrap())
            .execute(fixture.cp.pool_for_test())
            .await
            .unwrap();
        }
        RootCase::Retired => {
            sqlx::query(
                "UPDATE library_roots SET state = 'retired', enabled = 0, \
                 activation_identity = NULL WHERE id = ?",
            )
            .bind(i64::try_from(fixture.root_id.0).unwrap())
            .execute(fixture.cp.pool_for_test())
            .await
            .unwrap();
        }
        RootCase::OwnerRetired => {
            sqlx::query("UPDATE nodes SET status = 'retired', retired_at = ? WHERE id = ?")
                .bind("1970-01-01T00:00:01Z")
                .bind(i64::try_from(fixture.node_id.0).unwrap())
                .execute(fixture.cp.pool_for_test())
                .await
                .unwrap();
        }
    }
}

async fn corrupt_root_epoch(cp: &crate::ControlPlane, root_id: StorageRootId) {
    let mut connection = cp.pool_for_test().acquire().await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE library_roots SET root_epoch = -1 WHERE id = ?")
        .bind(i64::try_from(root_id.0).unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
}

async fn install_start_rollback_trigger(cp: &crate::ControlPlane, trigger: RollbackTrigger) {
    let sql = match trigger {
        RollbackTrigger::ReplayCompletion => {
            "CREATE TRIGGER reject_scan_replay_completion \
             BEFORE UPDATE OF status ON remote_idempotency_keys \
             WHEN NEW.status = 'completed' \
             BEGIN SELECT RAISE(ABORT, 'forced replay completion rollback'); END"
        }
        RollbackTrigger::SessionUpdate => {
            "CREATE TRIGGER reject_scan_session_update \
             BEFORE UPDATE OF status ON scan_sessions \
             BEGIN SELECT RAISE(ABORT, 'forced session rollback'); END"
        }
        RollbackTrigger::ObservationInsert | RollbackTrigger::BatchEvent => {
            panic!("invalid start rollback trigger")
        }
    };
    sqlx::query(sql).execute(cp.pool_for_test()).await.unwrap();
}

async fn install_batch_rollback_trigger(cp: &crate::ControlPlane, trigger: RollbackTrigger) {
    let sql = match trigger {
        RollbackTrigger::ObservationInsert => {
            "CREATE TRIGGER reject_scan_observation \
             BEFORE INSERT ON scan_observations \
             BEGIN SELECT RAISE(ABORT, 'forced observation rollback'); END"
        }
        RollbackTrigger::BatchEvent => {
            "CREATE TRIGGER reject_scan_batch_event \
             BEFORE INSERT ON events \
             WHEN NEW.kind = 'scan_session.observation_batch_accepted' \
             BEGIN SELECT RAISE(ABORT, 'forced batch event rollback'); END"
        }
        RollbackTrigger::ReplayCompletion | RollbackTrigger::SessionUpdate => {
            panic!("invalid batch rollback trigger")
        }
    };
    sqlx::query(sql).execute(cp.pool_for_test()).await.unwrap();
}

async fn routing_counts(cp: &crate::ControlPlane) -> (i64, i64) {
    let tickets = sqlx::query_scalar("SELECT COUNT(*) FROM tickets")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let leases = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    (tickets, leases)
}

async fn event_count(cp: &crate::ControlPlane, kind: EventKind) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?")
        .bind(kind.as_str())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn session_count(cp: &crate::ControlPlane) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM scan_sessions")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn observation_count(cp: &crate::ControlPlane, id: ScanSessionId) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM scan_observations WHERE scan_session_id = ?")
        .bind(i64::try_from(id.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn completed_replay_count(cp: &crate::ControlPlane) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM remote_idempotency_keys WHERE status = 'completed'")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn replay_rows_for_key(cp: &crate::ControlPlane, key: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM remote_idempotency_keys WHERE idempotency_key LIKE ?")
        .bind(format!("%:{key}"))
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn reconciliation_pointer(cp: &crate::ControlPlane, root: StorageRootId) -> Option<i64> {
    sqlx::query_scalar("SELECT last_scan_session_id FROM library_roots WHERE id = ?")
        .bind(i64::try_from(root.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn install_abort_trigger(cp: &crate::ControlPlane, table: &str, name: &str) {
    sqlx::query(&format!(
        "CREATE TRIGGER {name} BEFORE INSERT ON {table} \
         BEGIN SELECT RAISE(ABORT, 'forced rollback'); END"
    ))
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

async fn drop_trigger(cp: &crate::ControlPlane, name: &str) {
    sqlx::query(&format!("DROP TRIGGER {name}"))
        .execute(cp.pool_for_test())
        .await
        .unwrap();
}

fn assert_unknown_rejected<T>(value: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let mut json = serde_json::to_value(value).unwrap();
    json["unexpected"] = json!(true);
    assert!(serde_json::from_value::<T>(json).is_err());
}
