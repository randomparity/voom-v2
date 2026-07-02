use super::*;

#[test]
fn transcode_audio_request_serializes_selected_streams_wire_shape() {
    let request = TranscodeAudioRequest {
        input: TranscodeAudioInput {
            path: "/library/input.mkv".to_owned(),
            expected: AudioExpectedFacts {
                size_bytes: 1234,
                content_hash: "blake3:abc".to_owned(),
                modified_at: Some("2026-05-26T00:00:00Z".to_owned()),
                local_file_key: None,
            },
        },
        output: TranscodeAudioOutput {
            staging_root: "/tmp/voom-stage".to_owned(),
            path: "/tmp/voom-stage/ticket-1/lease-1/input.audio-opus.mkv".to_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: TranscodeAudioSelection {
            selected_streams: vec![
                AudioStreamRef {
                    snapshot_stream_id: "stream-1".to_owned(),
                    provider_stream_index: 1,
                },
                AudioStreamRef {
                    snapshot_stream_id: "stream-3".to_owned(),
                    provider_stream_index: 3,
                },
            ],
        },
        audio: TranscodeAudioSettings {
            target_codec: "opus".to_owned(),
            profile: "default-opus".to_owned(),
        },
    };

    let json = serde_json::to_value(&request).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "input": {
                "path": "/library/input.mkv",
                "expected": {
                    "size_bytes": 1234,
                    "content_hash": "blake3:abc",
                    "modified_at": "2026-05-26T00:00:00Z",
                    "local_file_key": null
                }
            },
            "output": {
                "staging_root": "/tmp/voom-stage",
                "path": "/tmp/voom-stage/ticket-1/lease-1/input.audio-opus.mkv",
                "container": "mkv",
                "overwrite": false
            },
            "selection": {
                "selected_streams": [
                    {
                        "snapshot_stream_id": "stream-1",
                        "provider_stream_index": 1
                    },
                    {
                        "snapshot_stream_id": "stream-3",
                        "provider_stream_index": 3
                    }
                ]
            },
            "audio": {
                "target_codec": "opus",
                "profile": "default-opus"
            }
        })
    );
}

#[test]
fn transcode_audio_result_serializes_selected_output_streams_in_request_order() {
    let result = TranscodeAudioResult {
        status: TranscodeAudioStatus::Transcoded,
        provider: "ffmpeg".to_owned(),
        provider_version: "ffmpeg version 7.0".to_owned(),
        input_pre: observed_facts("blake3:input-before"),
        input_post: observed_facts("blake3:input-after"),
        output: observed_facts("blake3:output"),
        output_container: "mkv".to_owned(),
        selected_snapshot_stream_ids: vec!["stream-1".to_owned(), "stream-3".to_owned()],
        output_audio_codecs: vec!["opus".to_owned(), "opus".to_owned()],
        selected_output_streams: vec![
            AudioOutputStreamFact {
                snapshot_stream_id: "stream-1".to_owned(),
                output_provider_stream_index: 1,
                codec: "opus".to_owned(),
                language: Some("eng".to_owned()),
                title: Some("Main".to_owned()),
                default: Some(true),
                disposition: Some(AudioDispositionFact {
                    default: Some(true),
                    forced: Some(false),
                    commentary: Some(false),
                }),
                channels: Some(6),
            },
            AudioOutputStreamFact {
                snapshot_stream_id: "stream-3".to_owned(),
                output_provider_stream_index: 3,
                codec: "opus".to_owned(),
                language: None,
                title: None,
                default: Some(false),
                disposition: Some(AudioDispositionFact {
                    default: Some(false),
                    forced: Some(false),
                    commentary: Some(true),
                }),
                channels: None,
            },
        ],
    };

    let json = serde_json::to_value(&result).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "status": "transcoded",
            "provider": "ffmpeg",
            "provider_version": "ffmpeg version 7.0",
            "input_pre": {
                "size_bytes": 1234,
                "content_hash": "blake3:input-before"
            },
            "input_post": {
                "size_bytes": 1234,
                "content_hash": "blake3:input-after"
            },
            "output": {
                "size_bytes": 1234,
                "content_hash": "blake3:output"
            },
            "output_container": "mkv",
            "selected_snapshot_stream_ids": ["stream-1", "stream-3"],
            "output_audio_codecs": ["opus", "opus"],
            "selected_output_streams": [
                {
                    "snapshot_stream_id": "stream-1",
                    "output_provider_stream_index": 1,
                    "codec": "opus",
                    "language": "eng",
                    "title": "Main",
                    "default": true,
                    "disposition": {
                        "default": true,
                        "forced": false,
                        "commentary": false
                    },
                    "channels": 6
                },
                {
                    "snapshot_stream_id": "stream-3",
                    "output_provider_stream_index": 3,
                    "codec": "opus",
                    "language": null,
                    "title": null,
                    "default": false,
                    "disposition": {
                        "default": false,
                        "forced": false,
                        "commentary": true
                    },
                    "channels": null
                }
            ]
        })
    );
}

#[test]
fn transcode_audio_result_rejects_unknown_fields() {
    let err = serde_json::from_value::<TranscodeAudioResult>(serde_json::json!({
        "status": "transcoded",
        "provider": "ffmpeg",
        "provider_version": "ffmpeg version 7.0",
        "input_pre": { "size_bytes": 1234, "content_hash": "blake3:input-before" },
        "input_post": { "size_bytes": 1234, "content_hash": "blake3:input-after" },
        "output": { "size_bytes": 987, "content_hash": "blake3:output" },
        "output_container": "mkv",
        "selected_snapshot_stream_ids": ["stream-1"],
        "output_audio_codecs": ["opus"],
        "selected_output_streams": [
            {
                "snapshot_stream_id": "stream-1",
                "output_provider_stream_index": 1,
                "codec": "opus",
                "language": "eng",
                "title": "Main",
                "default": true,
                "disposition": {
                    "default": true,
                    "forced": false,
                    "commentary": false
                },
                "channels": 6
            }
        ],
        "unexpected": true
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn transcode_audio_selected_output_streams_reject_unknown_fields() {
    let err = serde_json::from_value::<AudioOutputStreamFact>(serde_json::json!({
        "snapshot_stream_id": "stream-1",
        "output_provider_stream_index": 1,
        "codec": "opus",
        "language": "eng",
        "title": "Main",
        "default": true,
        "disposition": {
            "default": true,
            "forced": false,
            "commentary": false
        },
        "channels": 6,
        "unexpected": true
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn audio_disposition_rejects_unknown_fields() {
    let err = serde_json::from_value::<AudioDispositionFact>(serde_json::json!({
        "default": true,
        "forced": false,
        "commentary": false,
        "unexpected": true
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn extract_audio_request_rejects_unknown_fields() {
    let err = serde_json::from_value::<ExtractAudioRequest>(serde_json::json!({
        "input": {
            "path": "/library/input.mkv",
            "expected": {
                "size_bytes": 1234,
                "content_hash": "blake3:abc",
                "modified_at": null,
                "local_file_key": null
            }
        },
        "output": {
            "staging_root": "/tmp/voom-stage",
            "path": "/tmp/voom-stage/ticket-2/lease-1/input.commentary.opus.ogg",
            "container": "ogg",
            "audio_codec": "opus",
            "overwrite": false
        },
        "selection": {
            "snapshot_stream_id": "stream-3",
            "provider_stream_index": 3
        },
        "unexpected": true
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn audio_stream_ref_rejects_unknown_fields() {
    let err = serde_json::from_value::<AudioStreamRef>(serde_json::json!({
        "snapshot_stream_id": "stream-3",
        "provider_stream_index": 3,
        "unexpected": true
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn audio_expected_facts_reject_unknown_fields() {
    let err = serde_json::from_value::<AudioExpectedFacts>(serde_json::json!({
        "size_bytes": 1234,
        "content_hash": "blake3:abc",
        "modified_at": null,
        "local_file_key": null,
        "unexpected": true
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn audio_observed_facts_reject_unknown_fields() {
    let err = serde_json::from_value::<AudioObservedFacts>(serde_json::json!({
        "size_bytes": 1234,
        "content_hash": "blake3:abc",
        "unexpected": true
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn extract_audio_request_serializes_one_selected_stream_wire_shape() {
    let request = ExtractAudioRequest {
        input: ExtractAudioInput {
            path: "/library/input.mkv".to_owned(),
            expected: AudioExpectedFacts {
                size_bytes: 1234,
                content_hash: "blake3:abc".to_owned(),
                modified_at: Some("2026-05-26T00:00:00Z".to_owned()),
                local_file_key: None,
            },
        },
        output: ExtractAudioOutput {
            staging_root: "/tmp/voom-stage".to_owned(),
            path: "/tmp/voom-stage/ticket-2/lease-1/input.commentary.opus.ogg".to_owned(),
            container: "ogg".to_owned(),
            audio_codec: "opus".to_owned(),
            overwrite: false,
        },
        selection: AudioStreamRef {
            snapshot_stream_id: "stream-3".to_owned(),
            provider_stream_index: 3,
        },
    };

    let json = serde_json::to_value(&request).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "input": {
                "path": "/library/input.mkv",
                "expected": {
                    "size_bytes": 1234,
                    "content_hash": "blake3:abc",
                    "modified_at": "2026-05-26T00:00:00Z",
                    "local_file_key": null
                }
            },
            "output": {
                "staging_root": "/tmp/voom-stage",
                "path": "/tmp/voom-stage/ticket-2/lease-1/input.commentary.opus.ogg",
                "container": "ogg",
                "audio_codec": "opus",
                "overwrite": false
            },
            "selection": {
                "snapshot_stream_id": "stream-3",
                "provider_stream_index": 3
            }
        })
    );
}

#[test]
fn extract_audio_result_serializes_selected_stream_and_output_facts() {
    let result = ExtractAudioResult {
        status: ExtractAudioStatus::Extracted,
        provider: "ffmpeg".to_owned(),
        provider_version: "ffmpeg version 7.0".to_owned(),
        input_pre: observed_facts("blake3:input-before"),
        input_post: observed_facts("blake3:input-after"),
        output: observed_facts("blake3:output"),
        output_container: "ogg".to_owned(),
        output_audio_codec: "opus".to_owned(),
        selected_snapshot_stream_id: "stream-3".to_owned(),
        output_language: Some("eng".to_owned()),
        output_title: Some("Commentary".to_owned()),
    };

    let json = serde_json::to_value(&result).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "status": "extracted",
            "provider": "ffmpeg",
            "provider_version": "ffmpeg version 7.0",
            "input_pre": {
                "size_bytes": 1234,
                "content_hash": "blake3:input-before"
            },
            "input_post": {
                "size_bytes": 1234,
                "content_hash": "blake3:input-after"
            },
            "output": {
                "size_bytes": 1234,
                "content_hash": "blake3:output"
            },
            "output_container": "ogg",
            "output_audio_codec": "opus",
            "selected_snapshot_stream_id": "stream-3",
            "output_language": "eng",
            "output_title": "Commentary"
        })
    );
}

#[test]
fn audio_payloads_reject_unknown_fields() {
    let request_err = serde_json::from_value::<TranscodeAudioRequest>(serde_json::json!({
        "input": {
            "path": "/library/input.mkv",
            "expected": {
                "size_bytes": 1234,
                "content_hash": "blake3:abc",
                "modified_at": null,
                "local_file_key": null
            }
        },
        "output": {
            "staging_root": "/tmp/voom-stage",
            "path": "/tmp/voom-stage/ticket-1/lease-1/input.audio-opus.mkv",
            "container": "mkv",
            "overwrite": false
        },
        "selection": {
            "selected_streams": [
                {
                    "snapshot_stream_id": "stream-1",
                    "provider_stream_index": 1
                }
            ]
        },
        "audio": {
            "target_codec": "opus",
            "profile": "default-opus"
        },
        "unexpected": true
    }))
    .unwrap_err();
    assert!(request_err.to_string().contains("unknown field"));

    let result_err = serde_json::from_value::<ExtractAudioResult>(serde_json::json!({
        "status": "extracted",
        "provider": "ffmpeg",
        "provider_version": "ffmpeg version 7.0",
        "input_pre": { "size_bytes": 1234, "content_hash": "blake3:input-before" },
        "input_post": { "size_bytes": 1234, "content_hash": "blake3:input-after" },
        "output": { "size_bytes": 321, "content_hash": "blake3:output" },
        "output_container": "ogg",
        "output_audio_codec": "opus",
        "selected_snapshot_stream_id": "stream-3",
        "output_language": "eng",
        "output_title": "Commentary",
        "unexpected": true
    }))
    .unwrap_err();
    assert!(result_err.to_string().contains("unknown field"));
}

#[test]
fn audio_contract_constants_pin_canonical_values() {
    assert_eq!(TRANSCODE_AUDIO_CONTAINER, "mkv");
    assert_eq!(TRANSCODE_AUDIO_CODEC_AAC, "aac");
    assert_eq!(TRANSCODE_AUDIO_CODEC_OPUS, "opus");
    assert_eq!(TRANSCODE_AUDIO_CODEC_EAC3, "eac3");
    assert_eq!(AUDIO_PROFILE_DEFAULT, "default");
    assert_eq!(EXTRACT_AUDIO_CONTAINER, "ogg");
    assert_eq!(EXTRACT_AUDIO_CODEC, "opus");
}

#[test]
fn supported_transcode_audio_codecs_are_aac_opus_eac3() {
    assert!(is_supported_transcode_audio_codec("aac"));
    assert!(is_supported_transcode_audio_codec("opus"));
    assert!(is_supported_transcode_audio_codec("eac3"));
    assert!(!is_supported_transcode_audio_codec("flac"));
    assert!(!is_supported_transcode_audio_codec(""));
}

#[test]
fn default_profile_resolves_per_channel_bitrate_per_codec() {
    assert_eq!(
        audio_target_bitrate_kbps_per_channel("aac", AUDIO_PROFILE_DEFAULT),
        Some(64)
    );
    assert_eq!(
        audio_target_bitrate_kbps_per_channel("opus", AUDIO_PROFILE_DEFAULT),
        Some(48)
    );
    assert_eq!(
        audio_target_bitrate_kbps_per_channel("eac3", AUDIO_PROFILE_DEFAULT),
        Some(96)
    );
}

#[test]
fn unsupported_codec_or_profile_has_no_target_bitrate() {
    assert_eq!(
        audio_target_bitrate_kbps_per_channel("flac", AUDIO_PROFILE_DEFAULT),
        None
    );
    assert_eq!(
        audio_target_bitrate_kbps_per_channel("eac3", "premium"),
        None
    );
    assert_eq!(audio_target_bitrate_kbps_per_channel("aac", ""), None);
}

fn observed_facts(content_hash: &str) -> AudioObservedFacts {
    AudioObservedFacts {
        size_bytes: 1234,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}
