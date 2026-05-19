use super::OperationKind;

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
        (OperationKind::EditTracks, "edit_tracks"),
        (OperationKind::ExtractAudio, "extract_audio"),
        (OperationKind::TranscribeAudio, "transcribe_audio"),
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
fn unknown_string_fails_to_deserialize() {
    let res: Result<OperationKind, _> = serde_json::from_str("\"unknown_op\"");
    assert!(res.is_err(), "unknown_op should not deserialize");
}

#[test]
fn camel_case_string_fails_to_deserialize() {
    // serde with rename_all = "snake_case" rejects the variant's Rust name.
    let res: Result<OperationKind, _> = serde_json::from_str("\"ScanLibrary\"");
    assert!(res.is_err(), "ScanLibrary should not deserialize");
}
