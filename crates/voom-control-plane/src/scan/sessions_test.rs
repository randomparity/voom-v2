use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use secrecy::SecretString;
use serde_json::json;
use sqlx::ConnectOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use time::{Duration, OffsetDateTime};
use tokio::sync::Notify;
use voom_core::{
    ArtifactAccessMode, ErrorCode, LibraryId, NodeId, NodeIncarnationId, OperationKind,
    ProviderLocator, ProviderRelativeLocator, ScanSessionId, ScanSessionStatus, ScanTerminalReason,
    StorageProviderKind, StorageRootId, clock_test_support::ManualClock,
    rng_test_support::FrozenRng,
};
use voom_events::EventKind;
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::execution::remote_idempotency::RemoteMutationReplay;
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_store::repo::scan::sessions::{ScanObservation, ScanSession};
use voom_store::test_support::with_check_constraints_disabled;

use super::{
    RemoteScanBatchInput, RemoteScanBatchOutcome, RemoteScanCompleteInput,
    RemoteScanCompleteOutcome, RemoteScanFailInput, RemoteScanInspectInput,
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
    database: voom_test_support::TempDatabase,
}

struct ReconciliationFence {
    cp: crate::ControlPlane,
    barrier_pool: sqlx::SqlitePool,
    armed: Arc<AtomicBool>,
    releases: Arc<AtomicUsize>,
    fenced: Arc<Notify>,
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

#[derive(Debug, Clone, Copy)]
enum TerminalRollbackTrigger {
    EventInsert,
    ReplayCompletion,
    SessionUpdate,
}

#[derive(Debug, Clone, Copy)]
enum CancelRollbackTrigger {
    EventInsert,
    SessionUpdate,
}

#[derive(Debug, Clone, Copy)]
enum CompleteRollbackTrigger {
    LocationUpdate,
    SessionUpdate,
    EventInsert,
    ReplayCompletion,
}

#[derive(Debug, Clone, Copy)]
enum StartRootFence {
    EpochDrift,
    Unavailable,
}

#[derive(Debug, Clone, Copy)]
enum BatchLinkCorruption {
    MissingPredecessor,
    PredecessorCumulative,
}

#[derive(Debug)]
struct LifecycleSnapshot {
    session: ScanSession,
    observations: i64,
    events: i64,
    replays: i64,
    routing: (i64, i64),
    reconciliation_pointer: Option<i64>,
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
async fn start_rejects_non_current_incarnation_before_session_or_replay_effects() {
    let fixture = fixture().await;
    let requested = request(&fixture).await;
    let before = lifecycle_snapshot(&fixture, requested.id).await;
    let mut input = start_input(&fixture, requested.id, "non-current-incarnation");
    input.incarnation_id = "fedcba9876543210fedcba9876543210".parse().unwrap();

    let error = fixture.cp.start_scan_session(input).await.unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::Conflict);
    assert_lifecycle_unchanged(&fixture, requested.id, &before).await;
    assert_eq!(
        replay_rows_for_key(&fixture.cp, "non-current-incarnation").await,
        0
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStarted).await,
        0
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStale).await,
        0
    );
}

#[tokio::test]
async fn start_epoch_drift_persists_only_the_stale_conflict_replay() {
    assert_start_root_fence(StartRootFence::EpochDrift).await;
}

#[tokio::test]
async fn start_unavailable_root_persists_only_the_stale_conflict_replay() {
    assert_start_root_fence(StartRootFence::Unavailable).await;
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
async fn scan_session_capacity_replays_the_limit_rejects_crossing_and_preserves_deadline_order() {
    let fixture = fixture().await;
    let session = running_session(&fixture, 10).await;
    seed_capacity_prefix(&fixture.cp, session.id, 99_999).await;
    let last = batch_input(&fixture, session.id, 100, "capacity-last", 'a');
    let accepted = fixture
        .cp
        .accept_scan_observation_batch(last.clone())
        .await
        .unwrap();
    assert_eq!(accepted.cumulative_observation_count, 100_000);
    assert_eq!(
        fixture
            .cp
            .accept_scan_observation_batch(last)
            .await
            .unwrap(),
        accepted
    );

    let before = fixture.cp.scan_session(session.id).await.unwrap();
    let before_events = event_count(&fixture.cp, EventKind::ScanObservationBatchAccepted).await;
    let crossing = batch_input(&fixture, session.id, 101, "capacity-crossing", 'b');
    let error = fixture
        .cp
        .accept_scan_observation_batch(crossing.clone())
        .await
        .unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::Conflict);
    let message = error.to_string();
    assert!(message.contains("maximum 100000"));
    assert!(message.contains("current 100000"));
    assert!(message.contains("incoming 1"));
    let replay = fixture
        .cp
        .accept_scan_observation_batch(crossing)
        .await
        .unwrap_err();
    assert_eq!(replay.to_string(), error.to_string());
    assert_eq!(fixture.cp.scan_session(session.id).await.unwrap(), before);
    assert_eq!(observation_count(&fixture.cp, session.id).await, 100_000);
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanObservationBatchAccepted).await,
        before_events
    );
    assert_conflict_replay(&fixture.cp, "capacity-crossing").await;

    assert_expired_capacity_crossing_stales_once(&fixture).await;
}

async fn assert_expired_capacity_crossing_stales_once(fixture: &Fixture) {
    let expired_root = create_root(&fixture.cp, fixture.node_id, "capacity-expired").await;
    let requested = fixture
        .cp
        .request_scan_session(expired_root, 10)
        .await
        .unwrap();
    fixture
        .cp
        .start_scan_session(start_input(fixture, requested.id, "capacity-expired-start"))
        .await
        .unwrap();
    seed_capacity_prefix(&fixture.cp, requested.id, 100_000).await;
    fixture.clock.advance(Duration::seconds(10));
    let expired = batch_input(fixture, requested.id, 100, "capacity-expired-crossing", 'c');
    let stale = fixture
        .cp
        .accept_scan_observation_batch(expired.clone())
        .await
        .unwrap_err();
    assert_eq!(stale.error_code(), ErrorCode::Conflict);
    assert!(stale.to_string().contains("marked stale"));
    assert!(!stale.to_string().contains("maximum 100000"));
    let replayed = fixture
        .cp
        .accept_scan_observation_batch(expired)
        .await
        .unwrap_err();
    assert_eq!(replayed.to_string(), stale.to_string());
    assert_eq!(
        fixture.cp.scan_session(requested.id).await.unwrap().status,
        ScanSessionStatus::Stale
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStale).await,
        1
    );
}

async fn seed_capacity_prefix(
    cp: &crate::ControlPlane,
    session_id: ScanSessionId,
    observation_total: i64,
) {
    let session_id = i64::try_from(session_id.0).unwrap();
    let final_batch_count = observation_total - 99_000;
    sqlx::query("DROP TRIGGER IF EXISTS scan_observation_batches_validate_parent_frontier")
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query(
        "WITH RECURSIVE numbers(value) AS (\
             SELECT 0 UNION ALL SELECT value + 1 FROM numbers WHERE value < 99\
         )\
         INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
             request_hash, observation_count, accepted_at, cumulative_observation_count)\
         SELECT ?, value, CASE WHEN value = 0 THEN NULL ELSE value - 1 END, \
             printf('%064x', value), CASE WHEN value < 99 THEN 1000 ELSE ? END, \
             '1970-01-01T00:00:00Z', \
             CASE WHEN value < 99 THEN (value + 1) * 1000 ELSE ? END \
         FROM numbers ORDER BY value ASC",
    )
    .bind(session_id)
    .bind(final_batch_count)
    .bind(observation_total)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query(
        "WITH RECURSIVE numbers(value) AS (\
             SELECT 0 UNION ALL SELECT value + 1 FROM numbers WHERE value + 1 < ?\
         )\
         INSERT INTO scan_observations (scan_session_id, batch_sequence, ordinal, \
             provider_relative_locator, provider_object_identity, size_bytes, modified_at, \
             stability_started_at, stability_confirmed_at)\
         SELECT ?, value / 1000, value % 1000, 'capacity/' || value, 'object-' || value, 1, \
             '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
             '1970-01-01T00:00:00Z' FROM numbers ORDER BY value ASC",
    )
    .bind(observation_total)
    .bind(session_id)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = 100, batch_count = 100, \
         observation_count = ? WHERE id = ?",
    )
    .bind(observation_total)
    .bind(session_id)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

#[tokio::test]
async fn corrupt_batch_progress_is_database_and_repair_allows_the_same_key() {
    let fixture = fixture().await;
    let running = running_session(&fixture, 30).await;
    fixture
        .cp
        .accept_scan_observation_batch(batch_input(
            &fixture,
            running.id,
            0,
            "corrupt-progress-baseline",
            'a',
        ))
        .await
        .unwrap();
    let before_events = total_event_count(&fixture.cp).await;
    let session_id = i64::try_from(running.id.0).unwrap();
    with_check_constraints_disabled(fixture.cp.pool_for_test(), move |connection| {
        Box::pin(async move {
            sqlx::query(
                "UPDATE scan_sessions SET next_sequence = 2, batch_count = 1, \
                 observation_count = 1 WHERE id = ?",
            )
            .bind(session_id)
            .execute(connection)
            .await
        })
    })
    .await
    .unwrap();
    let input = batch_input(&fixture, running.id, 1, "repairable-corruption", 'b');

    let incoherent = fixture
        .cp
        .accept_scan_observation_batch(input.clone())
        .await
        .unwrap_err();
    assert!(matches!(incoherent, voom_core::VoomError::Database { .. }));
    assert_eq!(
        replay_rows_for_key(&fixture.cp, "repairable-corruption").await,
        0
    );
    assert_eq!(total_event_count(&fixture.cp).await, before_events);
    assert_eq!(observation_count(&fixture.cp, running.id).await, 1);
    assert_eq!(scan_progress(&fixture.cp, running.id).await, (2, 1, 1));

    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = 2, batch_count = 2, \
         observation_count = 2 WHERE id = ?",
    )
    .bind(session_id)
    .execute(fixture.cp.pool_for_test())
    .await
    .unwrap();
    let missing_ledger = fixture
        .cp
        .accept_scan_observation_batch(input.clone())
        .await
        .unwrap_err();
    assert!(matches!(
        missing_ledger,
        voom_core::VoomError::Database { .. }
    ));
    assert_eq!(
        replay_rows_for_key(&fixture.cp, "repairable-corruption").await,
        0
    );
    assert_eq!(total_event_count(&fixture.cp).await, before_events);
    assert_eq!(observation_count(&fixture.cp, running.id).await, 1);
    assert_eq!(scan_progress(&fixture.cp, running.id).await, (2, 2, 2));

    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = 1, batch_count = 1, \
         observation_count = 1 WHERE id = ?",
    )
    .bind(session_id)
    .execute(fixture.cp.pool_for_test())
    .await
    .unwrap();
    let accepted = fixture
        .cp
        .accept_scan_observation_batch(input)
        .await
        .unwrap();
    assert_eq!(accepted.sequence, 1);
    assert_eq!(accepted.cumulative_observation_count, 2);
    assert_eq!(
        replay_rows_for_key(&fixture.cp, "repairable-corruption").await,
        1
    );
    assert_eq!(total_event_count(&fixture.cp).await, before_events + 1);
    assert_eq!(observation_count(&fixture.cp, running.id).await, 2);
}

#[tokio::test]
async fn public_new_and_replayed_batches_reject_broken_links_without_side_effects() {
    for case in [
        BatchLinkCorruption::MissingPredecessor,
        BatchLinkCorruption::PredecessorCumulative,
    ] {
        assert_public_batch_link_corruption(case).await;
    }
}

async fn assert_public_batch_link_corruption(case: BatchLinkCorruption) {
    let fixture = fixture().await;
    let running = running_session(&fixture, 30).await;
    let first = batch_input(&fixture, running.id, 0, "link-first", 'a');
    let second = batch_input(&fixture, running.id, 1, "link-second", 'b');
    fixture
        .cp
        .accept_scan_observation_batch(first)
        .await
        .unwrap();
    fixture
        .cp
        .accept_scan_observation_batch(second)
        .await
        .unwrap();
    corrupt_public_batch_link(&fixture.cp, running.id, case).await;
    let before = lifecycle_snapshot(&fixture, running.id).await;

    let replay = batch_input(&fixture, running.id, 1, "broken-link-replay", 'b');
    let new = batch_input(&fixture, running.id, 2, "broken-link-new", 'c');
    for input in [replay.clone(), new.clone()] {
        let key = input.idempotency_key.clone();
        let error = fixture
            .cp
            .accept_scan_observation_batch(input)
            .await
            .unwrap_err();
        assert!(matches!(error, voom_core::VoomError::Database { .. }));
        assert_eq!(replay_rows_for_key(&fixture.cp, &key).await, 0);
        assert_lifecycle_unchanged(&fixture, running.id, &before).await;
    }

    repair_public_batch_link(&fixture.cp, running.id, case).await;
    assert_eq!(
        fixture
            .cp
            .accept_scan_observation_batch(replay)
            .await
            .unwrap()
            .sequence,
        1
    );
    assert_eq!(
        fixture
            .cp
            .accept_scan_observation_batch(new)
            .await
            .unwrap()
            .sequence,
        2
    );
}

#[tokio::test]
async fn public_mutations_reject_cross_node_incarnation_but_completed_replay_stays_first() {
    let mutation_fixture = fixture().await;
    let running = running_session(&mutation_fixture, 30).await;
    let other_incarnation = seed_other_scan_incarnation(&mutation_fixture.cp).await;
    corrupt_public_owner_incarnation(&mutation_fixture.cp, running.id, &other_incarnation).await;
    let before = lifecycle_snapshot(&mutation_fixture, running.id).await;

    let batch = batch_input(&mutation_fixture, running.id, 0, "cross-node-batch", 'a');
    let batch_error = mutation_fixture
        .cp
        .accept_scan_observation_batch(batch)
        .await
        .unwrap_err();
    assert!(matches!(batch_error, voom_core::VoomError::Database { .. }));
    let fail_error = mutation_fixture
        .cp
        .fail_scan_session(fail_input(&mutation_fixture, running.id, "cross-node-fail"))
        .await
        .unwrap_err();
    assert!(matches!(fail_error, voom_core::VoomError::Database { .. }));
    let complete_error = mutation_fixture
        .cp
        .complete_scan_session(complete_input(
            &mutation_fixture,
            running.id,
            "cross-node-complete",
            None,
            0,
        ))
        .await
        .unwrap_err();
    assert!(matches!(
        complete_error,
        voom_core::VoomError::Database { .. }
    ));
    for key in ["cross-node-batch", "cross-node-fail", "cross-node-complete"] {
        assert_eq!(replay_rows_for_key(&mutation_fixture.cp, key).await, 0);
    }
    assert_lifecycle_unchanged(&mutation_fixture, running.id, &before).await;

    let replay_fixture = fixture().await;
    let completed = running_session(&replay_fixture, 30).await;
    let input = complete_input(
        &replay_fixture,
        completed.id,
        "completed-before-corruption",
        None,
        0,
    );
    let outcome = replay_fixture
        .cp
        .complete_scan_session(input.clone())
        .await
        .unwrap();
    let other_incarnation = seed_other_scan_incarnation(&replay_fixture.cp).await;
    corrupt_public_owner_incarnation(&replay_fixture.cp, completed.id, &other_incarnation).await;
    assert_eq!(
        replay_fixture
            .cp
            .complete_scan_session(input)
            .await
            .unwrap(),
        outcome
    );
}

async fn seed_other_scan_incarnation(cp: &crate::ControlPlane) -> String {
    let node_id = seed_alternate_owner(cp).await;
    let incarnation = "78787878787878787878787878787878";
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES (?, ?, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(incarnation)
    .bind(i64::try_from(node_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    incarnation.to_owned()
}

async fn corrupt_public_owner_incarnation(
    cp: &crate::ControlPlane,
    session_id: ScanSessionId,
    incarnation: &str,
) {
    let mut connection = cp.pool_for_test().acquire().await.unwrap();
    connection.close_on_drop();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE scan_sessions SET owner_incarnation_id = ? WHERE id = ?")
        .bind(incarnation)
        .bind(i64::try_from(session_id.0).unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
}

async fn corrupt_public_batch_link(
    cp: &crate::ControlPlane,
    session_id: ScanSessionId,
    case: BatchLinkCorruption,
) {
    let session_id = i64::try_from(session_id.0).unwrap();
    match case {
        BatchLinkCorruption::MissingPredecessor => {
            sqlx::query("DROP TRIGGER scan_observation_batches_no_delete")
                .execute(cp.pool_for_test())
                .await
                .unwrap();
            let mut connection = cp.pool_for_test().acquire().await.unwrap();
            connection.close_on_drop();
            sqlx::query("PRAGMA foreign_keys = OFF")
                .execute(&mut *connection)
                .await
                .unwrap();
            sqlx::query(
                "DELETE FROM scan_observation_batches WHERE scan_session_id = ? AND sequence = 0",
            )
            .bind(session_id)
            .execute(&mut *connection)
            .await
            .unwrap();
            connection.close().await.unwrap();
        }
        BatchLinkCorruption::PredecessorCumulative => {
            sqlx::query("DROP TRIGGER scan_observation_batches_no_update")
                .execute(cp.pool_for_test())
                .await
                .unwrap();
            sqlx::query(
                "UPDATE scan_observation_batches SET cumulative_observation_count = 2 \
                 WHERE scan_session_id = ? AND sequence = 0",
            )
            .bind(session_id)
            .execute(cp.pool_for_test())
            .await
            .unwrap();
        }
    }
}

async fn repair_public_batch_link(
    cp: &crate::ControlPlane,
    session_id: ScanSessionId,
    case: BatchLinkCorruption,
) {
    let session_id = i64::try_from(session_id.0).unwrap();
    match case {
        BatchLinkCorruption::MissingPredecessor => {
            sqlx::query("DROP TRIGGER IF EXISTS scan_observation_batches_validate_parent_frontier")
                .execute(cp.pool_for_test())
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO scan_observation_batches (scan_session_id, sequence, \
                 previous_sequence, request_hash, observation_count, accepted_at, \
                 cumulative_observation_count) VALUES (?, 0, NULL, ?, 1, \
                 '1970-01-01T00:00:00Z', 1)",
            )
            .bind(session_id)
            .bind("a".repeat(64))
            .execute(cp.pool_for_test())
            .await
            .unwrap();
        }
        BatchLinkCorruption::PredecessorCumulative => {
            sqlx::query(
                "UPDATE scan_observation_batches SET cumulative_observation_count = 1 \
                 WHERE scan_session_id = ? AND sequence = 0",
            )
            .bind(session_id)
            .execute(cp.pool_for_test())
            .await
            .unwrap();
        }
    }
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
async fn reconciliation_keeps_authorization_and_evidence_in_one_transaction() {
    let fixture = fixture().await;
    seed_rooted_location(&fixture.cp, fixture.root_id, "fenced-evidence.mkv").await;
    let running = running_session(&fixture, 30).await;
    fixture
        .cp
        .complete_scan_session(complete_input(
            &fixture,
            running.id,
            "complete-fenced-evidence",
            None,
            0,
        ))
        .await
        .unwrap();

    let fence = reconciliation_fence(&fixture).await;
    let fence_completed = fence.fenced.notified();
    fence.armed.store(true, Ordering::SeqCst);

    let page = fence
        .cp
        .inspect_remote_scan_reconciliation(RemoteScanReconciliationInput {
            auth: RemoteScanInspectInput {
                node_id: fixture.node_id,
                scan_session_id: running.id,
                incarnation_id: fixture.incarnation_id,
                token: fixture.token.clone(),
            },
            after_id: None,
            limit: 50,
        })
        .await
        .unwrap();
    let post_request_barrier = fence.barrier_pool.acquire().await.unwrap();
    fence.armed.store(false, Ordering::SeqCst);
    drop(post_request_barrier);
    fence_completed.await;

    assert_eq!(page.items.len(), 1);
    assert_eq!(fence.releases.load(Ordering::SeqCst), 1);
}

async fn reconciliation_fence(fixture: &Fixture) -> ReconciliationFence {
    let armed = Arc::new(AtomicBool::new(false));
    let releases = Arc::new(AtomicUsize::new(0));
    let fenced = Arc::new(Notify::new());
    let released = Arc::new(Notify::new());
    let all_releases = Arc::new(AtomicUsize::new(0));
    let node_id = i64::try_from(fixture.node_id.0).unwrap();
    let hook_armed = Arc::clone(&armed);
    let hook_releases = Arc::clone(&releases);
    let hook_fenced = Arc::clone(&fenced);
    let hook_released = Arc::clone(&released);
    let hook_all_releases = Arc::clone(&all_releases);
    let url = format!("sqlite://{}", fixture.database.path().display());
    let options = url
        .parse::<SqliteConnectOptions>()
        .unwrap()
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(30))
        .disable_statement_logging();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .after_release(move |connection, _metadata| {
            let hook_armed = Arc::clone(&hook_armed);
            let hook_releases = Arc::clone(&hook_releases);
            let hook_fenced = Arc::clone(&hook_fenced);
            let hook_released = Arc::clone(&hook_released);
            let hook_all_releases = Arc::clone(&hook_all_releases);
            Box::pin(async move {
                if hook_armed.load(Ordering::SeqCst) {
                    let release = hook_releases.fetch_add(1, Ordering::SeqCst);
                    if release == 0 {
                        supersede_incarnation(connection, node_id).await?;
                        hook_fenced.notify_one();
                    }
                }
                hook_all_releases.fetch_add(1, Ordering::SeqCst);
                hook_released.notify_one();
                Ok(true)
            })
        })
        .connect_with(options)
        .await
        .unwrap();
    let barrier_pool = pool.clone();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        fixture.clock.clone(),
        Arc::new(Mutex::new(FrozenRng::new(420))),
    )
    .await
    .unwrap();
    drain_release_hooks(&barrier_pool, &all_releases, &released).await;
    assert_current_incarnation(fixture, node_id).await;
    assert_eq!(releases.load(Ordering::SeqCst), 0);
    ReconciliationFence {
        cp,
        barrier_pool,
        armed,
        releases,
        fenced,
    }
}

async fn supersede_incarnation(
    connection: &mut sqlx::SqliteConnection,
    node_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE nodes SET active_incarnation_id = NULL WHERE id = ?")
        .bind(node_id)
        .execute(&mut *connection)
        .await?;
    sqlx::query(
        "UPDATE node_incarnations SET status = 'superseded', ended_at = ?, \
         end_reason = 'superseded' WHERE incarnation_id = ?",
    )
    .bind("1970-01-01T00:00:01Z")
    .bind(INCARNATION)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

async fn drain_release_hooks(
    pool: &sqlx::SqlitePool,
    all_releases: &AtomicUsize,
    released: &Notify,
) {
    let connection = pool.acquire().await.unwrap();
    let before = all_releases.load(Ordering::SeqCst);
    drop(connection);
    while all_releases.load(Ordering::SeqCst) == before {
        released.notified().await;
    }
}

async fn assert_current_incarnation(fixture: &Fixture, node_id: i64) {
    let current: Option<String> =
        sqlx::query_scalar("SELECT active_incarnation_id FROM nodes WHERE id = ?")
            .bind(node_id)
            .fetch_one(fixture.cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(current.as_deref(), Some(INCARNATION));
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

#[tokio::test]
async fn failure_abort_points_roll_back_session_observations_replay_and_event() {
    for trigger in [
        TerminalRollbackTrigger::EventInsert,
        TerminalRollbackTrigger::ReplayCompletion,
        TerminalRollbackTrigger::SessionUpdate,
    ] {
        let fixture = fixture().await;
        let session = running_session_with_observation(&fixture).await;
        let before = lifecycle_snapshot(&fixture, session.id).await;
        install_failure_rollback_trigger(&fixture.cp, trigger).await;

        let error = fixture
            .cp
            .fail_scan_session(fail_input(&fixture, session.id, "rollback-failure"))
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::DbUnreachable, "{trigger:?}");
        assert_lifecycle_unchanged(&fixture, session.id, &before).await;
        assert_eq!(
            event_count(&fixture.cp, EventKind::ScanSessionFailed).await,
            0,
            "{trigger:?}"
        );
        assert_eq!(
            replay_rows_for_key(&fixture.cp, "rollback-failure").await,
            0,
            "{trigger:?}"
        );
    }
}

#[tokio::test]
async fn cancellation_abort_points_roll_back_session_observations_and_event() {
    for trigger in [
        CancelRollbackTrigger::EventInsert,
        CancelRollbackTrigger::SessionUpdate,
    ] {
        let fixture = fixture().await;
        let session = running_session_with_observation(&fixture).await;
        let before = lifecycle_snapshot(&fixture, session.id).await;
        install_cancel_rollback_trigger(&fixture.cp, trigger).await;

        let error = fixture
            .cp
            .cancel_scan_session(
                session.id,
                ScanTerminalReason::new("rollback cancellation").unwrap(),
            )
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::DbUnreachable, "{trigger:?}");
        assert_lifecycle_unchanged(&fixture, session.id, &before).await;
        assert_eq!(
            event_count(&fixture.cp, EventKind::ScanSessionCancelled).await,
            0,
            "{trigger:?}"
        );
    }
}

#[tokio::test]
async fn complete_empty_scan_retires_every_pre_start_location_with_one_summary_fact() {
    let fixture = fixture().await;
    let first = seed_rooted_location(&fixture.cp, fixture.root_id, "first-absent.mkv").await;
    let second = seed_rooted_location(&fixture.cp, fixture.root_id, "second-absent.mkv").await;
    let running = running_session(&fixture, 30).await;
    fixture.clock.advance(Duration::seconds(1));
    let input = complete_input(&fixture, running.id, "complete-empty", None, 0);

    let outcome = fixture
        .cp
        .complete_scan_session(input.clone())
        .await
        .unwrap();
    let replay = fixture.cp.complete_scan_session(input).await.unwrap();

    assert_eq!(outcome, replay);
    assert_eq!(outcome.status, ScanSessionStatus::Succeeded);
    assert_eq!(outcome.observation_count, 0);
    assert_eq!(outcome.retired_location_count, 2);
    let completed = fixture.cp.scan_session(running.id).await.unwrap();
    assert_eq!(completed.retired_location_count, 2);
    let first_state = location_retirement(&fixture.cp, first).await;
    let second_state = location_retirement(&fixture.cp, second).await;
    assert_eq!(first_state.0, completed.terminal_at);
    assert_eq!(second_state.0, completed.terminal_at);
    assert_eq!(first_state.1, 1);
    assert_eq!(second_state.1, 1);
    assert_eq!(first_state.2, Some(running.id));
    assert_eq!(second_state.2, Some(running.id));
    assert_eq!(
        reconciliation_pointer(&fixture.cp, fixture.root_id).await,
        Some(i64::try_from(running.id.0).unwrap())
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionSucceeded).await,
        1
    );
    assert_eq!(file_location_event_count(&fixture.cp).await, 0);
}

#[tokio::test]
async fn complete_retires_only_unobserved_pre_start_locations_from_the_session_root() {
    let fixture = fixture().await;
    let other_root = create_root(&fixture.cp, fixture.node_id, "completion-other").await;
    let observed = seed_rooted_location(&fixture.cp, fixture.root_id, "observed.mkv").await;
    let absent = seed_rooted_location(&fixture.cp, fixture.root_id, "absent.mkv").await;
    let other = seed_rooted_location(&fixture.cp, other_root, "other.mkv").await;
    let running = running_session(&fixture, 30).await;
    let concurrent = seed_rooted_location(&fixture.cp, fixture.root_id, "concurrent.mkv").await;
    let mut batch = batch_input(&fixture, running.id, 0, "complete-observation", 'a');
    batch.observations[0].provider_relative_locator =
        ProviderRelativeLocator::new("observed.mkv".to_owned()).unwrap();
    fixture
        .cp
        .accept_scan_observation_batch(batch)
        .await
        .unwrap();

    let outcome = fixture
        .cp
        .complete_scan_session(complete_input(
            &fixture,
            running.id,
            "complete-observed",
            Some(0),
            1,
        ))
        .await
        .unwrap();

    assert_eq!(outcome.retired_location_count, 1);
    assert!(location_retirement(&fixture.cp, observed).await.0.is_none());
    assert_eq!(
        location_retirement(&fixture.cp, absent).await.2,
        Some(running.id)
    );
    assert!(
        location_retirement(&fixture.cp, concurrent)
            .await
            .0
            .is_none()
    );
    assert!(location_retirement(&fixture.cp, other).await.0.is_none());
}

#[tokio::test]
async fn complete_rejects_wrong_root_high_watermark_without_any_mutation() {
    let fixture = fixture().await;
    let pre_start = seed_rooted_location(&fixture.cp, fixture.root_id, "pre-start.mkv").await;
    let running = running_session(&fixture, 30).await;
    let post_start = seed_rooted_location(&fixture.cp, fixture.root_id, "post-start.mkv").await;
    let other_root = create_root(&fixture.cp, fixture.node_id, "watermark-other").await;
    let other = seed_rooted_location(&fixture.cp, other_root, "other-root.mkv").await;
    let mut connection = fixture.cp.pool_for_test().acquire().await.unwrap();
    connection.close_on_drop();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE scan_sessions SET location_high_watermark_id = ? WHERE id = ?")
        .bind(i64::try_from(other).unwrap())
        .bind(i64::try_from(running.id.0).unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let before = lifecycle_snapshot(&fixture, running.id).await;
    let input = complete_input(&fixture, running.id, "wrong-root-watermark", None, 0);

    let error = fixture.cp.complete_scan_session(input).await.unwrap_err();

    assert!(matches!(error, voom_core::VoomError::Database { .. }));
    assert_lifecycle_unchanged(&fixture, running.id, &before).await;
    for location in [pre_start, post_start, other] {
        assert_eq!(
            location_retirement(&fixture.cp, location).await,
            (None, 0, None)
        );
    }
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionSucceeded).await,
        0
    );
    assert_eq!(
        replay_rows_for_key(&fixture.cp, "wrong-root-watermark").await,
        0
    );
}

#[tokio::test]
async fn complete_rejects_wrong_final_sequence_or_count_without_reconciliation() {
    for (last_sequence, observation_count) in [(Some(0), 0), (None, 1)] {
        let fixture = fixture().await;
        let location = seed_rooted_location(&fixture.cp, fixture.root_id, "still-live.mkv").await;
        let running = running_session(&fixture, 30).await;
        let input = complete_input(
            &fixture,
            running.id,
            "bad-watermark",
            last_sequence,
            observation_count,
        );

        let error = fixture
            .cp
            .complete_scan_session(input.clone())
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::Conflict);
        assert_eq!(replay_rows_for_key(&fixture.cp, "bad-watermark").await, 1);
        let replayed = fixture.cp.complete_scan_session(input).await.unwrap_err();
        assert_eq!(replayed.error_code(), ErrorCode::Conflict);
        assert_eq!(replay_rows_for_key(&fixture.cp, "bad-watermark").await, 1);
        assert_eq!(
            fixture.cp.scan_session(running.id).await.unwrap().status,
            ScanSessionStatus::Running
        );
        assert!(location_retirement(&fixture.cp, location).await.0.is_none());
        assert_eq!(
            reconciliation_pointer(&fixture.cp, fixture.root_id).await,
            None
        );
    }
}

#[tokio::test]
async fn complete_validates_corrupt_session_counters_before_request_watermark() {
    let fixture = fixture().await;
    let observed = seed_rooted_location(&fixture.cp, fixture.root_id, "batch/0-a.mkv").await;
    let absent =
        seed_rooted_location(&fixture.cp, fixture.root_id, "absent-after-corruption.mkv").await;
    let running = running_session(&fixture, 30).await;
    fixture
        .cp
        .accept_scan_observation_batch(batch_input(
            &fixture,
            running.id,
            0,
            "corrupt-ledger-batch",
            'a',
        ))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = 0, batch_count = 0, observation_count = 0 \
         WHERE id = ?",
    )
    .bind(i64::try_from(running.id.0).unwrap())
    .execute(fixture.cp.pool_for_test())
    .await
    .unwrap();
    let input = complete_input(&fixture, running.id, "corrupt-ledger-complete", Some(0), 1);

    let error = fixture
        .cp
        .complete_scan_session(input.clone())
        .await
        .unwrap_err();

    assert!(matches!(error, voom_core::VoomError::Database { .. }));
    assert_eq!(
        fixture.cp.scan_session(running.id).await.unwrap().status,
        ScanSessionStatus::Running
    );
    assert!(location_retirement(&fixture.cp, observed).await.0.is_none());
    assert!(location_retirement(&fixture.cp, absent).await.0.is_none());
    assert_eq!(
        reconciliation_pointer(&fixture.cp, fixture.root_id).await,
        None
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionSucceeded).await,
        0
    );
    assert_eq!(
        replay_rows_for_key(&fixture.cp, "corrupt-ledger-complete").await,
        0
    );

    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = 1, batch_count = 1, observation_count = 1 \
         WHERE id = ?",
    )
    .bind(i64::try_from(running.id.0).unwrap())
    .execute(fixture.cp.pool_for_test())
    .await
    .unwrap();
    let outcome = fixture.cp.complete_scan_session(input).await.unwrap();
    assert_eq!(outcome.status, ScanSessionStatus::Succeeded);
    assert!(location_retirement(&fixture.cp, observed).await.0.is_none());
    assert_eq!(
        location_retirement(&fixture.cp, absent).await.2,
        Some(running.id)
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionSucceeded).await,
        1
    );
}

#[tokio::test]
async fn complete_never_reconciles_failed_cancelled_or_stale_sessions() {
    let failed = fixture().await;
    let failed_location = seed_rooted_location(&failed.cp, failed.root_id, "failed.mkv").await;
    let failed_session = running_session(&failed, 30).await;
    failed
        .cp
        .fail_scan_session(fail_input(&failed, failed_session.id, "terminal-failed"))
        .await
        .unwrap();
    assert_terminal_completion_rejected(&failed, failed_session.id, failed_location).await;

    let cancelled = fixture().await;
    let cancelled_location =
        seed_rooted_location(&cancelled.cp, cancelled.root_id, "cancelled.mkv").await;
    let cancelled_session = running_session(&cancelled, 30).await;
    cancelled
        .cp
        .cancel_scan_session(
            cancelled_session.id,
            ScanTerminalReason::new("operator cancelled").unwrap(),
        )
        .await
        .unwrap();
    assert_terminal_completion_rejected(&cancelled, cancelled_session.id, cancelled_location).await;

    let stale = fixture().await;
    let stale_location = seed_rooted_location(&stale.cp, stale.root_id, "stale.mkv").await;
    let stale_session = running_session(&stale, 30).await;
    stale.clock.advance(Duration::seconds(30));
    let _ = stale
        .cp
        .fail_scan_session(fail_input(&stale, stale_session.id, "terminal-stale"))
        .await
        .unwrap_err();
    assert_terminal_completion_rejected(&stale, stale_session.id, stale_location).await;
}

#[tokio::test]
async fn complete_fence_drift_marks_session_stale_without_reconciliation() {
    for fence in ["epoch", "unavailable", "owner"] {
        let fixture = fixture().await;
        let location = seed_rooted_location(&fixture.cp, fixture.root_id, "fenced.mkv").await;
        let running = running_session(&fixture, 30).await;
        match fence {
            "epoch" => {
                sqlx::query("UPDATE library_roots SET root_epoch = root_epoch + 1 WHERE id = ?")
                    .bind(i64::try_from(fixture.root_id.0).unwrap())
                    .execute(fixture.cp.pool_for_test())
                    .await
                    .unwrap();
            }
            "unavailable" => {
                sqlx::query("UPDATE library_roots SET state = 'unavailable' WHERE id = ?")
                    .bind(i64::try_from(fixture.root_id.0).unwrap())
                    .execute(fixture.cp.pool_for_test())
                    .await
                    .unwrap();
            }
            "owner" => {
                let owner = seed_alternate_owner(&fixture.cp).await;
                sqlx::query("UPDATE library_roots SET owner_node_id = ? WHERE id = ?")
                    .bind(i64::try_from(owner.0).unwrap())
                    .bind(i64::try_from(fixture.root_id.0).unwrap())
                    .execute(fixture.cp.pool_for_test())
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }

        let error = fixture
            .cp
            .complete_scan_session(complete_input(
                &fixture,
                running.id,
                "complete-fenced",
                None,
                0,
            ))
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::Conflict);
        assert_eq!(
            fixture.cp.scan_session(running.id).await.unwrap().status,
            ScanSessionStatus::Stale
        );
        assert!(location_retirement(&fixture.cp, location).await.0.is_none());
        assert_eq!(
            reconciliation_pointer(&fixture.cp, fixture.root_id).await,
            None
        );
    }
}

#[tokio::test]
async fn complete_post_preflight_failures_roll_back_every_write() {
    for trigger in [
        CompleteRollbackTrigger::LocationUpdate,
        CompleteRollbackTrigger::SessionUpdate,
        CompleteRollbackTrigger::EventInsert,
        CompleteRollbackTrigger::ReplayCompletion,
    ] {
        let fixture = fixture().await;
        let location = seed_rooted_location(&fixture.cp, fixture.root_id, "rollback.mkv").await;
        let running = running_session(&fixture, 30).await;
        install_completion_rollback_trigger(&fixture.cp, trigger).await;

        let error = fixture
            .cp
            .complete_scan_session(complete_input(
                &fixture,
                running.id,
                "complete-rollback",
                None,
                0,
            ))
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::DbUnreachable, "{trigger:?}");
        assert_eq!(
            fixture.cp.scan_session(running.id).await.unwrap().status,
            ScanSessionStatus::Running,
            "{trigger:?}"
        );
        assert!(
            location_retirement(&fixture.cp, location).await.0.is_none(),
            "{trigger:?}"
        );
        assert_eq!(
            reconciliation_pointer(&fixture.cp, fixture.root_id).await,
            None,
            "{trigger:?}"
        );
        assert_eq!(
            event_count(&fixture.cp, EventKind::ScanSessionSucceeded).await,
            0,
            "{trigger:?}"
        );
        assert_eq!(
            replay_rows_for_key(&fixture.cp, "complete-rollback").await,
            0,
            "{trigger:?}"
        );
    }
}

#[tokio::test]
async fn complete_commit_locks_are_retryable_and_leave_the_session_running() {
    for state in ["pending", "authorized", "recovery_required"] {
        let fixture = fixture().await;
        let location =
            seed_rooted_location(&fixture.cp, fixture.root_id, "commit-locked.mkv").await;
        let running = running_session(&fixture, 30).await;
        let commit_id = seed_completion_commit_lock(&fixture.cp, location, state).await;

        let input = complete_input(&fixture, running.id, "locked-completion", None, 0);
        let error = fixture
            .cp
            .complete_scan_session(input.clone())
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::Conflict, "{state}");
        assert_eq!(
            fixture.cp.scan_session(running.id).await.unwrap().status,
            ScanSessionStatus::Running,
            "{state}"
        );
        assert!(location_retirement(&fixture.cp, location).await.0.is_none());
        assert_eq!(
            reconciliation_pointer(&fixture.cp, fixture.root_id).await,
            None
        );
        assert_eq!(
            replay_rows_for_key(&fixture.cp, "locked-completion").await,
            0
        );

        sqlx::query("DELETE FROM commit_intents WHERE id = ?")
            .bind(commit_id)
            .execute(fixture.cp.pool_for_test())
            .await
            .unwrap();
        let retried = fixture.cp.complete_scan_session(input).await.unwrap();
        assert_eq!(retried.status, ScanSessionStatus::Succeeded, "{state}");
        assert_eq!(
            event_count(&fixture.cp, EventKind::ScanSessionSucceeded).await,
            1,
            "{state}"
        );
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
    assert_unknown_rejected(RemoteScanCompleteOutcome {
        scan_session_id: ScanSessionId(1),
        status: ScanSessionStatus::Succeeded,
        observation_count: 2,
        retired_location_count: 3,
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
        database,
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

async fn assert_start_root_fence(fence: StartRootFence) {
    let fixture = fixture().await;
    seed_rooted_location(&fixture.cp, fixture.root_id, "must-not-be-captured.mkv").await;
    let requested = request(&fixture).await;
    apply_start_root_fence(&fixture, fence).await;
    let before_events = total_event_count(&fixture.cp).await;
    let before_replays = replay_count(&fixture.cp).await;
    let before_routing = routing_counts(&fixture.cp).await;
    let input = start_input(&fixture, requested.id, "start-root-fence");

    let first_error = fixture
        .cp
        .start_scan_session(input.clone())
        .await
        .unwrap_err();

    assert_eq!(first_error.error_code(), ErrorCode::Conflict, "{fence:?}");
    let stale = fixture.cp.scan_session(requested.id).await.unwrap();
    assert_eq!(stale.status, ScanSessionStatus::Stale, "{fence:?}");
    assert_eq!(stale.owner_incarnation_id, None, "{fence:?}");
    assert_eq!(stale.started_at, None, "{fence:?}");
    assert_eq!(stale.location_high_watermark_id, None, "{fence:?}");
    assert_eq!(observation_count(&fixture.cp, requested.id).await, 0);
    assert_eq!(total_event_count(&fixture.cp).await, before_events + 1);
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStale).await,
        1
    );
    assert_eq!(
        event_count(&fixture.cp, EventKind::ScanSessionStarted).await,
        0
    );
    assert_eq!(replay_count(&fixture.cp).await, before_replays + 1);
    assert_eq!(routing_counts(&fixture.cp).await, before_routing);
    assert_eq!(
        reconciliation_pointer(&fixture.cp, fixture.root_id).await,
        None
    );
    assert_conflict_replay(&fixture.cp, "start-root-fence").await;

    let replay_error = fixture.cp.start_scan_session(input).await.unwrap_err();
    assert_eq!(
        replay_error.to_string(),
        first_error.to_string(),
        "{fence:?}"
    );
    assert_eq!(total_event_count(&fixture.cp).await, before_events + 1);
    assert_eq!(replay_count(&fixture.cp).await, before_replays + 1);
}

async fn apply_start_root_fence(fixture: &Fixture, fence: StartRootFence) {
    let sql = match fence {
        StartRootFence::EpochDrift => {
            "UPDATE library_roots SET root_epoch = root_epoch + 1 WHERE id = ?"
        }
        StartRootFence::Unavailable => {
            "UPDATE library_roots SET state = 'unavailable' WHERE id = ?"
        }
    };
    sqlx::query(sql)
        .bind(i64::try_from(fixture.root_id.0).unwrap())
        .execute(fixture.cp.pool_for_test())
        .await
        .unwrap();
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

async fn running_session_with_observation(fixture: &Fixture) -> ScanSession {
    let running = running_session(fixture, 30).await;
    fixture
        .cp
        .accept_scan_observation_batch(batch_input(
            fixture,
            running.id,
            0,
            "terminal-rollback-baseline",
            'e',
        ))
        .await
        .unwrap();
    fixture.cp.scan_session(running.id).await.unwrap()
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

fn fail_input(fixture: &Fixture, id: ScanSessionId, key: &str) -> RemoteScanFailInput {
    RemoteScanFailInput {
        node_id: fixture.node_id,
        scan_session_id: id,
        incarnation_id: fixture.incarnation_id,
        token: fixture.token.clone(),
        idempotency_key: key.to_owned(),
        request_hash: format!("{key}-route-instance"),
        reason: ScanTerminalReason::new("scanner failed during rollback proof").unwrap(),
    }
}

fn complete_input(
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
        idempotency_key: key.to_owned(),
        request_hash: format!("{key}-route-instance"),
        last_sequence,
        observation_count,
    }
}

async fn assert_terminal_completion_rejected(fixture: &Fixture, id: ScanSessionId, location: u64) {
    let error = fixture
        .cp
        .complete_scan_session(complete_input(fixture, id, "terminal-completion", None, 0))
        .await
        .unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::Conflict);
    assert!(location_retirement(&fixture.cp, location).await.0.is_none());
    assert_eq!(
        reconciliation_pointer(&fixture.cp, fixture.root_id).await,
        None
    );
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
    let root_id = i64::try_from(root_id.0).unwrap();
    with_check_constraints_disabled(cp.pool_for_test(), move |connection| {
        Box::pin(async move {
            sqlx::query("UPDATE library_roots SET root_epoch = -1 WHERE id = ?")
                .bind(root_id)
                .execute(&mut *connection)
                .await?;
            Ok(())
        })
    })
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

async fn install_failure_rollback_trigger(
    cp: &crate::ControlPlane,
    trigger: TerminalRollbackTrigger,
) {
    let sql = match trigger {
        TerminalRollbackTrigger::EventInsert => {
            "CREATE TRIGGER reject_scan_failure_event BEFORE INSERT ON events \
             WHEN NEW.kind = 'scan_session.failed' \
             BEGIN SELECT RAISE(ABORT, 'forced failure event rollback'); END"
        }
        TerminalRollbackTrigger::ReplayCompletion => {
            "CREATE TRIGGER reject_scan_failure_replay \
             BEFORE UPDATE OF status ON remote_idempotency_keys \
             WHEN NEW.status = 'completed' \
             AND NEW.route_key = 'POST /v1/scan/session/fail' \
             BEGIN SELECT RAISE(ABORT, 'forced failure replay rollback'); END"
        }
        TerminalRollbackTrigger::SessionUpdate => {
            "CREATE TRIGGER reject_scan_failure_update BEFORE UPDATE OF status ON scan_sessions \
             WHEN NEW.status = 'failed' \
             BEGIN SELECT RAISE(ABORT, 'forced failure session rollback'); END"
        }
    };
    sqlx::query(sql).execute(cp.pool_for_test()).await.unwrap();
}

async fn install_cancel_rollback_trigger(cp: &crate::ControlPlane, trigger: CancelRollbackTrigger) {
    let sql = match trigger {
        CancelRollbackTrigger::EventInsert => {
            "CREATE TRIGGER reject_scan_cancel_event BEFORE INSERT ON events \
             WHEN NEW.kind = 'scan_session.cancelled' \
             BEGIN SELECT RAISE(ABORT, 'forced cancel event rollback'); END"
        }
        CancelRollbackTrigger::SessionUpdate => {
            "CREATE TRIGGER reject_scan_cancel_update BEFORE UPDATE OF status ON scan_sessions \
             WHEN NEW.status = 'cancelled' \
             BEGIN SELECT RAISE(ABORT, 'forced cancel session rollback'); END"
        }
    };
    sqlx::query(sql).execute(cp.pool_for_test()).await.unwrap();
}

async fn install_completion_rollback_trigger(
    cp: &crate::ControlPlane,
    trigger: CompleteRollbackTrigger,
) {
    let sql = match trigger {
        CompleteRollbackTrigger::LocationUpdate => {
            "CREATE TRIGGER reject_scan_completion_location BEFORE UPDATE OF retired_at \
             ON file_locations WHEN NEW.retired_by_scan_session_id IS NOT NULL \
             BEGIN SELECT RAISE(ABORT, 'forced completion location rollback'); END"
        }
        CompleteRollbackTrigger::SessionUpdate => {
            "CREATE TRIGGER reject_scan_completion_session BEFORE UPDATE OF status \
             ON scan_sessions WHEN NEW.status = 'succeeded' \
             BEGIN SELECT RAISE(ABORT, 'forced completion session rollback'); END"
        }
        CompleteRollbackTrigger::EventInsert => {
            "CREATE TRIGGER reject_scan_completion_event BEFORE INSERT ON events \
             WHEN NEW.kind = 'scan_session.succeeded' \
             BEGIN SELECT RAISE(ABORT, 'forced completion event rollback'); END"
        }
        CompleteRollbackTrigger::ReplayCompletion => {
            "CREATE TRIGGER reject_scan_completion_replay BEFORE UPDATE OF status \
             ON remote_idempotency_keys WHEN NEW.status = 'completed' \
             AND NEW.route_key = 'POST /v1/scan/session/complete' \
             BEGIN SELECT RAISE(ABORT, 'forced completion replay rollback'); END"
        }
    };
    sqlx::query(sql).execute(cp.pool_for_test()).await.unwrap();
}

async fn seed_alternate_owner(cp: &crate::ControlPlane) -> NodeId {
    let id = sqlx::query(
        "INSERT INTO nodes \
         (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata, epoch) \
         VALUES ('alternate-scan-owner', 'local', 'active', ?, ?, 60, \
                 'hash', 'hint', '{}', 0)",
    )
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    NodeId(u64::try_from(id).unwrap())
}

async fn seed_completion_commit_lock(cp: &crate::ControlPlane, location: u64, state: &str) -> i64 {
    let commit_id = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, state, started_at) \
         VALUES ('{}', '{}', '[]', 'pending', '1970-01-01T00:00:00Z')",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO commit_intent_scope_members (commit_intent_id, scope_location_id) \
         VALUES (?, ?)",
    )
    .bind(commit_id)
    .bind(i64::try_from(location).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    if state != "pending" {
        let recovery_reason = (state == "recovery_required").then_some("mutation_failed");
        sqlx::query(
            "UPDATE commit_intents SET state = ?, authorized_at = ?, \
             closure_authorized = closure_initial, target_row_epochs = '[]', \
             recovery_reason = ? WHERE id = ?",
        )
        .bind(state)
        .bind("1970-01-01T00:00:01Z")
        .bind(recovery_reason)
        .bind(commit_id)
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    }
    commit_id
}

async fn lifecycle_snapshot(fixture: &Fixture, id: ScanSessionId) -> LifecycleSnapshot {
    LifecycleSnapshot {
        session: fixture.cp.scan_session(id).await.unwrap(),
        observations: observation_count(&fixture.cp, id).await,
        events: total_event_count(&fixture.cp).await,
        replays: replay_count(&fixture.cp).await,
        routing: routing_counts(&fixture.cp).await,
        reconciliation_pointer: reconciliation_pointer(&fixture.cp, fixture.root_id).await,
    }
}

async fn assert_lifecycle_unchanged(
    fixture: &Fixture,
    id: ScanSessionId,
    before: &LifecycleSnapshot,
) {
    assert_eq!(fixture.cp.scan_session(id).await.unwrap(), before.session);
    assert_eq!(
        observation_count(&fixture.cp, id).await,
        before.observations
    );
    assert_eq!(total_event_count(&fixture.cp).await, before.events);
    assert_eq!(replay_count(&fixture.cp).await, before.replays);
    assert_eq!(routing_counts(&fixture.cp).await, before.routing);
    assert_eq!(
        reconciliation_pointer(&fixture.cp, fixture.root_id).await,
        before.reconciliation_pointer
    );
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

async fn total_event_count(cp: &crate::ControlPlane) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events")
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

async fn scan_progress(cp: &crate::ControlPlane, id: ScanSessionId) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT next_sequence, batch_count, observation_count FROM scan_sessions WHERE id = ?",
    )
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

async fn replay_count(cp: &crate::ControlPlane) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM remote_idempotency_keys")
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

async fn assert_conflict_replay(cp: &crate::ControlPlane, key: &str) {
    let response: String = sqlx::query_scalar(
        "SELECT response_json FROM remote_idempotency_keys \
         WHERE idempotency_key LIKE ? AND status = 'completed'",
    )
    .bind(format!("%:{key}"))
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    let replay: RemoteMutationReplay = serde_json::from_str(&response).unwrap();
    let RemoteMutationReplay::Error { code, .. } = replay else {
        panic!("expected a replayed error")
    };
    assert_eq!(code, ErrorCode::Conflict.as_str());
}

async fn reconciliation_pointer(cp: &crate::ControlPlane, root: StorageRootId) -> Option<i64> {
    sqlx::query_scalar("SELECT last_scan_session_id FROM library_roots WHERE id = ?")
        .bind(i64::try_from(root.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn location_retirement(
    cp: &crate::ControlPlane,
    location: u64,
) -> (Option<OffsetDateTime>, i64, Option<ScanSessionId>) {
    let row: (Option<String>, i64, Option<i64>) = sqlx::query_as(
        "SELECT retired_at, epoch, retired_by_scan_session_id FROM file_locations WHERE id = ?",
    )
    .bind(i64::try_from(location).unwrap())
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    (
        row.0.map(|value| {
            OffsetDateTime::parse(
                &value,
                &time::format_description::well_known::Iso8601::DEFAULT,
            )
            .unwrap()
        }),
        row.1,
        row.2.map(|id| ScanSessionId(u64::try_from(id).unwrap())),
    )
}

async fn file_location_event_count(cp: &crate::ControlPlane) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind LIKE 'file_location.%'")
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
