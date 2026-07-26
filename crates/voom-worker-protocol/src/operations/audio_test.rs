use super::*;

#[test]
fn transcode_audio_settings_default_add_track_and_omit_target_channels() {
    // A transcode request (no synthesize fields) still deserializes: add_track
    // defaults false and target_channels absent (ADR 0013 additive evolution).
    let settings: TranscodeAudioSettings = serde_json::from_value(serde_json::json!({
        "target_codec": "aac",
        "profile": "default"
    }))
    .unwrap();
    assert!(!settings.add_track);
    assert_eq!(settings.target_channels, None);
    // Both synthesize fields are skipped on the wire when defaulted, so the
    // transcode request shape is unchanged.
    let value = serde_json::to_value(&settings).unwrap();
    assert!(value.get("target_channels").is_none());
    assert!(value.get("add_track").is_none());
}

#[test]
fn synthesize_audio_settings_round_trip_add_track_and_channels() {
    let settings = TranscodeAudioSettings {
        target_codec: "aac".to_owned(),
        profile: "default".to_owned(),
        add_track: true,
        target_channels: Some(2),
    };
    let value = serde_json::to_value(&settings).unwrap();
    assert_eq!(value["add_track"], true);
    assert_eq!(value["target_channels"], 2);
    let parsed: TranscodeAudioSettings = serde_json::from_value(value).unwrap();
    assert_eq!(parsed, settings);
}

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
            add_track: false,
            target_channels: None,
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
        outputs: None,
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
        outputs: None,
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
fn extract_audio_outputs_preserve_absent_null_and_empty_wire_meanings() {
    let request = legacy_extract_request_json();
    let legacy_request: ExtractAudioRequest = serde_json::from_value(request.clone()).unwrap();
    assert_eq!(legacy_request.outputs, None);
    assert!(
        serde_json::to_value(&legacy_request)
            .unwrap()
            .get("outputs")
            .is_none()
    );

    let mut null_request = request.clone();
    null_request["outputs"] = serde_json::Value::Null;
    assert!(
        serde_json::from_value::<ExtractAudioRequest>(null_request)
            .unwrap_err()
            .to_string()
            .contains("sequence")
    );

    let mut empty_request = request;
    empty_request["outputs"] = serde_json::json!([]);
    let parsed: ExtractAudioRequest = serde_json::from_value(empty_request).unwrap();
    assert_eq!(parsed.outputs, Some(Vec::new()));
    assert!(validate_extract_audio_request(&parsed).is_err());

    let result = legacy_extract_result_json();
    let parsed: ExtractAudioResult = serde_json::from_value(result.clone()).unwrap();
    assert_eq!(parsed.outputs, None);
    assert!(
        serde_json::to_value(parsed)
            .unwrap()
            .get("outputs")
            .is_none()
    );

    let mut null_result = result.clone();
    null_result["outputs"] = serde_json::Value::Null;
    assert!(
        serde_json::from_value::<ExtractAudioResult>(null_result)
            .unwrap_err()
            .to_string()
            .contains("sequence")
    );

    let mut empty_result = result;
    empty_result["outputs"] = serde_json::json!([]);
    let parsed: ExtractAudioResult = serde_json::from_value(empty_result).unwrap();
    assert_eq!(parsed.outputs, Some(Vec::new()));
    assert!(validate_extract_audio_result(&legacy_request, &parsed).is_err());
}

#[test]
fn extract_audio_plural_contract_round_trips_and_validates_ordered_outputs() {
    let request = plural_extract_request();
    validate_extract_audio_request(&request).unwrap();
    let request_json = serde_json::to_value(&request).unwrap();
    assert_eq!(request_json["outputs"][1]["output_id"], "extract_output_2");
    assert_eq!(
        request_json["outputs"][1]["selection"]["provider_stream_index"],
        4
    );
    assert_eq!(
        request_json["outputs"][1]["output"]["path"],
        "/tmp/voom-stage/ticket-2/lease-1/input.main.opus.ogg"
    );
    assert_eq!(
        serde_json::from_value::<ExtractAudioRequest>(request_json).unwrap(),
        request
    );

    let result = plural_extract_result();
    validate_extract_audio_result(&request, &result).unwrap();
    let result_json = serde_json::to_value(&result).unwrap();
    assert_eq!(result_json["outputs"][1]["output_id"], "extract_output_2");
    assert_eq!(
        result_json["outputs"][1]["selection"]["snapshot_stream_id"],
        "stream-4"
    );
    assert_eq!(
        result_json["outputs"][1]["path"],
        "/tmp/voom-stage/ticket-2/lease-1/input.main.opus.ogg"
    );
    assert_eq!(
        serde_json::from_value::<ExtractAudioResult>(result_json).unwrap(),
        result
    );
}

#[test]
fn extract_audio_request_validation_rejects_projection_and_identity_collisions() {
    let mut projection = plural_extract_request();
    projection.output.path.push_str(".different");
    assert_contract_error(
        validate_extract_audio_request(&projection),
        "first output projection",
    );

    let mut duplicate_id = plural_extract_request();
    duplicate_id.outputs.as_mut().unwrap()[1].output_id = "extract_output_1".to_owned();
    assert_contract_error(
        validate_extract_audio_request(&duplicate_id),
        "duplicate output_id",
    );

    let mut duplicate_source = plural_extract_request();
    duplicate_source.outputs.as_mut().unwrap()[1]
        .selection
        .snapshot_stream_id = "stream-3".to_owned();
    assert_contract_error(
        validate_extract_audio_request(&duplicate_source),
        "duplicate source snapshot_stream_id",
    );

    let mut duplicate_index = plural_extract_request();
    duplicate_index.outputs.as_mut().unwrap()[1]
        .selection
        .provider_stream_index = 3;
    assert_contract_error(
        validate_extract_audio_request(&duplicate_index),
        "duplicate source provider_stream_index",
    );

    let mut out_of_order = plural_extract_request();
    out_of_order.outputs.as_mut().unwrap()[1]
        .selection
        .provider_stream_index = 2;
    assert_contract_error(
        validate_extract_audio_request(&out_of_order),
        "strictly increasing",
    );

    let mut duplicate_path = plural_extract_request();
    duplicate_path.outputs.as_mut().unwrap()[1].output.path =
        "/tmp/voom-stage/ticket-2/lease-1/./input.commentary.opus.ogg".to_owned();
    assert_contract_error(
        validate_extract_audio_request(&duplicate_path),
        "duplicate normalized output path",
    );

    let mut case_duplicate_path = plural_extract_request();
    case_duplicate_path.outputs.as_mut().unwrap()[1].output.path =
        "/tmp/voom-stage/ticket-2/lease-1/INPUT.COMMENTARY.OPUS.OGG".to_owned();
    assert_contract_error(
        validate_extract_audio_request(&case_duplicate_path),
        "duplicate normalized output path",
    );
}

#[test]
fn extract_audio_result_validation_rejects_missing_reordered_or_mismatched_outputs() {
    let request = plural_extract_request();

    let mut missing_list = plural_extract_result();
    missing_list.outputs = None;
    assert_contract_error(
        validate_extract_audio_result(&request, &missing_list),
        "missing the outputs list",
    );

    let legacy_request: ExtractAudioRequest =
        serde_json::from_value(legacy_extract_request_json()).unwrap();
    assert_contract_error(
        validate_extract_audio_result(&legacy_request, &plural_extract_result()),
        "unexpected outputs list",
    );

    let mut missing = plural_extract_result();
    missing.outputs.as_mut().unwrap().pop();
    assert_contract_error(
        validate_extract_audio_result(&request, &missing),
        "output count",
    );

    let mut reordered = plural_extract_result();
    reordered.outputs.as_mut().unwrap().swap(0, 1);
    assert_contract_error(
        validate_extract_audio_result(&request, &reordered),
        "first output projection",
    );

    let mut swapped_ids = plural_extract_result();
    let outputs = swapped_ids.outputs.as_mut().unwrap();
    let first_id = outputs[0].output_id.clone();
    outputs[0].output_id = outputs[1].output_id.clone();
    outputs[1].output_id = first_id;
    assert_contract_error(
        validate_extract_audio_result(&request, &swapped_ids),
        "output_id",
    );

    let mut wrong_selection = plural_extract_result();
    wrong_selection.outputs.as_mut().unwrap()[1]
        .selection
        .provider_stream_index = 9;
    assert_contract_error(
        validate_extract_audio_result(&request, &wrong_selection),
        "selection",
    );

    let mut wrong_path = plural_extract_result();
    wrong_path.outputs.as_mut().unwrap()[1]
        .path
        .push_str(".different");
    assert_contract_error(validate_extract_audio_result(&request, &wrong_path), "path");
}

#[test]
fn extract_audio_result_allows_identical_observed_facts_for_distinct_outputs() {
    let request = plural_extract_request();
    let mut result = plural_extract_result();
    let first = result.outputs.as_ref().unwrap()[0].clone();
    let second = &mut result.outputs.as_mut().unwrap()[1];
    second.output = first.output;
    second.output_language = first.output_language;
    second.output_title = first.output_title;

    validate_extract_audio_result(&request, &result).unwrap();
}

fn assert_contract_error(result: Result<(), ExtractAudioContractError>, expected_message: &str) {
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains(expected_message),
        "expected `{expected_message}` in `{error}`"
    );
}

fn plural_extract_request() -> ExtractAudioRequest {
    let first_output = ExtractAudioOutput {
        staging_root: "/tmp/voom-stage".to_owned(),
        path: "/tmp/voom-stage/ticket-2/lease-1/input.commentary.opus.ogg".to_owned(),
        container: "ogg".to_owned(),
        audio_codec: "opus".to_owned(),
        overwrite: false,
    };
    let first_selection = AudioStreamRef {
        snapshot_stream_id: "stream-3".to_owned(),
        provider_stream_index: 3,
    };
    ExtractAudioRequest {
        input: ExtractAudioInput {
            path: "/library/input.mkv".to_owned(),
            expected: AudioExpectedFacts {
                size_bytes: 1234,
                content_hash: "blake3:abc".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
        },
        output: first_output.clone(),
        selection: first_selection.clone(),
        outputs: Some(vec![
            ExtractAudioOutputDescriptor {
                output_id: "extract_output_1".to_owned(),
                selection: first_selection,
                output: first_output,
            },
            ExtractAudioOutputDescriptor {
                output_id: "extract_output_2".to_owned(),
                selection: AudioStreamRef {
                    snapshot_stream_id: "stream-4".to_owned(),
                    provider_stream_index: 4,
                },
                output: ExtractAudioOutput {
                    staging_root: "/tmp/voom-stage".to_owned(),
                    path: "/tmp/voom-stage/ticket-2/lease-1/input.main.opus.ogg".to_owned(),
                    container: "ogg".to_owned(),
                    audio_codec: "opus".to_owned(),
                    overwrite: false,
                },
            },
        ]),
    }
}

fn plural_extract_result() -> ExtractAudioResult {
    let first_output = observed_facts("blake3:output-1");
    ExtractAudioResult {
        status: ExtractAudioStatus::Extracted,
        provider: "ffmpeg".to_owned(),
        provider_version: "ffmpeg version 7.0".to_owned(),
        input_pre: observed_facts("blake3:input"),
        input_post: observed_facts("blake3:input"),
        output: first_output.clone(),
        output_container: "ogg".to_owned(),
        output_audio_codec: "opus".to_owned(),
        selected_snapshot_stream_id: "stream-3".to_owned(),
        output_language: Some("eng".to_owned()),
        output_title: Some("Commentary".to_owned()),
        outputs: Some(vec![
            ExtractAudioOutputResult {
                output_id: "extract_output_1".to_owned(),
                selection: AudioStreamRef {
                    snapshot_stream_id: "stream-3".to_owned(),
                    provider_stream_index: 3,
                },
                path: "/tmp/voom-stage/ticket-2/lease-1/input.commentary.opus.ogg".to_owned(),
                output: first_output,
                output_container: "ogg".to_owned(),
                output_audio_codec: "opus".to_owned(),
                output_language: Some("eng".to_owned()),
                output_title: Some("Commentary".to_owned()),
            },
            ExtractAudioOutputResult {
                output_id: "extract_output_2".to_owned(),
                selection: AudioStreamRef {
                    snapshot_stream_id: "stream-4".to_owned(),
                    provider_stream_index: 4,
                },
                path: "/tmp/voom-stage/ticket-2/lease-1/input.main.opus.ogg".to_owned(),
                output: observed_facts("blake3:output-2"),
                output_container: "ogg".to_owned(),
                output_audio_codec: "opus".to_owned(),
                output_language: Some("eng".to_owned()),
                output_title: Some("Main".to_owned()),
            },
        ]),
    }
}

fn legacy_extract_request_json() -> serde_json::Value {
    serde_json::json!({
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
        }
    })
}

fn legacy_extract_result_json() -> serde_json::Value {
    serde_json::json!({
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
        "output_title": "Commentary"
    })
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
