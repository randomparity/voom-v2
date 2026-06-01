use super::*;
use crate::payload::{Event, EventKind};
use time::OffsetDateTime;
#[test]
fn commit_intent_recorded_round_trip() {
    let p = CommitIntentRecordedPayload {
        commit_id: voom_core::CommitId(11),
        target_kind: "delete_file_location".to_owned(),
        closure_asset_count: 1,
        closure_bundle_count: 0,
        closure_version_count: 1,
        closure_location_count: 1,
        accepted_evidence_count: 0,
        started_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitIntentRecorded(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.intent_recorded");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitIntentRecorded(q) if q == p));
    assert_eq!(
        Event::CommitIntentRecorded(p).kind(),
        EventKind::CommitIntentRecorded
    );
}

#[test]
fn commit_aborted_by_use_lease_round_trip() {
    let p = CommitAbortedByUseLeasePayload {
        commit_id: voom_core::CommitId(12),
        lease_id: voom_core::UseLeaseId(3),
        lease_scope_type: "version".to_owned(),
        lease_scope_id: 99,
        phase: "prepare".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByUseLease(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_use_lease");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByUseLease(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByUseLease(p).kind(),
        EventKind::CommitAbortedByUseLease
    );
}

#[test]
fn commit_aborted_by_stale_evidence_round_trip() {
    let p = CommitAbortedByStaleEvidencePayload {
        commit_id: voom_core::CommitId(13),
        evidence_id: voom_core::EvidenceId(7),
        drift_kind: "pinned_hash_differs".to_owned(),
        phase: "prepare".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByStaleEvidence(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_stale_evidence");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByStaleEvidence(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByStaleEvidence(p).kind(),
        EventKind::CommitAbortedByStaleEvidence
    );
}

#[test]
fn commit_aborted_by_closure_incomplete_round_trip() {
    let p = CommitAbortedByClosureIncompletePayload {
        commit_id: voom_core::CommitId(14),
        phase: "prepare".to_owned(),
        message: "mount /srv/media offline".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByClosureIncomplete(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_closure_incomplete");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByClosureIncomplete(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByClosureIncomplete(p).kind(),
        EventKind::CommitAbortedByClosureIncomplete
    );
}

#[test]
fn commit_aborted_by_pending_commit_round_trip() {
    let p = CommitAbortedByPendingCommitPayload {
        commit_id: voom_core::CommitId(21),
        pending_commit_id: voom_core::CommitId(20),
        scope_type: "location".to_owned(),
        scope_id: 99,
        phase: "prepare".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByPendingCommit(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_pending_commit");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByPendingCommit(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByPendingCommit(p).kind(),
        EventKind::CommitAbortedByPendingCommit
    );
}

#[test]
fn commit_authorized_round_trip() {
    let p = CommitAuthorizedPayload {
        commit_id: voom_core::CommitId(21),
        closure_asset_count: 1,
        closure_bundle_count: 0,
        closure_version_count: 1,
        closure_location_count: 2,
        target_row_epoch_count: 4,
        authorized_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAuthorized(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.authorized");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAuthorized(q) if q == p));
    assert_eq!(
        Event::CommitAuthorized(p).kind(),
        EventKind::CommitAuthorized
    );
}

#[test]
fn commit_aborted_by_closure_grew_round_trip() {
    let p = CommitAbortedByClosureGrewPayload {
        commit_id: voom_core::CommitId(22),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 1,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 1,
        phase: "authorize".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByClosureGrew(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_closure_grew");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByClosureGrew(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByClosureGrew(p).kind(),
        EventKind::CommitAbortedByClosureGrew
    );
}

#[test]
fn commit_completed_round_trip() {
    let p = CommitCompletedPayload {
        commit_id: voom_core::CommitId(31),
        target_kind: "delete_file_location".to_owned(),
        closure_asset_count: 1,
        closure_bundle_count: 0,
        closure_version_count: 1,
        closure_location_count: 1,
        finalized_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitCompleted(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.completed");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitCompleted(q) if q == p));
    assert_eq!(Event::CommitCompleted(p).kind(), EventKind::CommitCompleted);
}

#[test]
fn commit_aborted_pre_mutation_round_trip_carries_prior_state() {
    // Two emission sites — `prior_state` distinguishes them so a single
    // event kind covers both abort entry points.
    let p_pending = CommitAbortedPreMutationPayload {
        commit_id: voom_core::CommitId(32),
        prior_state: "pending".to_owned(),
        reason: "operator_cancel".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedPreMutation(p_pending.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_pre_mutation");
    assert_eq!(json["payload"]["prior_state"], "pending");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedPreMutation(q) if q == p_pending));

    let p_authorized = CommitAbortedPreMutationPayload {
        commit_id: voom_core::CommitId(33),
        prior_state: "authorized".to_owned(),
        reason: "operator_cancel".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedPreMutation(p_authorized.clone())).unwrap();
    assert_eq!(json["payload"]["prior_state"], "authorized");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedPreMutation(q) if q == p_authorized));
    assert_eq!(
        Event::CommitAbortedPreMutation(p_authorized).kind(),
        EventKind::CommitAbortedPreMutation
    );
}

#[test]
fn commit_aborted_post_mutation_round_trip_unified_schema() {
    let p = CommitAbortedPostMutationPayload {
        commit_id: voom_core::CommitId(34),
        reason: "closure_grew_and_fresh_lease".to_owned(),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 1,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 0,
        fresh_lease_ids: vec![7, 9],
        target_epoch_drift: Vec::new(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedPostMutation(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_post_mutation");
    // Both arrays must be present on every payload (unified schema) —
    // a closure-grew firing carries an empty `fresh_lease_ids`; a
    // fresh-lease firing carries empty `added_*`/`removed_*`.
    assert!(json["payload"]["fresh_lease_ids"].is_array());
    assert!(json["payload"]["target_epoch_drift"].is_array());
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedPostMutation(q) if q == p));
    assert_eq!(
        Event::CommitAbortedPostMutation(p).kind(),
        EventKind::CommitAbortedPostMutation
    );
}

#[test]
fn commit_aborted_post_mutation_stale_target_epoch_carries_drift_array() {
    let p = CommitAbortedPostMutationPayload {
        commit_id: voom_core::CommitId(35),
        reason: "stale_target_epoch".to_owned(),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 0,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 0,
        fresh_lease_ids: Vec::new(),
        target_epoch_drift: vec![TargetEpochDriftWire {
            kind: "file_location".to_owned(),
            id: 17,
            expected: 4,
            observed: 5,
        }],
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedPostMutation(p.clone())).unwrap();
    assert_eq!(json["payload"]["reason"], "stale_target_epoch");
    assert_eq!(
        json["payload"]["target_epoch_drift"][0]["kind"],
        "file_location"
    );
    assert_eq!(json["payload"]["target_epoch_drift"][0]["id"], 17);
    assert_eq!(json["payload"]["target_epoch_drift"][0]["expected"], 4);
    assert_eq!(json["payload"]["target_epoch_drift"][0]["observed"], 5);
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedPostMutation(q) if q == p));
}

#[test]
fn commit_forced_override_round_trip() {
    let p = CommitForcedOverridePayload {
        commit_id: voom_core::CommitId(40),
        actor: "ops@example.com".to_owned(),
        reason: "fs mount offline; out-of-band confirmed".to_owned(),
        bypass: vec!["closure_incomplete".to_owned()],
        recorded_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitForcedOverride(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.forced_override");
    assert_eq!(json["payload"]["actor"], "ops@example.com");
    assert_eq!(json["payload"]["bypass"][0], "closure_incomplete");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitForcedOverride(q) if q == p));
    assert_eq!(
        Event::CommitForcedOverride(p).kind(),
        EventKind::CommitForcedOverride
    );
}

#[test]
fn commit_recovery_required_round_trip_mirrors_post_mutation_fields() {
    let p = CommitRecoveryRequiredPayload {
        commit_id: voom_core::CommitId(36),
        recovery_reason: "stale_target_epoch".to_owned(),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 0,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 0,
        fresh_lease_ids: Vec::new(),
        target_epoch_drift: vec![TargetEpochDriftWire {
            kind: "file_version".to_owned(),
            id: 7,
            expected: 1,
            observed: 2,
        }],
        recorded_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitRecoveryRequired(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.recovery_required");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitRecoveryRequired(q) if q == p));
    assert_eq!(
        Event::CommitRecoveryRequired(p).kind(),
        EventKind::CommitRecoveryRequired
    );
}
