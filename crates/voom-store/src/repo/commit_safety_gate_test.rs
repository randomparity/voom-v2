#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]

use super::*;
use voom_core::ids::{FileLocationId, FileVersionId};

#[test]
fn commit_target_constructors_compile_for_every_sprint_1_variant() {
    let _ = CommitTarget::DeleteFileLocation(FileLocationId(1));
    let _ = CommitTarget::ReplaceFileLocation {
        retired: FileLocationId(3),
        new: file_location_proposal_fixture(),
    };
    let _ = CommitTarget::MoveFileLocation {
        retired: FileLocationId(4),
        new: file_location_proposal_fixture(),
    };
}

#[test]
fn affected_scope_closure_default_is_empty() {
    let c = AffectedScopeClosure::default();
    assert!(c.file_assets.is_empty());
    assert!(c.file_versions.is_empty());
    assert!(c.file_locations.is_empty());
    assert!(c.bundles.is_empty());
    assert!(c.resolution_warnings.is_empty());
}

#[test]
fn closure_warning_debug_round_trips() {
    let w = ClosureWarning {
        message: "alias unreachable".to_owned(),
    };
    let debug = format!("{w:?}");
    assert!(debug.contains("alias unreachable"));
}

#[test]
fn closure_failure_variants_construct() {
    let _ = ClosureFailure::AliasUnreachable {
        message: "fs error".to_owned(),
    };
}

#[test]
fn evidence_drift_variants_construct() {
    let _ = EvidenceDrift::PinnedFileVersionRetired;
    let _ = EvidenceDrift::PinnedHashDiffers;
    let _ = EvidenceDrift::PinnedLocationRetired;
}

#[test]
fn target_member_kind_variants_construct() {
    let _ = TargetMemberKind::FileAsset;
    let _ = TargetMemberKind::FileVersion;
    let _ = TargetMemberKind::FileLocation;
    let _ = TargetMemberKind::Bundle;
}

#[test]
fn target_epoch_drift_constructor_smokes() {
    let d = TargetEpochDrift {
        kind: TargetMemberKind::FileLocation,
        id: 17,
        expected: 4,
        observed: 5,
    };
    assert_eq!(d.kind, TargetMemberKind::FileLocation);
    assert_eq!(d.id, 17);
    assert_eq!(d.expected, 4);
    assert_eq!(d.observed, 5);
}

fn file_location_proposal_fixture() -> FileLocationProposal {
    use crate::repo::identity::FileLocationKind;
    FileLocationProposal {
        kind: FileLocationKind::LocalPath,
        value: "/tmp/stub".to_owned(),
        proof: None,
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn commit_intent_state_variants_construct() {
    let _ = CommitIntentState::Pending;
    let _ = CommitIntentState::Authorized;
    let _ = CommitIntentState::Completed;
    let _ = CommitIntentState::Aborted;
    let _ = CommitIntentState::RecoveryRequired;
}

#[test]
fn mutation_outcome_variants_construct() {
    let _ = MutationOutcome::Applied { observed: None };
    let _ = MutationOutcome::Applied {
        observed: Some(AffectedScopeClosure::default()),
    };
    let _ = MutationOutcome::NotPerformed;
}

#[test]
fn abort_reason_variants_construct() {
    let _ = AbortReason::OperatorCancel;
    let _ = AbortReason::MutationFailed;
    let _ = AbortReason::ClosureGrew;
    let _ = AbortReason::ClosureIncomplete;
    let _ = AbortReason::FreshLease;
    let _ = AbortReason::StaleEvidence;
    let _ = AbortReason::StaleTargetEpoch;
    let _ = AbortReason::Other("custom".to_owned());
}

#[test]
fn commit_intent_constructor_smokes() {
    let intent = CommitIntent {
        commit_id: CommitId(1),
        closure_initial: AffectedScopeClosure::default(),
        evaluated_lease_ids: Vec::new(),
        revalidated_evidence: Vec::new(),
        epoch: 0,
    };
    assert_eq!(intent.commit_id, CommitId(1));
}

#[test]
fn commit_permit_constructor_smokes() {
    let permit = CommitPermit {
        commit_id: CommitId(2),
        authorized_at: time::OffsetDateTime::UNIX_EPOCH,
        closure_authorized: AffectedScopeClosure::default(),
        evaluated_lease_ids: Vec::new(),
        revalidated_evidence: Vec::new(),
        epoch: 1,
    };
    assert_eq!(permit.commit_id(), CommitId(2));
    assert_eq!(permit.epoch(), 1);
}

#[test]
fn commit_permit_accessors_return_internal_state() {
    // Round-4 finding: CommitPermit fields are module-private; external
    // consumers reach state through accessors. This test is a sibling
    // of the parent module and uses the struct literal directly to pin
    // each accessor to its field — a future rename or accessor
    // regression breaks the test.
    let mut closure = AffectedScopeClosure::default();
    closure.file_locations.insert(FileLocationId(99));
    let leases = vec![voom_core::ids::UseLeaseId(7)];
    let evidence = vec![EvidenceRevalidationResult {
        evidence_id: voom_core::ids::EvidenceId(3),
        drift: None,
    }];

    let permit = CommitPermit {
        commit_id: CommitId(42),
        authorized_at: time::OffsetDateTime::UNIX_EPOCH,
        closure_authorized: closure.clone(),
        evaluated_lease_ids: leases.clone(),
        revalidated_evidence: evidence.clone(),
        epoch: 5,
    };

    assert_eq!(permit.commit_id(), CommitId(42));
    assert_eq!(permit.authorized_at(), time::OffsetDateTime::UNIX_EPOCH);
    assert_eq!(permit.closure_authorized(), &closure);
    assert_eq!(permit.evaluated_lease_ids(), leases.as_slice());
    assert_eq!(permit.revalidated_evidence(), evidence.as_slice());
    assert_eq!(permit.epoch(), 5);
}

#[test]
fn commit_gate_outcome_constructor_smokes() {
    let outcome = CommitGateOutcome {
        commit_id: CommitId(4),
        closure_initial: AffectedScopeClosure::default(),
        closure_authorized: AffectedScopeClosure::default(),
        closure_final: AffectedScopeClosure::default(),
        evaluated_lease_ids: Vec::new(),
        revalidated_evidence: Vec::new(),
        result: CommitGateResult::Allowed,
    };
    assert!(matches!(outcome.result, CommitGateResult::Allowed));
}

#[test]
fn commit_gate_result_every_sprint_1_variant_constructs() {
    let _ = CommitGateResult::Allowed;
    let _ = CommitGateResult::CancelledAfterAuthorize;
    let _ = CommitGateResult::BlockedByUseLease {
        lease_id: voom_core::ids::UseLeaseId(1),
        lease_scope: LeaseScope::Bundle(BundleId(1)),
    };
    let _ = CommitGateResult::BlockedByPendingCommit {
        commit_id: CommitId(2),
        offending_scope: LeaseScope::Bundle(BundleId(1)),
    };
    let _ = CommitGateResult::BlockedByStaleEvidence {
        evidence_id: voom_core::ids::EvidenceId(3),
        drift: EvidenceDrift::PinnedFileVersionRetired,
    };
    let _ = CommitGateResult::BlockedByClosureIncomplete {
        reason: ClosureFailure::AliasUnreachable {
            message: "fs".into(),
        },
        unreachable: Vec::new(),
    };
    let _ = CommitGateResult::BlockedByClosureGrew {
        delta: ClosureMemberDelta::default(),
    };
    let _ = CommitGateResult::BlockedByStaleTargetEpoch { drift: Vec::new() };
}

#[test]
fn destructive_commit_constructs_without_override_token() {
    // `DestructiveCommit` currently carries no `override_token` field;
    // the force-path slice adds it. This test will need an update once
    // that lands.
    let _ = DestructiveCommit {
        target: CommitTarget::DeleteFileLocation(FileLocationId(1)),
        accepted_evidence_ids: Vec::new(),
    };
}

#[test]
fn affected_scope_closure_equality_is_order_insensitive() {
    // Same three locations inserted in different orders must compare
    // equal — that is the whole point of using BTreeSet over Vec.
    let mut a = AffectedScopeClosure::default();
    a.file_locations.insert(FileLocationId(3));
    a.file_locations.insert(FileLocationId(1));
    a.file_locations.insert(FileLocationId(2));

    let mut b = AffectedScopeClosure::default();
    b.file_locations.insert(FileLocationId(1));
    b.file_locations.insert(FileLocationId(2));
    b.file_locations.insert(FileLocationId(3));

    assert_eq!(a, b);
}

#[test]
fn affected_scope_closure_deduplicates_on_insert() {
    // A second insert of the same ID must not grow the set; the
    // commit_intent_scope_members write derived from this must not
    // emit duplicate rows for the same scope.
    let mut c = AffectedScopeClosure::default();
    c.file_locations.insert(FileLocationId(7));
    c.file_locations.insert(FileLocationId(7));
    assert_eq!(c.file_locations.len(), 1);
}

#[test]
fn file_location_proposal_does_not_carry_file_version_id() {
    // Finding 1: the type level forbids constructing a proposal
    // anchored to a different FileVersion than the retired location.
    // This test is a compile-time guarantee: if someone re-adds a
    // file_version_id field, the exhaustive destructuring below stops
    // compiling and the new field name must be added explicitly.
    let p = file_location_proposal_fixture();
    let FileLocationProposal {
        kind: _,
        value: _,
        proof: _,
        observed_at: _,
    } = p;
}

#[test]
fn evidence_revalidation_result_constructs() {
    let r = EvidenceRevalidationResult {
        evidence_id: voom_core::ids::EvidenceId(1),
        drift: None,
    };
    assert_eq!(r.evidence_id, voom_core::ids::EvidenceId(1));
    assert!(r.drift.is_none());

    let r2 = EvidenceRevalidationResult {
        evidence_id: voom_core::ids::EvidenceId(2),
        drift: Some(EvidenceDrift::PinnedHashDiffers),
    };
    assert!(r2.drift.is_some());
}

#[test]
fn pending_commit_intent_constructs() {
    let p = PendingCommitIntent {
        commit_id: CommitId(9),
        target: CommitTarget::DeleteFileLocation(FileLocationId(2)),
        state: CommitIntentState::Pending,
        closure_initial: AffectedScopeClosure::default(),
        closure_authorized: None,
        accepted_evidence_ids: Vec::new(),
        started_at: time::OffsetDateTime::UNIX_EPOCH,
        authorized_at: None,
    };
    assert_eq!(p.state, CommitIntentState::Pending);
    assert!(p.closure_authorized.is_none());

    let p2 = PendingCommitIntent {
        commit_id: CommitId(10),
        target: CommitTarget::DeleteFileLocation(FileLocationId(3)),
        state: CommitIntentState::Authorized,
        closure_initial: AffectedScopeClosure::default(),
        closure_authorized: Some(AffectedScopeClosure::default()),
        accepted_evidence_ids: Vec::new(),
        started_at: time::OffsetDateTime::UNIX_EPOCH,
        authorized_at: Some(time::OffsetDateTime::UNIX_EPOCH),
    };
    assert_eq!(p2.state, CommitIntentState::Authorized);
    assert!(p2.closure_authorized.is_some());
}

#[test]
fn bypass_kind_variants_construct() {
    let _ = BypassKind::ClosureIncomplete;
}

#[test]
fn force_path_token_constructs() {
    let mut bypass = std::collections::BTreeSet::new();
    bypass.insert(BypassKind::ClosureIncomplete);
    let t = ForcePathToken {
        actor: "ops@example.com".to_owned(),
        reason: "fs offline".to_owned(),
        bypass,
    };
    assert_eq!(t.actor, "ops@example.com");
    assert!(t.bypass.contains(&BypassKind::ClosureIncomplete));
}

#[test]
fn force_path_token_serde_round_trips_through_json() {
    let mut bypass = std::collections::BTreeSet::new();
    bypass.insert(BypassKind::ClosureIncomplete);
    let t = ForcePathToken {
        actor: "ops@example.com".to_owned(),
        reason: "fs offline".to_owned(),
        bypass,
    };
    let json = serde_json::to_string(&t).unwrap();
    let back: ForcePathToken = serde_json::from_str(&json).unwrap();
    assert_eq!(t, back);
}

#[test]
fn id_member_delta_ignores_resolution_warnings() {
    // Round-3 finding: warnings are non-fatal audit annotations and
    // must not contribute to closure drift. Two closures with the
    // same ID sets but different warning order, content, or
    // multiplicity must produce an empty delta.
    let mut a = AffectedScopeClosure::default();
    a.file_locations.insert(FileLocationId(1));
    a.resolution_warnings.push(ClosureWarning {
        message: "alias mount slow".to_owned(),
    });

    let mut b = AffectedScopeClosure::default();
    b.file_locations.insert(FileLocationId(1));
    b.resolution_warnings.push(ClosureWarning {
        message: "different warning text".to_owned(),
    });
    b.resolution_warnings.push(ClosureWarning {
        message: "second warning only on b".to_owned(),
    });

    let delta = a.id_member_delta(&b);
    assert!(delta.is_empty());
}

#[test]
fn id_member_delta_reports_added_and_removed_ids() {
    let mut initial = AffectedScopeClosure::default();
    initial.file_locations.insert(FileLocationId(1));
    initial.file_locations.insert(FileLocationId(2));
    initial.bundles.insert(BundleId(10));

    let mut recomputed = AffectedScopeClosure::default();
    recomputed.file_locations.insert(FileLocationId(2));
    recomputed.file_locations.insert(FileLocationId(3));
    recomputed.bundles.insert(BundleId(10));
    recomputed.bundles.insert(BundleId(11));

    let delta = initial.id_member_delta(&recomputed);
    assert!(!delta.is_empty());
    assert!(delta.added_locations.contains(&FileLocationId(3)));
    assert!(delta.removed_locations.contains(&FileLocationId(1)));
    assert!(delta.added_bundles.contains(&BundleId(11)));
    assert!(delta.removed_bundles.is_empty());
}

#[test]
fn alias_resolution_error_variants_construct() {
    let _ = AliasResolutionError::Unreachable {
        message: "fs offline".to_owned(),
    };
    let _ = AliasResolutionError::Database("connect refused".to_owned());
}

#[test]
fn alias_resolution_error_debug_round_trips() {
    let e = AliasResolutionError::Unreachable {
        message: "mount /srv/media offline".to_owned(),
    };
    let debug = format!("{e:?}");
    assert!(debug.contains("mount /srv/media offline"));
}

// -- FailingAliasResolver -------------------------------------------------

use crate::test_support::FailingAliasResolver;

#[tokio::test]
async fn failing_alias_resolver_returns_unreachable_for_configured_ids() {
    let resolver = FailingAliasResolver::new([FileVersionId(42)]);
    let err = resolver
        .aliases_for_version(FileVersionId(42))
        .await
        .unwrap_err();
    assert!(matches!(err, AliasResolutionError::Unreachable { .. }));
}

#[tokio::test]
async fn failing_alias_resolver_returns_empty_for_unconfigured_ids() {
    let resolver = FailingAliasResolver::new([FileVersionId(42)]);
    let got = resolver
        .aliases_for_version(FileVersionId(7))
        .await
        .unwrap();
    assert!(got.is_empty());
}

#[tokio::test]
async fn failing_alias_resolver_empty_set_never_fails() {
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let got = resolver
        .aliases_for_version(FileVersionId(1))
        .await
        .unwrap();
    assert!(got.is_empty());
}

// -- Migration 0005 CHECK negative coverage (round-6) ---------------------

use crate::test_support::fresh_initialized_pool_at;

/// Helper: open a fresh pool against a temp DB with all migrations
/// applied. Returns the pool and the tempfile so the test owns the
/// lifetime.
async fn fresh_pool_for_schema_check() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

#[tokio::test]
async fn commit_intents_check_rejects_authorized_with_null_closure_authorized() {
    // Round-6 finding #3: a row in state='authorized' that lacks
    // closure_authorized must be rejected by SQLite at INSERT time.
    // Before the round-6 tightening, this INSERT would have succeeded
    // and corrupted crash-recovery / list / finalize inspection.
    let (pool, _tmp) = fresh_pool_for_schema_check().await;
    let err = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, state, started_at, \
          authorized_at, target_row_epochs) \
         VALUES ('{}', '{}', '[]', 'authorized', '2026-05-18T00:00:00Z', \
                 '2026-05-18T00:00:00Z', '[]')",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    // SQLite surfaces CHECK violations through sqlx::Error::Database.
    assert!(
        format!("{err}").contains("CHECK"),
        "expected CHECK violation, got: {err}"
    );
}

#[tokio::test]
async fn commit_intents_check_rejects_completed_with_null_closure_authorized() {
    let (pool, _tmp) = fresh_pool_for_schema_check().await;
    let err = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, state, started_at, \
          authorized_at, finalized_at, target_row_epochs) \
         VALUES ('{}', '{}', '[]', 'completed', '2026-05-18T00:00:00Z', \
                 '2026-05-18T00:00:00Z', '2026-05-18T00:00:01Z', '[]')",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(
        format!("{err}").contains("CHECK"),
        "expected CHECK violation, got: {err}"
    );
}

#[tokio::test]
async fn commit_intents_check_rejects_recovery_required_with_null_closure_authorized() {
    let (pool, _tmp) = fresh_pool_for_schema_check().await;
    let err = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, state, started_at, \
          authorized_at, recovery_reason, target_row_epochs) \
         VALUES ('{}', '{}', '[]', 'recovery_required', '2026-05-18T00:00:00Z', \
                 '2026-05-18T00:00:00Z', 'stale_target_epoch', '[]')",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(
        format!("{err}").contains("CHECK"),
        "expected CHECK violation, got: {err}"
    );
}

// -- prepare_destructive_commit sibling tests (commit 4) -------------------

use crate::repo::events::{EventFilter, EventRepo, Page, SqliteEventRepo};
use crate::repo::identity::{
    AcceptedPin, FileLocationKind as IdentityFileLocationKind, IdentityRepo,
    NewFileLocation as IdentityNewFileLocation, NewFileVersion, NewIdentityEvidence, ProducedBy,
    SqliteIdentityRepo,
};
use crate::repo::use_leases::{
    BlockingMode, IssuerKind, NewUseLease, SqliteUseLeaseRepo, UseLeaseKind, UseLeaseRepo,
};
use crate::test_support::T0;
use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use voom_core::ids::FileLocationId as CoreFileLocationId;
use voom_events::EventKind;

/// One full M2 identity chain (asset → version → location), returned
/// in raw IDs. Sufficient for a Phase A closure walk.
#[expect(
    clippy::struct_field_names,
    reason = "every field is an ID by design; the `_id` postfix is the convention used elsewhere in the codebase (see ingest_identity_invariants.rs)"
)]
struct SeededLocation {
    asset_id: voom_core::FileAssetId,
    version_id: FileVersionId,
    location_id: FileLocationId,
}

/// Seed one asset/version/location. Uses raw repo calls (not control
/// plane) so the test stays inside `voom-store`. Each invocation
/// creates a distinct chain.
async fn seed_location(pool: &SqlitePool, value: &str) -> SeededLocation {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let asset = identity.create_file_asset(T0).await.unwrap();
    let version = identity
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: format!("hash-{value}"),
            size_bytes: 1,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let location = identity
        .create_file_location_in_tx(
            &mut tx,
            IdentityNewFileLocation {
                file_version_id: version.id,
                kind: IdentityFileLocationKind::LocalPath,
                value: value.to_owned(),
                proof: None,
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    SeededLocation {
        asset_id: asset.id,
        version_id: version.id,
        location_id: location.id,
    }
}

async fn fresh_pool() -> (SqlitePool, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

async fn pending_commit_intent_count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM commit_intents WHERE state = 'pending'")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn aborted_commit_intent_state(pool: &SqlitePool, commit_id: CommitId) -> (String, String) {
    let row = sqlx::query("SELECT state, abort_reason FROM commit_intents WHERE id = ?")
        .bind(commit_id.0.cast_signed())
        .fetch_one(pool)
        .await
        .unwrap();
    let state: String = row.try_get("state").unwrap();
    let abort_reason: String = row.try_get("abort_reason").unwrap();
    (state, abort_reason)
}

async fn events_for_commit(pool: &SqlitePool, commit_id: CommitId) -> Vec<EventKind> {
    let events = SqliteEventRepo::new(pool.clone());
    let page = events
        .list(
            EventFilter {
                subject_type: Some(voom_events::SubjectType::CommitIntent),
                subject_id: Some(commit_id.0),
                ..EventFilter::default()
            },
            Page {
                limit: 20,
                cursor: None,
            },
        )
        .await
        .unwrap();
    page.items
        .iter()
        .map(|r| r.envelope.payload.kind())
        .collect()
}

/// Construct a default `DestructiveCommit` targeting `location_id` with
/// `DeleteFileLocation`. Used by every Phase A test except the
/// stale-evidence variant which carries `accepted_evidence_ids`.
fn delete_target_for(location_id: FileLocationId) -> DestructiveCommit {
    DestructiveCommit {
        target: CommitTarget::DeleteFileLocation(location_id),
        accepted_evidence_ids: Vec::new(),
    }
}

#[tokio::test]
async fn prepare_phase_a_success_lands_pending_row_plus_intent_recorded_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        delete_target_for(seeded.location_id),
        T0,
    )
    .await
    .unwrap();
    let intent = match outcome {
        PrepareOutcome::Pending(i) => i,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending, got Blocked({result:?})")
        }
    };
    // Closure must carry the asset, version, and location we seeded.
    assert!(
        intent
            .closure_initial
            .file_assets
            .contains(&seeded.asset_id)
    );
    assert!(
        intent
            .closure_initial
            .file_versions
            .contains(&seeded.version_id)
    );
    assert!(
        intent
            .closure_initial
            .file_locations
            .contains(&seeded.location_id)
    );
    // Pending row landed.
    assert_eq!(pending_commit_intent_count(&pool).await, 1);
    // scope_members expanded across all four granularities (3 in this
    // closure: asset + version + location; no bundle, since the asset
    // is not a member of any).
    let scope_member_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM commit_intent_scope_members WHERE commit_intent_id = ?",
    )
    .bind(intent.commit_id.0.cast_signed())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(scope_member_count, 3);
    // Event emitted on the same tx.
    let kinds = events_for_commit(&pool, intent.commit_id).await;
    assert!(kinds.contains(&EventKind::CommitIntentRecorded));
}

#[tokio::test]
async fn prepare_phase_a_blocked_by_use_lease_lands_aborted_row_plus_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let leases = SqliteUseLeaseRepo::new(pool.clone());
    // Place a blocking lease on the FileVersion (so the lease overlaps
    // the closure's Version granularity).
    leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Version(seeded.version_id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(time::Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        delete_target_for(seeded.location_id),
        T0 + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let (commit_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(i) => panic!("expected Blocked, got Pending({i:?})"),
    };
    assert!(matches!(result, CommitGateResult::BlockedByUseLease { .. }));
    // Aborted row landed with abort_reason='fresh_lease'.
    let (state, reason) = aborted_commit_intent_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(reason, "fresh_lease");
    // No pending row remains.
    assert_eq!(pending_commit_intent_count(&pool).await, 0);
    // Event emitted via the two-tx pattern.
    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedByUseLease),
        "expected CommitAbortedByUseLease in {kinds:?}"
    );
}

#[tokio::test]
async fn prepare_phase_a_advisory_lease_does_not_block() {
    // The blocking-lease query restricts to `blocking_mode = 'blocking'`.
    // An advisory lease must not cause a Phase A abort.
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let leases = SqliteUseLeaseRepo::new(pool.clone());
    leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Scan,
            scope: LeaseScope::Version(seeded.version_id),
            issuer_kind: IssuerKind::Worker,
            issuer_ref: "w-1".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: Some(time::Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        delete_target_for(seeded.location_id),
        T0,
    )
    .await
    .unwrap();
    assert!(matches!(outcome, PrepareOutcome::Pending(_)));
}

#[tokio::test]
async fn prepare_phase_a_blocked_by_stale_evidence_lands_aborted_row_plus_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());

    // Record evidence then accept it with a pin to the live version's
    // hash. Then mutate the hash via direct UPDATE so the pin drifts.
    let evidence = identity
        .record_identity_evidence_in_tx(
            &mut pool.begin().await.unwrap(),
            NewIdentityEvidence {
                target_type: crate::repo::identity::IdentityEvidenceTarget::FileVersion,
                target_id: seeded.version_id.0,
                assertion_type: voom_events::AssertionKind::HashMatch,
                candidate_id: None,
                candidate_value: None,
                provider: "test".to_owned(),
                provider_version: "0".to_owned(),
                confidence: 1.0,
                provenance: serde_json::json!({}),
                observed_at: T0,
            },
        )
        .await;
    // Open a fresh tx to record + accept evidence in one go.
    let mut tx = pool.begin().await.unwrap();
    let recorded = identity
        .record_identity_evidence_in_tx(
            &mut tx,
            NewIdentityEvidence {
                target_type: crate::repo::identity::IdentityEvidenceTarget::FileVersion,
                target_id: seeded.version_id.0,
                assertion_type: voom_events::AssertionKind::HashMatch,
                candidate_id: None,
                candidate_value: None,
                provider: "test".to_owned(),
                provider_version: "0".to_owned(),
                confidence: 1.0,
                provenance: serde_json::json!({}),
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    identity
        .accept_identity_evidence_in_tx(
            &mut tx,
            recorded.id,
            Some("alice".to_owned()),
            T0,
            AcceptedPin {
                file_version_ids: None,
                hashes: Some(serde_json::json!([[seeded.version_id.0, "hash-/srv/x"]])),
                locations: None,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let _ = evidence; // pre-flight call only used to ensure target schema exists; drop result.
    // Drift the hash on the version row to force `PinnedHashDiffers`.
    sqlx::query("UPDATE file_versions SET content_hash = 'drifted' WHERE id = ?")
        .bind(seeded.version_id.0.cast_signed())
        .execute(&pool)
        .await
        .unwrap();

    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(seeded.location_id),
            accepted_evidence_ids: vec![recorded.id],
        },
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap();
    let (commit_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(i) => panic!("expected Blocked, got Pending({i:?})"),
    };
    assert!(matches!(
        result,
        CommitGateResult::BlockedByStaleEvidence {
            drift: EvidenceDrift::PinnedHashDiffers,
            ..
        }
    ));
    let (state, reason) = aborted_commit_intent_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(reason, "stale_evidence");
    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedByStaleEvidence),
        "expected CommitAbortedByStaleEvidence in {kinds:?}"
    );
}

#[tokio::test]
async fn prepare_phase_a_blocked_by_closure_incomplete_lands_aborted_row_plus_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    // Configure the resolver to fail for this version_id.
    let resolver = crate::test_support::FailingAliasResolver::new([seeded.version_id]);

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        delete_target_for(seeded.location_id),
        T0 + time::Duration::seconds(3),
    )
    .await
    .unwrap();
    let (commit_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(i) => panic!("expected Blocked, got Pending({i:?})"),
    };
    assert!(matches!(
        result,
        CommitGateResult::BlockedByClosureIncomplete { .. }
    ));
    let (state, reason) = aborted_commit_intent_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(reason, "closure_incomplete");
    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedByClosureIncomplete),
        "expected CommitAbortedByClosureIncomplete in {kinds:?}"
    );
}

#[tokio::test]
async fn prepare_phase_a_missing_target_location_aborts_as_closure_incomplete() {
    // Defense-in-depth: a caller may target a location that does not
    // exist (e.g., stale operator handle). The closure walker surfaces
    // it as closure-incomplete rather than a generic NotFound, so the
    // audit trail carries the abort row.
    let (pool, _tmp) = fresh_pool().await;
    let _seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        delete_target_for(CoreFileLocationId(99_999)),
        T0,
    )
    .await
    .unwrap();
    let (commit_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(i) => panic!("expected Blocked, got Pending({i:?})"),
    };
    assert!(matches!(
        result,
        CommitGateResult::BlockedByClosureIncomplete { .. }
    ));
    let (state, reason) = aborted_commit_intent_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(reason, "closure_incomplete");
}

// -- authorize_destructive_commit sibling tests (commit 6) -----------------

async fn prepare_pending_intent(pool: &SqlitePool, location_id: FileLocationId) -> CommitId {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = prepare_destructive_commit(
        pool,
        &identity,
        &events,
        &resolver,
        delete_target_for(location_id),
        T0,
    )
    .await
    .unwrap();
    match outcome {
        PrepareOutcome::Pending(intent) => intent.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending after seed, got Blocked({result:?})")
        }
    }
}

async fn authorized_commit_intent_row(
    pool: &SqlitePool,
    commit_id: CommitId,
) -> (String, Option<String>, Option<String>) {
    let row = sqlx::query(
        "SELECT state, closure_authorized, target_row_epochs \
         FROM commit_intents WHERE id = ?",
    )
    .bind(commit_id.0.cast_signed())
    .fetch_one(pool)
    .await
    .unwrap();
    let state: String = row.try_get("state").unwrap();
    let closure_authorized: Option<String> = row.try_get("closure_authorized").unwrap();
    let target_row_epochs: Option<String> = row.try_get("target_row_epochs").unwrap();
    (state, closure_authorized, target_row_epochs)
}

#[tokio::test]
async fn authorize_phase_b_success_lands_authorized_row_plus_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let outcome = authorize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        commit_id,
        T0 + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let permit = match outcome {
        AuthorizeOutcome::Authorized(p) => p,
        AuthorizeOutcome::Blocked { result, .. } => {
            panic!("expected Authorized, got Blocked({result:?})")
        }
    };
    // Permit carries the authorized closure mirroring the seed chain.
    assert_eq!(permit.commit_id(), commit_id);
    assert!(
        permit
            .closure_authorized()
            .file_locations
            .contains(&seeded.location_id)
    );
    assert!(
        permit
            .closure_authorized()
            .file_versions
            .contains(&seeded.version_id)
    );
    assert!(
        permit
            .closure_authorized()
            .file_assets
            .contains(&seeded.asset_id)
    );
    // Durable row state.
    let (state, closure_json, epochs_json) = authorized_commit_intent_row(&pool, commit_id).await;
    assert_eq!(state, "authorized");
    let closure_json = closure_json.expect("authorized row has closure_authorized");
    // FileLocationId is a transparent newtype over u64 — serialized as a
    // bare number, not a string.
    assert!(
        closure_json.contains(&format!("{}", seeded.location_id.0)),
        "closure_json does not mention {}: {closure_json}",
        seeded.location_id.0,
    );
    // target_row_epochs populated with one [kind, id, epoch] triple per
    // member of the authorized closure. asset (1) + version (1) +
    // location (1) = 3 triples for this seed (no bundle membership).
    let epochs_json = epochs_json.expect("authorized row has target_row_epochs");
    let triples: serde_json::Value = serde_json::from_str(&epochs_json).unwrap();
    let arr = triples.as_array().unwrap();
    assert_eq!(
        arr.len(),
        permit.closure_authorized().file_assets.len()
            + permit.closure_authorized().file_versions.len()
            + permit.closure_authorized().file_locations.len()
            + permit.closure_authorized().bundles.len()
    );
    // Event emitted on the same tx.
    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAuthorized),
        "expected CommitAuthorized in {kinds:?}"
    );
}

#[tokio::test]
async fn authorize_phase_b_blocked_by_closure_incomplete_lands_aborted_row_plus_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    // Configure the resolver to fail for this version on the Phase B
    // recomputation. The prepare step seeded a Pending intent against a
    // healthy resolver; the same call site with a degraded resolver
    // surfaces the closure-incomplete trip-wire at Phase B.
    let resolver = crate::test_support::FailingAliasResolver::new([seeded.version_id]);

    let outcome = authorize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        commit_id,
        T0 + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let result = match outcome {
        AuthorizeOutcome::Blocked { result, .. } => result,
        AuthorizeOutcome::Authorized(p) => panic!("expected Blocked, got Authorized({p:?})"),
    };
    assert!(matches!(
        result,
        CommitGateResult::BlockedByClosureIncomplete { .. }
    ));
    let (state, reason) = aborted_commit_intent_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(reason, "closure_incomplete");
    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedByClosureIncomplete),
        "expected CommitAbortedByClosureIncomplete in {kinds:?}"
    );
}

#[tokio::test]
async fn authorize_phase_b_blocked_by_closure_grew_lands_aborted_row_plus_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;

    // Add a second live location on the same FileVersion out-of-band
    // between prepare and authorize. Phase B's closure walker enumerates
    // live locations on the version; the recomputed closure now carries
    // an `added_location` the Phase A snapshot did not.
    let identity = SqliteIdentityRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let _extra = identity
        .create_file_location_in_tx(
            &mut tx,
            IdentityNewFileLocation {
                file_version_id: seeded.version_id,
                kind: IdentityFileLocationKind::LocalPath,
                value: "/srv/x-alias".to_owned(),
                proof: None,
                observed_at: T0 + time::Duration::seconds(2),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let outcome = authorize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        commit_id,
        T0 + time::Duration::seconds(3),
    )
    .await
    .unwrap();
    let result = match outcome {
        AuthorizeOutcome::Blocked { result, .. } => result,
        AuthorizeOutcome::Authorized(p) => panic!("expected Blocked, got Authorized({p:?})"),
    };
    let delta = match result {
        CommitGateResult::BlockedByClosureGrew { delta } => delta,
        other => panic!("expected BlockedByClosureGrew, got {other:?}"),
    };
    assert!(!delta.added_locations.is_empty());
    let (state, reason) = aborted_commit_intent_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(reason, "closure_grew");
    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedByClosureGrew),
        "expected CommitAbortedByClosureGrew in {kinds:?}"
    );
}

#[tokio::test]
async fn authorize_phase_b_blocked_by_use_lease_lands_aborted_row_plus_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;

    // Fresh blocking lease appears between prepare and authorize. The
    // architectural exemption only excuses `reconcile_rename_in_tx`;
    // direct acquire-against-version is still legal, and the lock
    // helper at acquire_in_tx is permissive when no in-flight commit
    // covers the scope — that is the case here for an unrelated scope.
    // For the test we use `acquire_in_tx` directly against the version
    // scope; the pending lock IS armed but the test exercises the
    // post-lock world (the lease landed during a window where the lock
    // was disarmed). We bypass `acquire` and INSERT the lease directly
    // so the test surfaces only the Phase B trip-wire, not the lock.
    sqlx::query(
        "INSERT INTO asset_use_leases \
         (kind, scope_version_id, issuer_kind, issuer_ref, blocking_mode, \
          ttl_bound, clock_source, acquired_at, expires_at) \
         VALUES ('playback', ?, 'user', 'alice', 'blocking', 1, 'control_plane', ?, ?)",
    )
    .bind(seeded.version_id.0.cast_signed())
    .bind("2026-05-18T00:00:00Z")
    .bind("2026-05-19T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let outcome = authorize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        commit_id,
        T0 + time::Duration::seconds(4),
    )
    .await
    .unwrap();
    let result = match outcome {
        AuthorizeOutcome::Blocked { result, .. } => result,
        AuthorizeOutcome::Authorized(p) => panic!("expected Blocked, got Authorized({p:?})"),
    };
    assert!(matches!(result, CommitGateResult::BlockedByUseLease { .. }));
    let (state, reason) = aborted_commit_intent_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(reason, "fresh_lease");
    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedByUseLease),
        "expected CommitAbortedByUseLease in {kinds:?}"
    );
}

#[tokio::test]
async fn authorize_phase_b_blocked_by_stale_evidence_lands_aborted_row_plus_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());

    // Accept evidence pinning the live hash, then prepare carrying
    // that evidence id (so Phase A's revalidation passes).
    let mut tx = pool.begin().await.unwrap();
    let recorded = identity
        .record_identity_evidence_in_tx(
            &mut tx,
            NewIdentityEvidence {
                target_type: crate::repo::identity::IdentityEvidenceTarget::FileVersion,
                target_id: seeded.version_id.0,
                assertion_type: voom_events::AssertionKind::HashMatch,
                candidate_id: None,
                candidate_value: None,
                provider: "test".to_owned(),
                provider_version: "0".to_owned(),
                confidence: 1.0,
                provenance: serde_json::json!({}),
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    identity
        .accept_identity_evidence_in_tx(
            &mut tx,
            recorded.id,
            Some("alice".to_owned()),
            T0,
            AcceptedPin {
                file_version_ids: None,
                hashes: Some(serde_json::json!([[seeded.version_id.0, "hash-/srv/x"]])),
                locations: None,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(seeded.location_id),
            accepted_evidence_ids: vec![recorded.id],
        },
        T0,
    )
    .await
    .unwrap();
    let commit_id = match outcome {
        PrepareOutcome::Pending(i) => i.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending after seed, got Blocked({result:?})")
        }
    };

    // Drift the hash between prepare and authorize.
    sqlx::query("UPDATE file_versions SET content_hash = 'drifted' WHERE id = ?")
        .bind(seeded.version_id.0.cast_signed())
        .execute(&pool)
        .await
        .unwrap();

    let outcome = authorize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        commit_id,
        T0 + time::Duration::seconds(5),
    )
    .await
    .unwrap();
    let result = match outcome {
        AuthorizeOutcome::Blocked { result, .. } => result,
        AuthorizeOutcome::Authorized(p) => panic!("expected Blocked, got Authorized({p:?})"),
    };
    assert!(matches!(
        result,
        CommitGateResult::BlockedByStaleEvidence {
            drift: EvidenceDrift::PinnedHashDiffers,
            ..
        }
    ));
    let (state, reason) = aborted_commit_intent_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(reason, "stale_evidence");
    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedByStaleEvidence),
        "expected CommitAbortedByStaleEvidence in {kinds:?}"
    );
}

#[tokio::test]
async fn authorize_phase_b_already_authorized_row_returns_conflict() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    // First authorize lands `authorized`.
    let _ok = authorize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        commit_id,
        T0 + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    // Second authorize on the same commit_id is `Conflict`.
    let err = authorize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        commit_id,
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn authorize_phase_b_missing_row_returns_conflict() {
    let (pool, _tmp) = fresh_pool().await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let err =
        authorize_destructive_commit(&pool, &identity, &events, &resolver, CommitId(99_999), T0)
            .await
            .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn authorize_phase_b_aborted_row_returns_conflict() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    // Force a Phase A abort by configuring the resolver to fail.
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = crate::test_support::FailingAliasResolver::new([seeded.version_id]);
    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        delete_target_for(seeded.location_id),
        T0,
    )
    .await
    .unwrap();
    let commit_id = match outcome {
        PrepareOutcome::Blocked { commit_id, .. } => commit_id,
        PrepareOutcome::Pending(i) => panic!("expected Blocked, got Pending({i:?})"),
    };
    // Second authorize on the same aborted commit_id is `Conflict`.
    let resolver2 =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let err = authorize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver2,
        commit_id,
        T0 + time::Duration::seconds(1),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn commit_intents_check_accepts_authorized_with_closure_authorized_set() {
    // Positive control: the same row shape with closure_authorized
    // populated MUST be accepted. Without this control, a CHECK that
    // silently rejects every authorized row would still pass the
    // three negative tests above.
    let (pool, _tmp) = fresh_pool_for_schema_check().await;
    sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, closure_authorized, accepted_evidence_ids, state, \
          started_at, authorized_at, target_row_epochs) \
         VALUES ('{}', '{}', '{}', '[]', 'authorized', '2026-05-18T00:00:00Z', \
                 '2026-05-18T00:00:00Z', '[]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Confirm the row landed.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM commit_intents WHERE state = 'authorized'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

// -- finalize_destructive_commit sibling tests (commit 7) ------------------

async fn authorize_pending_intent(
    pool: &SqlitePool,
    commit_id: CommitId,
    now: time::OffsetDateTime,
) -> CommitPermit {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = authorize_destructive_commit(pool, &identity, &events, &resolver, commit_id, now)
        .await
        .unwrap();
    match outcome {
        AuthorizeOutcome::Authorized(p) => p,
        AuthorizeOutcome::Blocked { result, .. } => {
            panic!("expected Authorized, got Blocked({result:?})")
        }
    }
}

async fn finalized_commit_intent_row(
    pool: &SqlitePool,
    commit_id: CommitId,
) -> (String, Option<String>, Option<String>) {
    let row =
        sqlx::query("SELECT state, abort_reason, recovery_reason FROM commit_intents WHERE id = ?")
            .bind(commit_id.0.cast_signed())
            .fetch_one(pool)
            .await
            .unwrap();
    let state: String = row.try_get("state").unwrap();
    let abort_reason: Option<String> = row.try_get("abort_reason").unwrap();
    let recovery_reason: Option<String> = row.try_get("recovery_reason").unwrap();
    (state, abort_reason, recovery_reason)
}

#[tokio::test]
async fn finalize_phase_c_wrong_state_returns_conflict() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    // Row is in `pending` — finalize requires `authorized`.

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    // Fabricate a permit with the same commit_id (module-private fields
    // visible inside this sibling module). The real permit hasn't been
    // issued yet, but the conflict check fires on the row state before
    // the permit shape is consumed.
    let permit = CommitPermit {
        commit_id,
        authorized_at: T0,
        closure_authorized: AffectedScopeClosure::default(),
        evaluated_lease_ids: Vec::new(),
        revalidated_evidence: Vec::new(),
        epoch: 99,
    };

    let err = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn finalize_phase_c_wrong_epoch_returns_conflict() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;

    // Bump the row's epoch out-of-band so the permit's epoch is stale.
    sqlx::query("UPDATE commit_intents SET epoch = epoch + 1 WHERE id = ?")
        .bind(commit_id.0.cast_signed())
        .execute(&pool)
        .await
        .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let err = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn finalize_phase_c_not_performed_cancels_after_authorize() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::NotPerformed,
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::CancelledAfterAuthorize(o) => o,
        other => panic!("expected CancelledAfterAuthorize, got {other:?}"),
    };
    assert!(matches!(
        gate_outcome.result,
        CommitGateResult::CancelledAfterAuthorize
    ));
    // §9.3.2 Phase C step 2: closure_final mirrors the authorized closure.
    assert_eq!(gate_outcome.closure_final, gate_outcome.closure_authorized);

    let (state, abort_reason, recovery_reason) =
        finalized_commit_intent_row(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(abort_reason.as_deref(), Some("operator_cancel"));
    assert!(recovery_reason.is_none());

    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedPreMutation),
        "expected CommitAbortedPreMutation in {kinds:?}"
    );
    // The retired location's row must still be live — no FS mutation
    // means no durable identity mutation.
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(seeded.location_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_none());
}

#[tokio::test]
async fn finalize_phase_c_silent_delete_dispatches_retire() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Completed(o) => o,
        other => panic!("expected Completed, got {other:?}"),
    };
    assert!(matches!(gate_outcome.result, CommitGateResult::Allowed));

    let (state, abort_reason, recovery_reason) =
        finalized_commit_intent_row(&pool, commit_id).await;
    assert_eq!(state, "completed");
    assert!(abort_reason.is_none());
    assert!(recovery_reason.is_none());

    // The retired location row is now retired.
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(seeded.location_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_some());

    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitCompleted),
        "expected CommitCompleted in {kinds:?}"
    );
}

#[tokio::test]
async fn finalize_phase_c_silent_replace_dispatches_replace() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    // Prepare a ReplaceFileLocation target — the proposal carries no
    // file_version_id; Phase C reads it from the retired row.
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        DestructiveCommit {
            target: CommitTarget::ReplaceFileLocation {
                retired: seeded.location_id,
                new: FileLocationProposal {
                    kind: IdentityFileLocationKind::LocalPath,
                    value: "/srv/x-replaced".to_owned(),
                    proof: None,
                    observed_at: T0,
                },
            },
            accepted_evidence_ids: Vec::new(),
        },
        T0,
    )
    .await
    .unwrap();
    let commit_id = match outcome {
        PrepareOutcome::Pending(i) => i.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending, got Blocked({result:?})")
        }
    };
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;

    let outcome = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, FinalizeOutcome::Completed(_)));

    // Old location retired, new location live on the same FileVersion.
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(seeded.location_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_some());
    let new_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM file_locations \
         WHERE file_version_id = ? AND value = '/srv/x-replaced' AND retired_at IS NULL",
    )
    .bind(seeded.version_id.0.cast_signed())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(new_count, 1);
}

#[tokio::test]
async fn finalize_phase_c_silent_move_dispatches_replace() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        DestructiveCommit {
            target: CommitTarget::MoveFileLocation {
                retired: seeded.location_id,
                new: FileLocationProposal {
                    kind: IdentityFileLocationKind::LocalPath,
                    value: "/srv/x-moved".to_owned(),
                    proof: None,
                    observed_at: T0,
                },
            },
            accepted_evidence_ids: Vec::new(),
        },
        T0,
    )
    .await
    .unwrap();
    let commit_id = match outcome {
        PrepareOutcome::Pending(i) => i.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending, got Blocked({result:?})")
        }
    };
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;

    let outcome = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, FinalizeOutcome::Completed(_)));
    // The MoveFileLocation variant uses the same dispatch as
    // ReplaceFileLocation — both route through replace_file_location_in_tx.
    let new_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM file_locations \
         WHERE file_version_id = ? AND value = '/srv/x-moved' AND retired_at IS NULL",
    )
    .bind(seeded.version_id.0.cast_signed())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(new_count, 1);
}

#[tokio::test]
async fn finalize_phase_c_closure_grew_drives_recovery_required() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;

    // Between authorize and finalize, add a second live FileLocation on
    // the same FileVersion. The Phase C closure recompute sees the
    // added alias.
    let identity = SqliteIdentityRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let _extra = identity
        .create_file_location_in_tx(
            &mut tx,
            IdentityNewFileLocation {
                file_version_id: seeded.version_id,
                kind: IdentityFileLocationKind::LocalPath,
                value: "/srv/x-late-alias".to_owned(),
                proof: None,
                observed_at: T0 + time::Duration::seconds(2),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    let delta = match gate_outcome.result {
        CommitGateResult::BlockedByClosureGrew { delta } => delta,
        other => panic!("expected BlockedByClosureGrew, got {other:?}"),
    };
    assert!(!delta.added_locations.is_empty());

    let (state, abort_reason, recovery_reason) =
        finalized_commit_intent_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(
        abort_reason.is_none(),
        "recovery_required must have NULL abort_reason"
    );
    assert_eq!(recovery_reason.as_deref(), Some("closure_grew"));

    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedPostMutation),
        "expected CommitAbortedPostMutation in {kinds:?}"
    );
    assert!(
        kinds.contains(&EventKind::CommitRecoveryRequired),
        "expected CommitRecoveryRequired in {kinds:?}"
    );

    // The retired location row must still be live — Phase C did not
    // apply the durable mutation on a trip-wire branch.
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(seeded.location_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_none());
}

#[tokio::test]
async fn finalize_phase_c_fresh_lease_drives_recovery_required() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;

    // Direct INSERT of a blocking lease on the version scope so the
    // Phase C blocking-lease recheck fires without any closure drift.
    // (`acquire_in_tx` would hit the pending-commit lock; we exercise
    // the trip-wire in isolation, like the Phase B fresh-lease sibling
    // test.)
    sqlx::query(
        "INSERT INTO asset_use_leases \
         (kind, scope_version_id, issuer_kind, issuer_ref, blocking_mode, \
          ttl_bound, clock_source, acquired_at, expires_at) \
         VALUES ('playback', ?, 'user', 'alice', 'blocking', 1, 'control_plane', ?, ?)",
    )
    .bind(seeded.version_id.0.cast_signed())
    .bind("2026-05-18T00:00:00Z")
    .bind("2026-05-19T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    assert!(matches!(
        gate_outcome.result,
        CommitGateResult::BlockedByUseLease { .. }
    ));

    let (state, abort_reason, recovery_reason) =
        finalized_commit_intent_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(recovery_reason.as_deref(), Some("fresh_lease"));

    let kinds = events_for_commit(&pool, commit_id).await;
    assert!(
        kinds.contains(&EventKind::CommitAbortedPostMutation),
        "expected CommitAbortedPostMutation in {kinds:?}"
    );
}

#[tokio::test]
async fn finalize_phase_c_closure_grew_and_fresh_lease_drives_recovery_required() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;

    // Both wires fire: add a fresh location AND a fresh blocking lease
    // between authorize and finalize.
    let identity = SqliteIdentityRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let _extra = identity
        .create_file_location_in_tx(
            &mut tx,
            IdentityNewFileLocation {
                file_version_id: seeded.version_id,
                kind: IdentityFileLocationKind::LocalPath,
                value: "/srv/x-late-alias".to_owned(),
                proof: None,
                observed_at: T0 + time::Duration::seconds(2),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    sqlx::query(
        "INSERT INTO asset_use_leases \
         (kind, scope_version_id, issuer_kind, issuer_ref, blocking_mode, \
          ttl_bound, clock_source, acquired_at, expires_at) \
         VALUES ('playback', ?, 'user', 'alice', 'blocking', 1, 'control_plane', ?, ?)",
    )
    .bind(seeded.version_id.0.cast_signed())
    .bind("2026-05-18T00:00:00Z")
    .bind("2026-05-19T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    // Spec §9.3.2 step 3 third bullet: combined trip-wire returns
    // BlockedByClosureGrew (closure shift is the dominant signal).
    assert!(matches!(
        gate_outcome.result,
        CommitGateResult::BlockedByClosureGrew { .. }
    ));

    let (state, abort_reason, recovery_reason) =
        finalized_commit_intent_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(
        recovery_reason.as_deref(),
        Some("closure_grew_and_fresh_lease")
    );
}

#[tokio::test]
async fn finalize_phase_c_stale_target_epoch_drives_recovery_required() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;

    // Bump the target location's epoch between authorize and finalize.
    // The closure recompute still observes the same member set, the
    // blocking-lease query still empty — but the per-member epoch
    // comparison now drifts on the snapshotted target row.
    sqlx::query("UPDATE file_locations SET epoch = epoch + 1 WHERE id = ?")
        .bind(seeded.location_id.0.cast_signed())
        .execute(&pool)
        .await
        .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    let drift = match gate_outcome.result {
        CommitGateResult::BlockedByStaleTargetEpoch { drift } => drift,
        other => panic!("expected BlockedByStaleTargetEpoch, got {other:?}"),
    };
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].kind, TargetMemberKind::FileLocation);
    assert_eq!(drift[0].id, seeded.location_id.0);

    let (state, abort_reason, recovery_reason) =
        finalized_commit_intent_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(recovery_reason.as_deref(), Some("stale_target_epoch"));

    // Durable mutation must NOT have run on the trip-wire branch.
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(seeded.location_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_none());
}

// -- abort_destructive_commit sibling tests (commit 8) ---------------------

/// Read the first `commit.aborted_pre_mutation` event for `commit_id`
/// and return `(prior_state, reason)` from the payload. Sibling tests
/// use this to assert the caller-initiated abort entry writes
/// `prior_state = 'pending'` (distinguishing it from the Phase C
/// `NotPerformed` branch, which shares the event kind but writes
/// `prior_state = 'authorized'`).
async fn first_aborted_pre_mutation_payload(
    pool: &SqlitePool,
    commit_id: CommitId,
) -> (String, String) {
    let events = SqliteEventRepo::new(pool.clone());
    let page = events
        .list(
            EventFilter {
                kind: Some(EventKind::CommitAbortedPreMutation),
                subject_type: Some(voom_events::SubjectType::CommitIntent),
                subject_id: Some(commit_id.0),
            },
            Page {
                limit: 20,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let row = page
        .items
        .first()
        .expect("expected one CommitAbortedPreMutation event");
    match &row.envelope.payload {
        voom_events::Event::CommitAbortedPreMutation(p) => {
            (p.prior_state.clone(), p.reason.clone())
        }
        other => panic!("expected CommitAbortedPreMutation payload, got {other:?}"),
    }
}

async fn commit_intent_state_and_epoch(
    pool: &SqlitePool,
    commit_id: CommitId,
) -> (String, Option<String>, Option<String>, u64) {
    let row = sqlx::query(
        "SELECT state, abort_reason, aborted_at, epoch FROM commit_intents WHERE id = ?",
    )
    .bind(commit_id.0.cast_signed())
    .fetch_one(pool)
    .await
    .unwrap();
    let state: String = row.try_get("state").unwrap();
    let abort_reason: Option<String> = row.try_get("abort_reason").unwrap();
    let aborted_at: Option<String> = row.try_get("aborted_at").unwrap();
    let epoch_raw: i64 = row.try_get("epoch").unwrap();
    (state, abort_reason, aborted_at, u64_from_i64(epoch_raw))
}

#[tokio::test]
async fn abort_pending_transitions_to_aborted_and_emits_pre_mutation_event() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let (_, _, _, epoch_before) = commit_intent_state_and_epoch(&pool, commit_id).await;

    let events = SqliteEventRepo::new(pool.clone());
    let outcome = abort_destructive_commit(
        &pool,
        &events,
        commit_id,
        AbortReason::OperatorCancel,
        T0 + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let new_epoch = match outcome {
        AbortOutcome::Aborted {
            commit_id: c,
            epoch,
        } => {
            assert_eq!(c, commit_id);
            epoch
        }
    };

    let (state, abort_reason, aborted_at, epoch_after) =
        commit_intent_state_and_epoch(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(abort_reason.as_deref(), Some("operator_cancel"));
    assert!(aborted_at.is_some(), "aborted_at must be populated");
    assert_eq!(
        epoch_after,
        epoch_before + 1,
        "epoch must bump exactly once"
    );
    assert_eq!(epoch_after, new_epoch, "outcome epoch must match row epoch");

    let (prior_state, reason) = first_aborted_pre_mutation_payload(&pool, commit_id).await;
    assert_eq!(prior_state, "pending");
    assert_eq!(reason, "operator_cancel");
}

#[tokio::test]
async fn abort_authorized_row_rejects_with_conflict() {
    // Recovery contract: the only sanctioned post-authorize termination
    // is finalize(_, NotPerformed). abort_destructive_commit must not
    // accept an authorized row.
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let _permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;
    let (_, _, _, epoch_before) = commit_intent_state_and_epoch(&pool, commit_id).await;

    let events = SqliteEventRepo::new(pool.clone());
    let err = abort_destructive_commit(
        &pool,
        &events,
        commit_id,
        AbortReason::OperatorCancel,
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");

    // Row state and epoch must be untouched.
    let (state, abort_reason, _, epoch_after) =
        commit_intent_state_and_epoch(&pool, commit_id).await;
    assert_eq!(state, "authorized");
    assert!(abort_reason.is_none());
    assert_eq!(epoch_after, epoch_before);
}

#[tokio::test]
async fn abort_already_aborted_row_rejects_with_conflict() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let events = SqliteEventRepo::new(pool.clone());
    abort_destructive_commit(
        &pool,
        &events,
        commit_id,
        AbortReason::OperatorCancel,
        T0 + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    // Row is now in `aborted` — second call must reject.
    let err = abort_destructive_commit(
        &pool,
        &events,
        commit_id,
        AbortReason::OperatorCancel,
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn abort_completed_row_rejects_with_conflict() {
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap();
    // Row is now in `completed` — terminal state, must reject.
    let err = abort_destructive_commit(
        &pool,
        &events,
        commit_id,
        AbortReason::OperatorCancel,
        T0 + time::Duration::seconds(3),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn abort_recovery_required_row_rejects_with_conflict() {
    // Drive a row into `recovery_required` via the Phase C
    // stale-target-epoch trip-wire, then assert abort rejects.
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let permit = authorize_pending_intent(&pool, commit_id, T0 + time::Duration::seconds(1)).await;
    sqlx::query("UPDATE file_locations SET epoch = epoch + 1 WHERE id = ?")
        .bind(seeded.location_id.0.cast_signed())
        .execute(&pool)
        .await
        .unwrap();
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver =
        crate::test_support::FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    finalize_destructive_commit(
        &pool,
        &identity,
        &events,
        &resolver,
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap();
    // Row is now in `recovery_required` — terminal-for-abort, must reject.
    let err = abort_destructive_commit(
        &pool,
        &events,
        commit_id,
        AbortReason::OperatorCancel,
        T0 + time::Duration::seconds(3),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn abort_missing_row_rejects_with_conflict() {
    let (pool, _tmp) = fresh_pool().await;
    let events = SqliteEventRepo::new(pool.clone());
    let err = abort_destructive_commit(
        &pool,
        &events,
        CommitId(9_999),
        AbortReason::OperatorCancel,
        T0,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn abort_rejects_gate_driven_reason_variants_without_touching_row() {
    // Caller-initiated abort accepts only pre-mutation variants the
    // gate does not itself drive. Passing a gate-driven variant
    // (FreshLease, ClosureGrew, ...) or the post-mutation-only
    // StaleTargetEpoch must surface VoomError::Config without writing.
    let (pool, _tmp) = fresh_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let commit_id = prepare_pending_intent(&pool, seeded.location_id).await;
    let (_, _, _, epoch_before) = commit_intent_state_and_epoch(&pool, commit_id).await;
    let events = SqliteEventRepo::new(pool.clone());

    for reason in [
        AbortReason::FreshLease,
        AbortReason::ClosureGrew,
        AbortReason::ClosureIncomplete,
        AbortReason::StaleEvidence,
        AbortReason::StaleTargetEpoch,
    ] {
        let err = abort_destructive_commit(
            &pool,
            &events,
            commit_id,
            reason.clone(),
            T0 + time::Duration::seconds(1),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, VoomError::Config(_)),
            "reason {reason:?}: got {err:?}"
        );
    }

    // Row must still be `pending` and epoch untouched after the rejections.
    let (state, abort_reason, _, epoch_after) =
        commit_intent_state_and_epoch(&pool, commit_id).await;
    assert_eq!(state, "pending");
    assert!(abort_reason.is_none());
    assert_eq!(epoch_after, epoch_before);
}

#[tokio::test]
async fn abort_accepts_mutation_failed_and_other_variants() {
    // Sanctioned non-OperatorCancel pre-mutation variants must succeed
    // and round-trip their snake_case tag into both the durable
    // abort_reason column and the event payload's `reason` field.
    let (pool, _tmp) = fresh_pool().await;
    let events = SqliteEventRepo::new(pool.clone());

    let seeded_a = seed_location(&pool, "/srv/a").await;
    let commit_a = prepare_pending_intent(&pool, seeded_a.location_id).await;
    abort_destructive_commit(
        &pool,
        &events,
        commit_a,
        AbortReason::MutationFailed,
        T0 + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let (state_a, reason_a, _, _) = commit_intent_state_and_epoch(&pool, commit_a).await;
    assert_eq!(state_a, "aborted");
    assert_eq!(reason_a.as_deref(), Some("mutation_failed"));
    let (prior_a, payload_reason_a) = first_aborted_pre_mutation_payload(&pool, commit_a).await;
    assert_eq!(prior_a, "pending");
    assert_eq!(payload_reason_a, "mutation_failed");

    let seeded_b = seed_location(&pool, "/srv/b").await;
    let commit_b = prepare_pending_intent(&pool, seeded_b.location_id).await;
    abort_destructive_commit(
        &pool,
        &events,
        commit_b,
        AbortReason::Other("custom note".to_owned()),
        T0 + time::Duration::seconds(2),
    )
    .await
    .unwrap();
    let (state_b, reason_b, _, _) = commit_intent_state_and_epoch(&pool, commit_b).await;
    assert_eq!(state_b, "aborted");
    assert_eq!(reason_b.as_deref(), Some("other"));
    let (prior_b, payload_reason_b) = first_aborted_pre_mutation_payload(&pool, commit_b).await;
    assert_eq!(prior_b, "pending");
    assert_eq!(payload_reason_b, "other");
}
