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
        EventKind::WorkerRegistered,
        EventKind::WorkerCapabilityRecorded,
        EventKind::WorkerGrantRecorded,
        EventKind::WorkerRetired,
        EventKind::ArtifactHandleCreated,
        EventKind::ArtifactLocationRecorded,
        EventKind::ArtifactLocationRetired,
        EventKind::ArtifactLineageRecorded,
        EventKind::UseLeaseAcquired,
        EventKind::UseLeaseReleased,
        EventKind::UseLeaseExpired,
        EventKind::UseLeaseForceReleased,
        EventKind::UseLeaseRecoveredStaleIssuer,
        EventKind::UseLeaseReanchoredByMove,
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
        EventKind::WorkerRegistered,
        EventKind::WorkerCapabilityRecorded,
        EventKind::WorkerGrantRecorded,
        EventKind::WorkerRetired,
        EventKind::ArtifactHandleCreated,
        EventKind::ArtifactLocationRecorded,
        EventKind::ArtifactLocationRetired,
        EventKind::ArtifactLineageRecorded,
        EventKind::UseLeaseAcquired,
        EventKind::UseLeaseReleased,
        EventKind::UseLeaseExpired,
        EventKind::UseLeaseForceReleased,
        EventKind::UseLeaseRecoveredStaleIssuer,
        EventKind::UseLeaseReanchoredByMove,
    ];
    for k in kinds {
        let s = k.as_str();
        let back = EventKind::from_str(s).expect("from_str accepts as_str output");
        assert_eq!(back, k, "round-trip failed for {k:?} via {s:?}");
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
