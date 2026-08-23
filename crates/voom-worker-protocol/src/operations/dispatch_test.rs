use super::*;

fn source(root: u64, locator: &str) -> MediaSourceRef {
    MediaSourceRef {
        storage_root_id: voom_core::StorageRootId(root),
        provider_relative_locator: ProviderRelativeLocator::new(locator.to_owned()).unwrap(),
    }
}

fn planned(root: u64, locator: &str) -> MediaPlannedOutput {
    MediaPlannedOutput {
        storage_root_id: voom_core::StorageRootId(root),
        provider_relative_locator: ProviderRelativeLocator::new(locator.to_owned()).unwrap(),
        overwrite: false,
    }
}

fn expected() -> ExpectedFileFacts {
    ExpectedFileFacts {
        size_bytes: 128,
        content_hash: "blake3:abc".to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

#[test]
fn probe_envelope_round_trips_with_operation_tag() {
    let dispatch = MediaDispatch::Probe(MediaProbeDispatch {
        schema: PROTOCOL_VERSION,
        source: source(7, "movies/a.mkv"),
        expected: expected(),
    });
    let encoded = serde_json::to_value(&dispatch).unwrap();
    assert_eq!(encoded["operation"], "probe");
    assert_eq!(encoded["schema"], PROTOCOL_VERSION);
    let decoded = decode_media_dispatch(&encoded).unwrap();
    assert_eq!(decoded, dispatch);
}

#[test]
fn decode_rejects_unknown_operation_tag() {
    let payload = serde_json::json!({
        "operation": "teleport",
        "schema": PROTOCOL_VERSION,
    });
    let error = decode_media_dispatch(&payload).unwrap_err();
    assert!(error.contains("unknown variant"), "{error}");
}

#[test]
fn decode_rejects_wrong_schema_before_any_use() {
    let dispatch = MediaDispatch::BackUpFile(MediaBackUpFileDispatch {
        schema: PROTOCOL_VERSION + 1,
        source: source(7, "movies/a.mkv"),
        destination: planned(9, "backup/v1/a.mkv"),
    });
    let error = decode_media_dispatch(&serde_json::to_value(&dispatch).unwrap()).unwrap_err();
    assert!(error.contains("does not match protocol version"), "{error}");
}

#[test]
fn decode_rejects_unknown_fields_on_content_struct() {
    let mut payload = serde_json::to_value(MediaDispatch::Remux(MediaRemuxDispatch {
        schema: PROTOCOL_VERSION,
        source: source(7, "movies/a.mkv"),
        expected: crate::operations::remux::RemuxExpectedFacts {
            size_bytes: 1,
            content_hash: "blake3:abc".to_owned(),
            modified_at: None,
            local_file_key: None,
        },
        output_container: "mkv".to_owned(),
        output: planned(8, "staging/a.mkv"),
        selection: crate::operations::remux::RemuxSelection {
            keep_streams: vec![],
            default_streams: vec![],
            clear_default_streams: vec![],
            track_order: vec![],
            head_streams: vec![],
            forced_streams: vec![],
            clear_forced_streams: vec![],
        },
    }))
    .unwrap();
    payload["surprise"] = serde_json::Value::Bool(true);
    let error = decode_media_dispatch(&payload).unwrap_err();
    assert!(
        error.contains("unknown field `surprise`"),
        "deny-unknown-fields must reject the former field: {error}"
    );
}

#[test]
fn extract_audio_dispatch_round_trips_ordered_outputs() {
    let dispatch = MediaDispatch::ExtractAudio(MediaExtractAudioDispatch {
        schema: PROTOCOL_VERSION,
        source: source(3, "movies/a.mkv"),
        expected: AudioExpectedFacts {
            size_bytes: 10,
            content_hash: "blake3:abc".to_owned(),
            modified_at: None,
            local_file_key: None,
        },
        output_container: "ogg".to_owned(),
        outputs: vec![MediaExtractOutput {
            output_id: "out-1".to_owned(),
            selection: AudioStreamRef {
                snapshot_stream_id: "s-1".to_owned(),
                provider_stream_index: 0,
            },
            audio_codec: "opus".to_owned(),
            output: planned(4, "staging/a-1.ogg"),
        }],
    });
    let decoded = decode_media_dispatch(&serde_json::to_value(&dispatch).unwrap()).unwrap();
    assert_eq!(decoded, dispatch);
}

#[test]
fn stage_source_dispatch_round_trips() {
    let dispatch = MediaDispatch::StageSource(MediaStageSourceDispatch {
        schema: PROTOCOL_VERSION,
        source: source(5, "library/incoming/a.mkv"),
        expected: expected(),
        target: planned(6, "staging/v2/a.mkv"),
    });
    let decoded = decode_media_dispatch(&serde_json::to_value(&dispatch).unwrap()).unwrap();
    assert_eq!(decoded, dispatch);
}

#[test]
fn transcode_audio_additive_settings_round_trip() {
    let dispatch = MediaDispatch::TranscodeAudio(MediaTranscodeAudioDispatch {
        schema: PROTOCOL_VERSION,
        source: source(3, "movies/a.mkv"),
        expected: AudioExpectedFacts {
            size_bytes: 10,
            content_hash: "blake3:abc".to_owned(),
            modified_at: None,
            local_file_key: None,
        },
        output_container: "mkv".to_owned(),
        output: planned(4, "staging/a.mkv"),
        selection: TranscodeAudioSelection {
            selected_streams: vec![],
        },
        settings: TranscodeAudioSettings {
            target_codec: "eac3".to_owned(),
            profile: "default".to_owned(),
            add_track: true,
            target_channels: Some(2),
        },
    });
    let decoded = decode_media_dispatch(&serde_json::to_value(&dispatch).unwrap()).unwrap();
    assert_eq!(decoded, dispatch);
}
