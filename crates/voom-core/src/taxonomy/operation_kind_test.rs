use super::OperationKind;

#[test]
fn all_contains_every_operation_kind_once() {
    use std::collections::HashSet;

    let all = OperationKind::ALL;
    assert_eq!(all.len(), 15);
    let unique = all.iter().copied().collect::<HashSet<_>>();
    assert_eq!(unique.len(), all.len());
    assert!(unique.contains(&OperationKind::ScanLibrary));
    assert!(unique.contains(&OperationKind::ProbeFile));
    assert!(unique.contains(&OperationKind::HashFile));
    assert!(unique.contains(&OperationKind::IdentifyMedia));
    assert!(unique.contains(&OperationKind::ScoreQuality));
    assert!(unique.contains(&OperationKind::SyncExternalSystem));
    assert!(unique.contains(&OperationKind::BackUpFile));
    assert!(unique.contains(&OperationKind::Remux));
    assert!(unique.contains(&OperationKind::TranscodeVideo));
    assert!(unique.contains(&OperationKind::TranscodeAudio));
    assert!(unique.contains(&OperationKind::EditTracks));
    assert!(unique.contains(&OperationKind::ExtractAudio));
    assert!(unique.contains(&OperationKind::VerifyArtifact));
    assert!(unique.contains(&OperationKind::CommitArtifact));
    assert!(unique.contains(&OperationKind::DeleteArtifact));
}

#[test]
fn all_operation_kinds_round_trip_through_wire_names() {
    for operation in OperationKind::ALL {
        let encoded = serde_json::to_string(operation).unwrap();
        let decoded: OperationKind = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, *operation);
    }
}

#[test]
fn every_variant_round_trips_snake_case() {
    let cases: &[(OperationKind, &str)] = &[
        (OperationKind::ScanLibrary, "scan_library"),
        (OperationKind::ProbeFile, "probe_file"),
        (OperationKind::HashFile, "hash_file"),
        (OperationKind::IdentifyMedia, "identify_media"),
        (OperationKind::ScoreQuality, "score_quality"),
        (OperationKind::SyncExternalSystem, "sync_external_system"),
        (OperationKind::BackUpFile, "back_up_file"),
        (OperationKind::Remux, "remux"),
        (OperationKind::TranscodeVideo, "transcode_video"),
        (OperationKind::TranscodeAudio, "transcode_audio"),
        (OperationKind::EditTracks, "edit_tracks"),
        (OperationKind::ExtractAudio, "extract_audio"),
        (OperationKind::VerifyArtifact, "verify_artifact"),
        (OperationKind::CommitArtifact, "commit_artifact"),
        (OperationKind::DeleteArtifact, "delete_artifact"),
    ];
    for (variant, expected) in cases {
        let json = serde_json::to_string(variant).unwrap();
        assert_eq!(json, format!("\"{expected}\""), "encode of {variant:?}");
        let decoded: OperationKind = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, *variant, "decode of {expected}");
    }
}

#[test]
fn transcode_audio_serializes_to_stable_wire_name() {
    let json = serde_json::to_string(&OperationKind::TranscodeAudio).unwrap();

    assert_eq!(json, "\"transcode_audio\"");
}

#[test]
fn extract_audio_deserializes_from_stable_wire_name() {
    let operation: OperationKind = serde_json::from_str("\"extract_audio\"").unwrap();

    assert_eq!(operation, OperationKind::ExtractAudio);
}

#[test]
fn from_wire_round_trips_every_variant_and_rejects_unknown() {
    for operation in OperationKind::ALL {
        assert_eq!(
            OperationKind::from_wire(operation.as_str()),
            Some(*operation)
        );
    }
    assert_eq!(OperationKind::from_wire("unknown_op"), None);
    assert_eq!(OperationKind::from_wire("transcribe_audio"), None);
    assert_eq!(OperationKind::from_wire("ScanLibrary"), None);
    assert_eq!(OperationKind::from_wire(""), None);
}

#[test]
fn byte_touching_classification_is_the_documented_split() {
    let not_byte_touching: &[OperationKind] = &[
        OperationKind::IdentifyMedia,
        OperationKind::ScoreQuality,
        OperationKind::SyncExternalSystem,
    ];
    for operation in OperationKind::ALL {
        let expected = !not_byte_touching.contains(operation);
        assert_eq!(
            operation.is_byte_touching(),
            expected,
            "classification of {operation:?}"
        );
    }
    assert_eq!(
        OperationKind::ALL
            .iter()
            .filter(|operation| operation.is_byte_touching())
            .count(),
        12
    );
}

#[test]
fn scan_library_is_byte_touching_and_identify_media_is_not() {
    // scan_library enumerates a root's contents, which #421 moves to the owner
    // node; identify_media derives from facts an earlier operation recorded.
    assert!(OperationKind::ScanLibrary.is_byte_touching());
    assert!(!OperationKind::IdentifyMedia.is_byte_touching());
}

#[test]
fn unknown_string_fails_to_deserialize() {
    let res: Result<OperationKind, _> = serde_json::from_str("\"unknown_op\"");
    assert!(res.is_err(), "unknown_op should not deserialize");
}

#[test]
fn removed_transcribe_audio_wire_name_fails_to_deserialize() {
    let res: Result<OperationKind, _> = serde_json::from_str("\"transcribe_audio\"");
    assert!(res.is_err(), "transcribe_audio should not deserialize");
}

#[test]
fn camel_case_string_fails_to_deserialize() {
    // serde with rename_all = "snake_case" rejects the variant's Rust name.
    let res: Result<OperationKind, _> = serde_json::from_str("\"ScanLibrary\"");
    assert!(res.is_err(), "ScanLibrary should not deserialize");
}
