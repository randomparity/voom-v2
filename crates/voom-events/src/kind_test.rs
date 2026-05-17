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
