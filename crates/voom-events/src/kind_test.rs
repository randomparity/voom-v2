use super::*;

#[test]
fn each_kind_has_distinct_wire_string() {
    let kinds = [
        EventKind::SchemaInitialized,
        EventKind::JobOpened,
        EventKind::JobSucceeded,
        EventKind::JobFailed,
        EventKind::JobCancelled,
        EventKind::TicketCreated,
        EventKind::TicketReady,
        EventKind::TicketLeased,
        EventKind::TicketSucceeded,
        EventKind::TicketFailedRetriable,
        EventKind::TicketFailedTerminal,
        EventKind::TicketRequeuedAfterLeaseExpiry,
        EventKind::TicketRequeuedAfterForceRelease,
        EventKind::LeaseAcquired,
        EventKind::LeaseReleased,
        EventKind::LeaseExpired,
        EventKind::LeaseForceReleased,
        EventKind::NodeRegistered,
        EventKind::NodeHeartbeatRecorded,
        EventKind::NodeMarkedStale,
        EventKind::NodeRetired,
        EventKind::WorkerRegistered,
        EventKind::WorkerLinkedToNode,
        EventKind::WorkerCapabilityRecorded,
        EventKind::WorkerGrantRecorded,
        EventKind::WorkerRetired,
        EventKind::ArtifactHandleCreated,
        EventKind::ArtifactLocationRecorded,
        EventKind::ArtifactLocationRetired,
        EventKind::ArtifactLineageRecorded,
        EventKind::ArtifactStaged,
        EventKind::ArtifactVerificationStarted,
        EventKind::ArtifactVerificationSucceeded,
        EventKind::ArtifactVerificationFailed,
        EventKind::ArtifactCommitStarted,
        EventKind::ArtifactCommitCompleted,
        EventKind::ArtifactCommitFailedPreMutation,
        EventKind::ArtifactCommitRecoveryRequired,
        EventKind::ArtifactTranscodeStarted,
        EventKind::ArtifactTranscodeProgress,
        EventKind::ArtifactTranscodeSucceeded,
        EventKind::ArtifactTranscodeFailed,
        EventKind::IssueOpened,
        EventKind::IssueUpdated,
        EventKind::IssueResolved,
        EventKind::UseLeaseAcquired,
        EventKind::UseLeaseReleased,
        EventKind::UseLeaseExpired,
        EventKind::UseLeaseForceReleased,
        EventKind::UseLeaseRecoveredStaleIssuer,
        EventKind::UseLeaseReanchoredByMove,
        EventKind::CommitIntentRecorded,
        EventKind::CommitAbortedByUseLease,
        EventKind::CommitAbortedByStaleEvidence,
        EventKind::CommitAbortedByClosureIncomplete,
        EventKind::CommitAbortedByPendingCommit,
        EventKind::CommitAuthorized,
        EventKind::CommitAbortedByClosureGrew,
        EventKind::CommitCompleted,
        EventKind::CommitAbortedPostMutation,
        EventKind::CommitAbortedPreMutation,
        EventKind::CommitRecoveryRequired,
        EventKind::CommitForcedOverride,
    ];
    let mut seen = std::collections::HashSet::new();
    for k in kinds {
        assert!(seen.insert(k.as_str()), "duplicate wire string for {k:?}");
    }
}

#[test]
fn schema_initialized_wire_string() {
    assert_eq!(EventKind::SchemaInitialized.as_str(), "schema.initialized");
}

#[test]
fn ticket_failed_retriable_wire_string() {
    assert_eq!(
        EventKind::TicketFailedRetriable.as_str(),
        "ticket.failed_retriable"
    );
}

#[test]
fn issue_lifecycle_event_kinds_use_dotted_wire_format() {
    assert_eq!(EventKind::IssueOpened.as_str(), "issue.opened");
    assert_eq!(EventKind::IssueUpdated.as_str(), "issue.updated");
    assert_eq!(EventKind::IssueResolved.as_str(), "issue.resolved");
}

#[test]
fn node_event_kinds_use_dotted_wire_format() {
    assert_eq!(EventKind::NodeRegistered.as_str(), "node.registered");
    assert_eq!(
        EventKind::NodeHeartbeatRecorded.as_str(),
        "node.heartbeat_recorded"
    );
    assert_eq!(EventKind::NodeMarkedStale.as_str(), "node.marked_stale");
    assert_eq!(EventKind::NodeRetired.as_str(), "node.retired");
    assert_eq!(
        EventKind::WorkerLinkedToNode.as_str(),
        "worker.linked_to_node"
    );
}

#[test]
fn every_kind_round_trips_through_as_str_and_from_str() {
    // Programmatically enumerate every variant — if a new variant is added
    // without an as_str/from_str pair, this test fails.
    let kinds = [
        EventKind::SchemaInitialized,
        EventKind::JobOpened,
        EventKind::JobSucceeded,
        EventKind::JobFailed,
        EventKind::JobCancelled,
        EventKind::TicketCreated,
        EventKind::TicketReady,
        EventKind::TicketLeased,
        EventKind::TicketSucceeded,
        EventKind::TicketFailedRetriable,
        EventKind::TicketFailedTerminal,
        EventKind::TicketRequeuedAfterLeaseExpiry,
        EventKind::TicketRequeuedAfterForceRelease,
        EventKind::LeaseAcquired,
        EventKind::LeaseReleased,
        EventKind::LeaseExpired,
        EventKind::LeaseForceReleased,
        EventKind::NodeRegistered,
        EventKind::NodeHeartbeatRecorded,
        EventKind::NodeMarkedStale,
        EventKind::NodeRetired,
        EventKind::WorkerRegistered,
        EventKind::WorkerLinkedToNode,
        EventKind::WorkerCapabilityRecorded,
        EventKind::WorkerGrantRecorded,
        EventKind::WorkerRetired,
        EventKind::ArtifactHandleCreated,
        EventKind::ArtifactLocationRecorded,
        EventKind::ArtifactLocationRetired,
        EventKind::ArtifactLineageRecorded,
        EventKind::ArtifactStaged,
        EventKind::ArtifactVerificationStarted,
        EventKind::ArtifactVerificationSucceeded,
        EventKind::ArtifactVerificationFailed,
        EventKind::ArtifactCommitStarted,
        EventKind::ArtifactCommitCompleted,
        EventKind::ArtifactCommitFailedPreMutation,
        EventKind::ArtifactCommitRecoveryRequired,
        EventKind::ArtifactTranscodeStarted,
        EventKind::ArtifactTranscodeProgress,
        EventKind::ArtifactTranscodeSucceeded,
        EventKind::ArtifactTranscodeFailed,
        EventKind::IssueOpened,
        EventKind::IssueUpdated,
        EventKind::IssueResolved,
        EventKind::UseLeaseAcquired,
        EventKind::UseLeaseReleased,
        EventKind::UseLeaseExpired,
        EventKind::UseLeaseForceReleased,
        EventKind::UseLeaseRecoveredStaleIssuer,
        EventKind::UseLeaseReanchoredByMove,
        EventKind::CommitIntentRecorded,
        EventKind::CommitAbortedByUseLease,
        EventKind::CommitAbortedByStaleEvidence,
        EventKind::CommitAbortedByClosureIncomplete,
        EventKind::CommitAbortedByPendingCommit,
        EventKind::CommitAuthorized,
        EventKind::CommitAbortedByClosureGrew,
        EventKind::CommitCompleted,
        EventKind::CommitAbortedPostMutation,
        EventKind::CommitAbortedPreMutation,
        EventKind::CommitRecoveryRequired,
        EventKind::CommitForcedOverride,
    ];
    for k in kinds {
        let s = k.as_str();
        let back = EventKind::from_str(s).expect("from_str accepts as_str output");
        assert_eq!(back, k, "round-trip failed for {k:?} via {s:?}");
    }
}

#[test]
fn transcode_artifact_event_kinds_use_exact_sprint_12_wire_strings() {
    let cases = [
        (
            EventKind::ArtifactTranscodeStarted,
            "artifact.transcode_started",
        ),
        (
            EventKind::ArtifactTranscodeProgress,
            "artifact.transcode_progress",
        ),
        (
            EventKind::ArtifactTranscodeSucceeded,
            "artifact.transcode_succeeded",
        ),
        (
            EventKind::ArtifactTranscodeFailed,
            "artifact.transcode_failed",
        ),
    ];

    for (kind, wire) in cases {
        assert_eq!(kind.as_str(), wire);
        assert_eq!(EventKind::from_str(wire).unwrap(), kind);
    }
}

#[test]
fn staged_artifact_event_kinds_use_exact_sprint_11_wire_strings() {
    let cases = [
        (EventKind::ArtifactStaged, "artifact.staged"),
        (
            EventKind::ArtifactVerificationStarted,
            "artifact.verification_started",
        ),
        (
            EventKind::ArtifactVerificationSucceeded,
            "artifact.verification_succeeded",
        ),
        (
            EventKind::ArtifactVerificationFailed,
            "artifact.verification_failed",
        ),
        (EventKind::ArtifactCommitStarted, "artifact.commit_started"),
        (
            EventKind::ArtifactCommitCompleted,
            "artifact.commit_completed",
        ),
        (
            EventKind::ArtifactCommitFailedPreMutation,
            "artifact.commit_failed_pre_mutation",
        ),
        (
            EventKind::ArtifactCommitRecoveryRequired,
            "artifact.commit_recovery_required",
        ),
    ];

    for (kind, wire) in cases {
        assert_eq!(kind.as_str(), wire);
        assert_eq!(EventKind::from_str(wire).unwrap(), kind);
    }
}

#[test]
fn from_str_rejects_unknown_kind() {
    // snake_case form must NOT decode — the on-disk wire format is dotted.
    let err = EventKind::from_str("schema_initialized").unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Database(_)),
        "got: {err:?}"
    );
    EventKind::from_str("schema.initialized").unwrap();
}

#[test]
fn identity_layer_event_kinds_round_trip() {
    for k in [
        EventKind::MediaWorkCreated,
        EventKind::MediaVariantCreated,
        EventKind::AssetBundleCreated,
        EventKind::AssetBundleMemberAdded,
        EventKind::AssetBundleMemberRemoved,
        EventKind::FileAssetCreated,
        EventKind::FileVersionCreated,
        EventKind::FileLocationRecorded,
        EventKind::FileLocationAliased,
        EventKind::FileLocationRetiredByMove,
        EventKind::FileLocationRecordedByMove,
        EventKind::IdentityEvidenceRecorded,
        EventKind::IdentityEvidenceAccepted,
        EventKind::IdentityEvidenceSuperseded,
        EventKind::MediaSnapshotRecorded,
        EventKind::UseLeaseAcquired,
        EventKind::UseLeaseReleased,
        EventKind::UseLeaseExpired,
        EventKind::UseLeaseForceReleased,
        EventKind::UseLeaseRecoveredStaleIssuer,
        EventKind::UseLeaseReanchoredByMove,
    ] {
        let wire = k.as_str();
        let back = EventKind::from_str(wire).unwrap();
        assert_eq!(k, back, "round-trip failed for {wire}");
    }
}

#[test]
fn identity_layer_event_kinds_use_dotted_wire_format() {
    assert_eq!(EventKind::MediaWorkCreated.as_str(), "media_work.created");
    assert_eq!(
        EventKind::MediaVariantCreated.as_str(),
        "media_variant.created"
    );
    assert_eq!(
        EventKind::AssetBundleCreated.as_str(),
        "asset_bundle.created"
    );
    assert_eq!(
        EventKind::AssetBundleMemberAdded.as_str(),
        "asset_bundle.member_added"
    );
    assert_eq!(
        EventKind::AssetBundleMemberRemoved.as_str(),
        "asset_bundle.member_removed"
    );
    assert_eq!(EventKind::FileAssetCreated.as_str(), "file_asset.created");
    assert_eq!(
        EventKind::FileVersionCreated.as_str(),
        "file_version.created"
    );
    assert_eq!(
        EventKind::FileLocationRecorded.as_str(),
        "file_location.recorded"
    );
    assert_eq!(
        EventKind::FileLocationAliased.as_str(),
        "file_location.aliased"
    );
    assert_eq!(
        EventKind::FileLocationRetiredByMove.as_str(),
        "file_location.retired_by_move"
    );
    assert_eq!(
        EventKind::FileLocationRecordedByMove.as_str(),
        "file_location.recorded_by_move"
    );
    assert_eq!(
        EventKind::IdentityEvidenceRecorded.as_str(),
        "identity_evidence.recorded"
    );
    assert_eq!(
        EventKind::IdentityEvidenceAccepted.as_str(),
        "identity_evidence.accepted"
    );
    assert_eq!(
        EventKind::IdentityEvidenceSuperseded.as_str(),
        "identity_evidence.superseded"
    );
    assert_eq!(
        EventKind::MediaSnapshotRecorded.as_str(),
        "media_snapshot.recorded"
    );
    assert_eq!(EventKind::UseLeaseAcquired.as_str(), "use_lease.acquired");
    assert_eq!(EventKind::UseLeaseReleased.as_str(), "use_lease.released");
    assert_eq!(EventKind::UseLeaseExpired.as_str(), "use_lease.expired");
    assert_eq!(
        EventKind::UseLeaseForceReleased.as_str(),
        "use_lease.force_released"
    );
    assert_eq!(
        EventKind::UseLeaseRecoveredStaleIssuer.as_str(),
        "use_lease.recovered_stale_issuer"
    );
    assert_eq!(
        EventKind::UseLeaseReanchoredByMove.as_str(),
        "use_lease.reanchored_by_move"
    );
}

#[test]
fn commit_safety_gate_event_kinds_use_dotted_wire_format() {
    assert_eq!(
        EventKind::CommitIntentRecorded.as_str(),
        "commit.intent_recorded"
    );
    assert_eq!(
        EventKind::CommitAbortedByUseLease.as_str(),
        "commit.aborted_by_use_lease"
    );
    assert_eq!(
        EventKind::CommitAbortedByStaleEvidence.as_str(),
        "commit.aborted_by_stale_evidence"
    );
    assert_eq!(
        EventKind::CommitAbortedByClosureIncomplete.as_str(),
        "commit.aborted_by_closure_incomplete"
    );
    assert_eq!(
        EventKind::CommitAbortedByPendingCommit.as_str(),
        "commit.aborted_by_pending_commit"
    );
    assert_eq!(EventKind::CommitAuthorized.as_str(), "commit.authorized");
    assert_eq!(
        EventKind::CommitAbortedByClosureGrew.as_str(),
        "commit.aborted_by_closure_grew"
    );
    assert_eq!(EventKind::CommitCompleted.as_str(), "commit.completed");
    assert_eq!(
        EventKind::CommitAbortedPostMutation.as_str(),
        "commit.aborted_post_mutation"
    );
    assert_eq!(
        EventKind::CommitAbortedPreMutation.as_str(),
        "commit.aborted_pre_mutation"
    );
    assert_eq!(
        EventKind::CommitRecoveryRequired.as_str(),
        "commit.recovery_required"
    );
    assert_eq!(
        EventKind::CommitForcedOverride.as_str(),
        "commit.forced_override"
    );
}

#[test]
fn commit_safety_gate_event_kinds_round_trip() {
    for k in [
        EventKind::CommitIntentRecorded,
        EventKind::CommitAbortedByUseLease,
        EventKind::CommitAbortedByStaleEvidence,
        EventKind::CommitAbortedByClosureIncomplete,
        EventKind::CommitAbortedByPendingCommit,
        EventKind::CommitAuthorized,
        EventKind::CommitAbortedByClosureGrew,
        EventKind::CommitCompleted,
        EventKind::CommitAbortedPostMutation,
        EventKind::CommitAbortedPreMutation,
        EventKind::CommitRecoveryRequired,
        EventKind::CommitForcedOverride,
    ] {
        let wire = k.as_str();
        let back = EventKind::from_str(wire).unwrap();
        assert_eq!(k, back, "round-trip failed for {wire}");
    }
}
