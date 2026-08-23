use sqlx::SqlitePool;
use time::Duration;
use voom_core::ids::{ArtifactCommitIntentId, ArtifactCommitRecordId};
use voom_core::{NodeId, StorageRootId, VoomError};
use voom_test_support::TempDatabase;

use super::*;
use crate::repo::media::use_leases::{
    BlockingMode, IssuerKind, LeaseScope, NewUseLease, SqliteUseLeaseRepo, UseLeaseKind,
};
use crate::test_support::{
    T0, TEST_FILE_VERSION_ID, fresh_initialized_pool_at, seed_test_rooted_location,
};
const STAGING_LOCATION_ID: u64 = 9_000_002;
const OWNER_NODE_ID: u64 = 9_000_001;
/// Fixed active incarnation seeded for the owner node (16 bytes as hex).
const TEST_INCARNATION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn owner_incarnation() -> voom_core::NodeIncarnationId {
    TEST_INCARNATION.parse().unwrap()
}

fn expected_facts() -> CommitExpectedFacts {
    CommitExpectedFacts {
        size_bytes: 1024,
        content_hash: "blake3:fixture".to_owned(),
    }
}

fn new_intent(commit_record_id: ArtifactCommitRecordId) -> NewArtifactCommitIntent {
    NewArtifactCommitIntent {
        commit_record_id,
        artifact_handle_id: voom_core::ArtifactHandleId(9_000_001),
        source_file_version_id: TEST_FILE_VERSION_ID,
        verification_id: voom_core::ids::ArtifactVerificationId(9_000_001),
        staging_location_id: voom_core::FileLocationId(STAGING_LOCATION_ID),
        staging_location_epoch: 0,
        source_storage_root_id: StorageRootId(9_000_001),
        source_provider_relative_locator: crate::test_support::test_relative_locator(
            "source/movie.mkv",
        ),
        target_storage_root_id: StorageRootId(9_000_001),
        target_root_epoch: 1,
        target_provider_relative_locator: "committed/movie.mkv".to_owned(),
        owner_node_id: NodeId(OWNER_NODE_ID),
        expected_facts: expected_facts(),
        requested_at: T0,
    }
}

/// Seed the identity chain (root, version, staging location), plus handle,
/// worker, successful staging verification, and one pending commit record;
/// return their ids alongside the pool.
async fn fixture() -> (SqlitePool, TempDatabase, ArtifactCommitRecordId) {
    let tmp = TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    seed_test_rooted_location(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO media_works (id, kind, display_title, created_at) \
         VALUES (9000001, 'unknown', 'intent-fixture-work', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO media_variants (id, media_work_id, label, created_at) \
         VALUES (9000001, 9000001, 'main', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO asset_bundles (id, media_variant_id, display_name, created_at) \
         VALUES (9000001, 9000001, 'intent-fixture-bundle', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO file_assets (id, created_at, retired_at, epoch) \
         VALUES (9000002, '1970-01-01T00:00:00Z', NULL, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO artifact_handles \
         (id, privacy_class, durability_class, allowed_access_modes, mutability, \
          created_at, file_asset_id, asset_bundle_id) \
         VALUES (9000001, 'private', 'durable', '[]', 'immutable', \
                 '1970-01-01T00:00:00Z', 9000001, 9000001)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workers (id, name, kind, status, registered_at, last_seen_at, epoch) \
         VALUES (9000001, 'intent-test-worker', 'local', 'active', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO artifact_locations \
         (id, artifact_handle_id, kind, value, observed_at) \
         VALUES (9000002, 9000001, 'staging', 'staging/intent-fixture.mkv', \
                 '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO artifact_verifications \
         (id, artifact_handle_id, artifact_location_id, path, worker_id, status, \
          expected_size_bytes, expected_checksum, observed_size_bytes, observed_checksum, \
          report, started_at, finished_at) \
         VALUES (9000001, 9000001, 9000002, 'staging/intent-fixture.mkv', 9000001, \
                 'succeeded', 1024, 'blake3:fixture', 1024, 'blake3:fixture', '{}', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES (?, 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(TEST_INCARNATION)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO file_locations \
         (id, file_version_id, address_state, storage_root_id, provider_relative_locator, \
          legacy_kind, legacy_locator, proof_kind, proof_value, observed_at, retired_at, epoch) \
         VALUES (9000002, 9000001, 'rooted', 9000001, 'staging/intent-fixture.mkv', \
                 NULL, NULL, NULL, NULL, '1970-01-01T00:00:00Z', NULL, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let res = sqlx::query(
        "INSERT INTO artifact_commit_records \
         (artifact_handle_id, source_file_version_id, verification_id, target_path, state, \
          report, started_at) \
         VALUES (9000001, 9000001, 9000001, 'committed/intent-fixture.mkv', 'pending', \
                 '{}', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    (
        pool,
        tmp,
        ArtifactCommitRecordId(u64::try_from(res.last_insert_rowid()).unwrap()),
    )
}

async fn create_pending(
    pool: &SqlitePool,
    commit_record_id: ArtifactCommitRecordId,
) -> ArtifactCommitIntent {
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let intent = repo
        .create_pending_in_tx(&mut tx, new_intent(commit_record_id))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    intent
}

async fn authorize(pool: &SqlitePool, id: ArtifactCommitIntentId) -> ArtifactCommitIntent {
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let intent = repo
        .authorize_in_tx(&mut tx, id, owner_incarnation(), T0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    intent
}

// --- create / read ---

#[tokio::test]
async fn create_pending_pins_full_scope() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    assert_eq!(intent.state, ArtifactCommitIntentState::Pending);
    assert_eq!(intent.intent_epoch, 0);
    assert_eq!(intent.commit_fence, None);
    assert_eq!(intent.receipt, None);
    assert_eq!(intent.supplemental_receipt, None);
    assert_eq!(intent.owner_incarnation_id, None);
    assert_eq!(intent.expected_facts, expected_facts());
    assert_eq!(
        intent.target_provider_relative_locator,
        "committed/movie.mkv"
    );

    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let fetched = repo.require_intent_in_tx(&mut tx, intent.id).await.unwrap();
    assert_eq!(fetched.id, intent.id);
    let by_record = repo
        .get_by_commit_record_in_tx(&mut tx, record)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_record.id, intent.id);
}

#[tokio::test]
async fn second_intent_for_same_commit_record_conflicts() {
    let (pool, _tmp, record) = fixture().await;
    create_pending(&pool, record).await;
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .create_pending_in_tx(&mut tx, new_intent(record))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));
}

#[tokio::test]
async fn require_unknown_intent_is_not_found() {
    let (pool, _tmp, _record) = fixture().await;
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .require_intent_in_tx(&mut tx, ArtifactCommitIntentId(999_999))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::NotFound(_)));
}

// --- authorize ---

#[tokio::test]
async fn authorize_mints_one_time_fence_and_bumps_epoch() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    let authorized = authorize(&pool, intent.id).await;
    assert_eq!(authorized.state, ArtifactCommitIntentState::Authorized);
    assert_eq!(authorized.intent_epoch, 1);
    let fence = authorized.commit_fence.as_ref().unwrap();
    assert_eq!(fence.len(), 32);
    assert!(authorized.owner_incarnation_id.is_some());
    assert_eq!(authorized.authorized_at, Some(T0));
    assert_eq!(authorized.receipt, None);

    // A second authorize fails closed: the CAS no longer matches pending.
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .authorize_in_tx(&mut tx, intent.id, owner_incarnation(), T0)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));
}

#[tokio::test]
async fn intent_debug_redacts_fence_bytes() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    let authorized = authorize(&pool, intent.id).await;

    // The raw fence is capability material: the repo row's Debug rendering
    // must never leak it into a log or telemetry surface.
    let rendered = format!("{authorized:?}");
    let fence = authorized.commit_fence.as_ref().unwrap();
    let fence_hex = fence
        .iter()
        .fold(String::new(), |acc, byte| format!("{acc}{byte:02x}"));
    assert!(!rendered.contains(&fence_hex), "{rendered}");
    assert!(!rendered.contains(&format!("{fence:?}")), "{rendered}");
    assert!(rendered.contains("[REDACTED]"), "{rendered}");
}

// --- receipts ---

#[tokio::test]
async fn receipt_ordering_applying_then_applied() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    let _authorized = authorize(&pool, intent.id).await;

    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    // Applied before any Applying journal is rejected.
    let err = repo
        .record_receipt_in_tx(
            &mut tx,
            intent.id,
            CommitReceipt::Applied(AppliedReceipt {
                observed: CommitObservedFacts {
                    size_bytes: 1024,
                    content_hash: "blake3:fixture".to_owned(),
                },
                reported_at: "1970-01-01T00:00:05Z".to_owned(),
            }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));

    repo.record_receipt_in_tx(
        &mut tx,
        intent.id,
        CommitReceipt::Applying(ApplyingReceipt {
            reported_at: "1970-01-01T00:00:01Z".to_owned(),
        }),
    )
    .await
    .unwrap();
    // A second Applying may not overwrite the journal.
    let err = repo
        .record_receipt_in_tx(
            &mut tx,
            intent.id,
            CommitReceipt::Applying(ApplyingReceipt {
                reported_at: "1970-01-01T00:00:02Z".to_owned(),
            }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));

    let updated = repo
        .record_receipt_in_tx(
            &mut tx,
            intent.id,
            CommitReceipt::Applied(AppliedReceipt {
                observed: CommitObservedFacts {
                    size_bytes: 1024,
                    content_hash: "blake3:fixture".to_owned(),
                },
                reported_at: "1970-01-01T00:00:03Z".to_owned(),
            }),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(updated.receipt, Some(CommitReceipt::Applied(_))));
    assert_eq!(updated.intent_epoch, 3);
}

#[tokio::test]
async fn receipt_on_pending_or_terminal_state_is_rejected() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());

    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .record_receipt_in_tx(
            &mut tx,
            intent.id,
            CommitReceipt::Applying(ApplyingReceipt {
                reported_at: "1970-01-01T00:00:01Z".to_owned(),
            }),
        )
        .await
        .unwrap_err();
    drop(tx);
    assert!(matches!(err, VoomError::Conflict(_)));

    let aborted = {
        let mut tx = pool.begin().await.unwrap();
        repo.mark_aborted_in_tx(&mut tx, intent.id, T0)
            .await
            .unwrap()
    };
    assert_eq!(aborted.state, ArtifactCommitIntentState::Aborted);
    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .record_receipt_in_tx(
            &mut tx,
            intent.id,
            CommitReceipt::Applying(ApplyingReceipt {
                reported_at: "1970-01-01T00:00:01Z".to_owned(),
            }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));
}

#[tokio::test]
async fn unknown_receipt_vocabulary_decodes_as_database_error() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    authorize(&pool, intent.id).await;
    sqlx::query("UPDATE artifact_commit_intents SET receipt = '{\"kind\":\"bogus\"}' WHERE id = ?")
        .bind(i64::try_from(intent.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .require_intent_in_tx(&mut tx, intent.id)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Database { .. }));
}

// --- terminal transitions ---

#[tokio::test]
async fn complete_consumes_fence_from_authorized() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    authorize(&pool, intent.id).await;

    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let completed = repo
        .mark_completed_in_tx(&mut tx, intent.id, T0)
        .await
        .unwrap();
    assert_eq!(completed.state, ArtifactCommitIntentState::Completed);
    // The terminal row retains no fence material once it can no longer
    // gate a mutation.
    assert_eq!(completed.commit_fence, None);
    assert_eq!(completed.terminal_at, Some(T0));

    // Terminal states never reopen.
    let err = repo
        .mark_completed_in_tx(&mut tx, intent.id, T0)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));
    let err = repo
        .mark_aborted_in_tx(&mut tx, intent.id, T0)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));
}

#[tokio::test]
async fn recovery_required_accepts_supplemental_receipt_then_completes() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    authorize(&pool, intent.id).await;

    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    // Supplemental evidence cannot land on an authorized intent.
    let err = repo
        .append_supplemental_receipt_in_tx(
            &mut tx,
            intent.id,
            CommitReceipt::Applied(AppliedReceipt {
                observed: CommitObservedFacts {
                    size_bytes: 1024,
                    content_hash: "blake3:fixture".to_owned(),
                },
                reported_at: "1970-01-01T00:00:09Z".to_owned(),
            }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));

    let stuck = repo
        .mark_recovery_required_in_tx(&mut tx, intent.id, T0)
        .await
        .unwrap();
    assert_eq!(stuck.state, ArtifactCommitIntentState::RecoveryRequired);
    // The fence keeps blocking through recovery; only a terminal
    // transition nulls it.
    assert!(stuck.commit_fence.is_some());
    repo.append_supplemental_receipt_in_tx(
        &mut tx,
        intent.id,
        CommitReceipt::OutcomeUnknown(OutcomeUnknownReceipt {
            reason: "target absent".to_owned(),
            reported_at: "1970-01-01T00:00:10Z".to_owned(),
        }),
    )
    .await
    .unwrap();
    let completed = repo
        .mark_completed_in_tx(&mut tx, intent.id, T0)
        .await
        .unwrap();
    assert_eq!(completed.state, ArtifactCommitIntentState::Completed);
    assert_eq!(completed.commit_fence, None);
    assert!(completed.supplemental_receipt.is_some());
}

#[tokio::test]
async fn abort_nulls_the_fence_of_an_authorized_intent() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    assert!(authorize(&pool, intent.id).await.commit_fence.is_some());

    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let aborted = repo
        .mark_aborted_in_tx(&mut tx, intent.id, T0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(aborted.state, ArtifactCommitIntentState::Aborted);
    assert_eq!(aborted.commit_fence, None);
    assert_eq!(aborted.terminal_at, Some(T0));
}

#[tokio::test]
async fn abort_releases_pending_without_fence() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let aborted = repo
        .mark_aborted_in_tx(&mut tx, intent.id, T0)
        .await
        .unwrap();
    assert_eq!(aborted.state, ArtifactCommitIntentState::Aborted);
    assert_eq!(aborted.commit_fence, None);
    assert_eq!(aborted.terminal_at, Some(T0));
}

// --- open listing ---

#[tokio::test]
async fn open_listing_follows_current_root_owner_and_epoch() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;

    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let listed = repo
        .list_open_for_roots_in_tx(&mut tx, NodeId(OWNER_NODE_ID))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, intent.id);
    drop(tx);

    // Bumping the root epoch hides the stale-pinned intent...
    sqlx::query("UPDATE library_roots SET root_epoch = 2 WHERE id = 9000001")
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        repo.list_open_for_roots_in_tx(&mut tx, NodeId(OWNER_NODE_ID))
            .await
            .unwrap()
            .is_empty()
    );
    drop(tx);

    // ...until the pin moves with it (simulating re-pin), or ownership moves.
    sqlx::query("UPDATE artifact_commit_intents SET target_root_epoch = 2 WHERE id = ?")
        .bind(i64::try_from(intent.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        repo.list_open_for_roots_in_tx(&mut tx, NodeId(OWNER_NODE_ID))
            .await
            .unwrap()
            .len(),
        1
    );
    drop(tx);

    sqlx::query(
        "INSERT OR IGNORE INTO nodes (id, name, kind, status, registered_at, last_seen_at, \
         retired_at, heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata, epoch) \
         VALUES (9000002, 'other-owner', 'local', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', NULL, 60, 'hash', 'hint', '{}', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE library_roots SET owner_node_id = 9000002, root_epoch = 3 WHERE id = 9000001",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE artifact_commit_intents SET target_root_epoch = 3 WHERE id = ?")
        .bind(i64::try_from(intent.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        repo.list_open_for_roots_in_tx(&mut tx, NodeId(OWNER_NODE_ID))
            .await
            .unwrap()
            .is_empty()
    );
    let moved = repo
        .list_open_for_roots_in_tx(&mut tx, NodeId(9_000_002))
        .await
        .unwrap();
    assert_eq!(moved.len(), 1);
}

#[tokio::test]
async fn open_listing_hides_terminal_intents() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    {
        let mut tx = pool.begin().await.unwrap();
        repo.mark_aborted_in_tx(&mut tx, intent.id, T0)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let mut tx = pool.begin().await.unwrap();
    assert!(
        repo.list_open_for_roots_in_tx(&mut tx, NodeId(OWNER_NODE_ID))
            .await
            .unwrap()
            .is_empty()
    );
}

// --- lease consultation ---

#[tokio::test]
async fn blocking_lease_refused_on_pinned_scope_until_abort() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;
    let leases = SqliteUseLeaseRepo::new(pool.clone());
    let err = leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Location(voom_core::FileLocationId(STAGING_LOCATION_ID)),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, VoomError::Conflict(_)),
        "location scope must be pinned"
    );

    // The source-version scope is pinned by the same intent.
    let err = leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Version(TEST_FILE_VERSION_ID),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, VoomError::Conflict(_)),
        "version scope must be pinned"
    );

    // The parent asset and bundle of the pinned artifact handle are pinned too.
    for scope in [
        LeaseScope::Asset(voom_core::FileAssetId(9_000_001)),
        LeaseScope::Bundle(voom_core::BundleId(9_000_001)),
    ] {
        let err = leases
            .acquire(NewUseLease {
                kind: UseLeaseKind::Playback,
                scope,
                issuer_kind: IssuerKind::User,
                issuer_ref: "alice".to_owned(),
                blocking_mode: BlockingMode::Blocking,
                ttl: Some(Duration::seconds(60)),
                acquired_at: T0,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, VoomError::Conflict(_)),
            "{scope:?} must be pinned"
        );
    }

    // Unrelated scopes are unaffected.
    let other = leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(voom_core::FileAssetId(9_000_002)),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    assert!(other.epoch == 0);

    // Abort releases the pin.
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    repo.mark_aborted_in_tx(&mut tx, intent.id, T0)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let released = leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Location(voom_core::FileLocationId(STAGING_LOCATION_ID)),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    assert_eq!(
        released.scope,
        LeaseScope::Location(voom_core::FileLocationId(STAGING_LOCATION_ID))
    );
}

// --- scan-completion retirement lock ---

#[tokio::test]
async fn scan_reconciliation_lock_refuses_retiring_a_pinned_staging_location() {
    let (pool, _tmp, record) = fixture().await;
    let intent = create_pending(&pool, record).await;

    let mut tx = pool.begin().await.unwrap();
    let hit = consult_scan_reconciliation_artifact_intent_lock_in_tx(
        &mut tx,
        StorageRootId(9_000_001),
        voom_core::ids::ScanSessionId(1),
        Some(voom_core::FileLocationId(STAGING_LOCATION_ID)),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        hit,
        Some((intent.id, "pending".to_owned(), STAGING_LOCATION_ID))
    );

    // The fence stays blocking through authorization and recovery.
    let intent = authorize(&pool, intent.id).await;
    let mut tx = pool.begin().await.unwrap();
    let hit = consult_scan_reconciliation_artifact_intent_lock_in_tx(
        &mut tx,
        StorageRootId(9_000_001),
        voom_core::ids::ScanSessionId(1),
        Some(voom_core::FileLocationId(STAGING_LOCATION_ID)),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        hit,
        Some((intent.id, "authorized".to_owned(), STAGING_LOCATION_ID))
    );

    // Abort releases the pin: the location may be retired again.
    let repo = SqliteArtifactCommitIntentRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    repo.mark_aborted_in_tx(&mut tx, intent.id, T0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let hit = consult_scan_reconciliation_artifact_intent_lock_in_tx(
        &mut tx,
        StorageRootId(9_000_001),
        voom_core::ids::ScanSessionId(1),
        Some(voom_core::FileLocationId(STAGING_LOCATION_ID)),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(hit, None);
}
