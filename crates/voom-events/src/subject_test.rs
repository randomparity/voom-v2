use super::*;

#[test]
fn subject_type_wire_strings() {
    assert_eq!(SubjectType::System.as_str(), "system");
    assert_eq!(SubjectType::Job.as_str(), "job");
    assert_eq!(SubjectType::Ticket.as_str(), "ticket");
    assert_eq!(SubjectType::Lease.as_str(), "lease");
    assert_eq!(SubjectType::Worker.as_str(), "worker");
    assert_eq!(SubjectType::ArtifactHandle.as_str(), "artifact_handle");
    assert_eq!(SubjectType::ArtifactLocation.as_str(), "artifact_location");
}

#[test]
fn every_subject_round_trips_through_as_str_and_from_str() {
    let subjects = [
        SubjectType::System,
        SubjectType::Job,
        SubjectType::Ticket,
        SubjectType::Lease,
        SubjectType::Worker,
        SubjectType::ArtifactHandle,
        SubjectType::ArtifactLocation,
    ];
    for s in subjects {
        let wire = s.as_str();
        let back = SubjectType::from_str(wire).expect("from_str accepts as_str output");
        assert_eq!(back, s, "round-trip failed for {s:?} via {wire:?}");
    }
}

#[test]
fn from_str_rejects_unknown_subject() {
    let err = SubjectType::from_str("not_a_subject").unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Database(_)),
        "got: {err:?}"
    );
}
