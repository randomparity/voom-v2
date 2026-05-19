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
