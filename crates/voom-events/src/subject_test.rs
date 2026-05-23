use super::*;

#[test]
fn subject_type_wire_strings() {
    assert_eq!(SubjectType::System.as_str(), "system");
    assert_eq!(SubjectType::Job.as_str(), "job");
    assert_eq!(SubjectType::Ticket.as_str(), "ticket");
    assert_eq!(SubjectType::Lease.as_str(), "lease");
    assert_eq!(SubjectType::Node.as_str(), "node");
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
        SubjectType::Node,
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

#[test]
fn identity_layer_subjects_round_trip() {
    for s in [
        SubjectType::MediaWork,
        SubjectType::MediaVariant,
        SubjectType::AssetBundle,
        SubjectType::FileAsset,
        SubjectType::FileVersion,
        SubjectType::FileLocation,
        SubjectType::IdentityEvidence,
        SubjectType::MediaSnapshot,
        SubjectType::AssetUseLease,
    ] {
        let wire = s.as_str();
        let back = SubjectType::from_str(wire).unwrap();
        assert_eq!(s, back, "round-trip failed for {wire}");
    }
}

#[test]
fn commit_safety_gate_subject_round_trips() {
    let s = SubjectType::CommitIntent;
    assert_eq!(s.as_str(), "commit_intent");
    let back = SubjectType::from_str("commit_intent").unwrap();
    assert_eq!(back, s);
}

#[test]
fn identity_layer_subjects_use_expected_wire_format() {
    assert_eq!(SubjectType::MediaWork.as_str(), "media_work");
    assert_eq!(SubjectType::MediaVariant.as_str(), "media_variant");
    assert_eq!(SubjectType::AssetBundle.as_str(), "asset_bundle");
    assert_eq!(SubjectType::FileAsset.as_str(), "file_asset");
    assert_eq!(SubjectType::FileVersion.as_str(), "file_version");
    assert_eq!(SubjectType::FileLocation.as_str(), "file_location");
    assert_eq!(SubjectType::IdentityEvidence.as_str(), "identity_evidence");
    assert_eq!(SubjectType::MediaSnapshot.as_str(), "media_snapshot");
    assert_eq!(SubjectType::AssetUseLease.as_str(), "asset_use_lease");
}
