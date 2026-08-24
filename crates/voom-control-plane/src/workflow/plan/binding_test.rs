use crate::workflow::execution::timing::EffectiveTiming;
use crate::workflow::plan::binding::{
    PolicyFileSource, branch_context_with_probe_codec, render_default_payload,
    render_default_payload_with_fan_out, render_policy_extract_audio_payload,
    render_policy_remux_payload, render_policy_transcode_audio_payload,
    render_policy_transcode_payload, render_policy_verify_artifact_payload,
};
use crate::workflow::plan::model::WorkflowPlan;
use voom_core::OperationKind;
use voom_core::{FileLocationId, FileVersionId, StorageRootId};

#[test]
fn default_payload_rendering_preserves_static_fields_then_applies_bindings() {
    let rendered = render_default_payload(
        OperationKind::ScoreQuality,
        &branch_context_with_probe_codec("file-001", "h264"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();
    assert_eq!(rendered["profile"], "default");
    assert_eq!(rendered["path"], "/library/file-001.mkv");
    assert_eq!(rendered["codec"], "h264");
    assert_eq!(rendered["duration_ms"], 25);
}

#[test]
fn default_payload_rendering_covers_default_ci_operations() {
    let branch = branch_context_with_probe_codec("file-001", "h264");
    let timing = EffectiveTiming::for_test(25, 10);
    for node in WorkflowPlan::default_ci(FileLocationId(7)).nodes {
        let payload = render_default_payload(node.operation(), &branch, timing).unwrap();
        assert_eq!(payload["operation"], operation_name_value(node.operation()));
        match node.operation() {
            OperationKind::CommitArtifact => {
                assert_eq!(payload["reason"], "quality_regression");
            }
            OperationKind::SyncExternalSystem => {
                assert_eq!(payload["system"], "plex");
                assert_eq!(payload["action"], "refresh");
            }
            OperationKind::EditTracks => {
                assert_eq!(payload["holder"], "manual");
                assert_eq!(payload["reason"], "playback");
            }
            OperationKind::ScanLibrary => {
                assert_eq!(payload["fan_out_count"], 3);
            }
            _ => {}
        }
    }
}

#[test]
fn default_transcode_payload_uses_worker_protocol_shape() {
    let rendered = render_default_payload(
        OperationKind::TranscodeVideo,
        &branch_context_with_probe_codec("file-001", "h264"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(rendered["operation"], "transcode_video");
    assert_eq!(rendered["input"]["path"], "/library/file-001.mkv");
    assert_eq!(
        rendered["input"]["expected"]["size_bytes"],
        4_200_000_000_u64
    );
    assert_eq!(
        rendered["input"]["expected"]["content_hash"],
        "blake3:file-001"
    );
    assert_eq!(rendered["output"]["container"], "mkv");
    assert_eq!(rendered["output"]["video_codec"], "hevc");
    assert_eq!(rendered["output"]["overwrite"], true);
    assert_eq!(rendered["profile"]["name"], "default-hevc");
    assert!(
        rendered["output"]["path"]
            .as_str()
            .unwrap()
            .ends_with("/file-001/file-001.default-hevc.hevc.mkv")
    );
}

#[test]
fn scan_payload_uses_effective_fan_out() {
    let rendered = render_default_payload_with_fan_out(
        OperationKind::ScanLibrary,
        &branch_context_with_probe_codec("file-001", "h264"),
        EffectiveTiming::for_test(25, 10),
        7,
    )
    .unwrap();

    assert_eq!(rendered["fan_out_count"], 7);
}

#[test]
fn policy_verify_payload_pins_exact_file_identity_without_dsl_arguments() {
    let rendered = render_policy_verify_artifact_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(
        rendered,
        serde_json::json!({
            "operation": "verify_artifact",
            "source_file_version_id": 42,
            "source_storage_root_id": 3,
            "source_location_id": 7,
            "duration_ms": 25,
            "progress_interval_ms": 10,
        })
    );
}

#[test]
fn policy_transcode_video_payload_preserves_expected_source_video_facts() {
    let operation_payload = serde_json::json!({
        "type": "transcode_video",
        "target_codec": "hevc",
        "container": "mkv",
        "profile": "default-hevc",
        "resolved_profile": voom_core::TranscodeVideoProfile::default_hevc(),
        "source_video_codec": "h264",
        "source_video_pixel_format": "yuv420p"
    });

    let rendered = render_policy_transcode_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &operation_payload,
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(rendered["source_video_codec"], "h264");
    assert_eq!(rendered["source_video_pixel_format"], "yuv420p");
}

#[test]
fn policy_remux_payload_renders_source_target_and_operation_payload() {
    let operation_payload = serde_json::json!({
        "type": "remux",
        "container": "mkv",
        "track_actions": [],
        "track_order": ["video", "audio", "subtitle"],
        "defaults": [],
        "source_media_snapshot_id": 99
    });

    let rendered = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &operation_payload,
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(rendered["operation"], "remux");
    assert_eq!(rendered["remux"], operation_payload);
    assert_eq!(rendered["remux"]["source_media_snapshot_id"], 99);
    assert_eq!(rendered["duration_ms"], 25);
    assert_eq!(rendered["progress_interval_ms"], 10);
    assert_eq!(rendered["source_file_version_id"], 42);
    assert_eq!(rendered["source_location_id"], 7);
}

#[test]
fn policy_transcode_audio_payload_renders_source_target_and_operation_payload() {
    let operation_payload = serde_json::json!({
        "type": "transcode_audio",
        "target_codec": "opus",
        "container": "mkv",
        "source_media_snapshot_id": 99
    });

    let rendered = render_policy_transcode_audio_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &operation_payload,
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(rendered["operation"], "transcode_audio");
    assert_eq!(rendered["audio"], operation_payload);
    assert_eq!(rendered["audio"]["source_media_snapshot_id"], 99);
    assert_eq!(rendered["source_file_version_id"], 42);
    assert_eq!(rendered["source_location_id"], 7);
}

#[test]
fn policy_transcode_audio_payload_accepts_published_synthesis_mode() {
    let operation_id = "node_synthesis_test";
    let companion_id = voom_plan::planner::audio::synthesis_companion_id(operation_id, "audio-1");
    let operation_payload = serde_json::json!({
        "type": "synthesize_audio",
        "operation_id": operation_id,
        "target_codec": "aac",
        "container": "mkv",
        "target_channels": 2,
        "source_media_snapshot_id": 99,
        "companions": [{
            "companion_id": companion_id,
            "source_snapshot_stream_id": "audio-1",
            "source_provider_stream_index": 1,
            "result_snapshot_stream_id": companion_id
        }]
    });

    let rendered = render_policy_transcode_audio_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &operation_payload,
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(rendered["operation"], "transcode_audio");
    assert_eq!(rendered["audio"], operation_payload);
}

#[test]
fn policy_extract_audio_payload_renders_source_target_and_operation_payload() {
    let operation_payload = serde_json::json!({
        "type": "extract_audio",
        "operation_id": "node_extract_audio_test",
        "target_codec": "opus",
        "container": "ogg",
        "source_media_snapshot_id": 99,
        "outputs": [
            {
                "output_id": "extract_output_first",
                "source_snapshot_stream_id": "audio-1",
                "source_provider_stream_index": 1,
                "name_suffix": "audio-1.opus.ogg",
                "bundle_role": "external_audio"
            },
            {
                "output_id": "extract_output_second",
                "source_snapshot_stream_id": "audio-2",
                "source_provider_stream_index": 2,
                "name_suffix": "audio-2.opus.ogg",
                "bundle_role": "commentary_audio"
            }
        ]
    });

    let rendered = render_policy_extract_audio_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &operation_payload,
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(rendered["operation"], "extract_audio");
    assert_eq!(rendered["audio"], operation_payload);
    assert_eq!(rendered["source_file_version_id"], 42);
    // A policy-rendered node is byte-touching, so the identity its declaration
    // is checked against is never optional.
    assert_eq!(rendered["source_storage_root_id"], 3);
    assert_eq!(rendered["source_location_id"], 7);
}

#[test]
fn policy_remux_payload_rejects_non_numeric_source_media_snapshot_id() {
    let err = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": [],
            "source_media_snapshot_id": "99"
        }),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "remux payload `source_media_snapshot_id` must be a positive integer"
    );
}

#[test]
fn policy_remux_payload_rejects_missing_source_media_snapshot_id() {
    let err = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": []
        }),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "remux payload `source_media_snapshot_id` must be a positive integer"
    );
}

#[test]
fn policy_remux_payload_always_carries_its_source_root_and_location() {
    let rendered = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": [],
            "source_media_snapshot_id": 99
        }),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    // A policy-rendered node is byte-touching, so the identity its declaration
    // is checked against is never optional.
    assert_eq!(rendered["source_storage_root_id"], 3);
    assert_eq!(rendered["source_location_id"], 7);
}

#[test]
fn policy_remux_payload_rejects_non_remux_payload() {
    let err = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({"type": "set_container", "container": "mkv"}),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux payload missing `type: remux`");
}

#[test]
fn policy_remux_payload_rejects_incomplete_typed_payload() {
    let err = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({"type": "remux"}),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux payload missing `container`");
}

#[test]
fn policy_remux_payload_rejects_malformed_track_action_entry() {
    let err = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [{"type": "keep_tracks"}],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": []
        }),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux track_actions[0] missing `target`");
}

#[test]
fn policy_remux_payload_preserves_attachment_track_action_target() {
    let rendered = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [{"type": "remove_tracks", "target": "attachment"}],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": [],
            "source_media_snapshot_id": 99
        }),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(
        rendered["remux"]["track_actions"],
        serde_json::json!([{"type": "remove_tracks", "target": "attachment"}])
    );
}

#[test]
fn policy_remux_payload_rejects_malformed_track_order_entry() {
    let err = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", 42, "subtitle"],
            "defaults": []
        }),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux track_order[1] must be a string");
}

#[test]
fn policy_remux_payload_accepts_published_attachment_track_order_group() {
    let payload = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "attachment"],
            "defaults": [],
            "source_media_snapshot_id": 99
        }),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(
        payload["remux"]["track_order"],
        serde_json::json!(["video", "attachment"])
    );
}

#[test]
fn policy_remux_payload_rejects_duplicate_track_order_group() {
    let err = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "audio"],
            "defaults": []
        }),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "remux track_order[2] duplicates target `audio`"
    );
}

#[test]
fn policy_remux_payload_rejects_malformed_defaults_entry() {
    let err = render_policy_remux_payload(
        PolicyFileSource {
            file_version_id: FileVersionId(42),
            storage_root_id: StorageRootId(3),
            location_id: FileLocationId(7),
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": [{"target": "audio"}]
        }),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux defaults[0] missing `strategy`");
}

fn operation_name_value(operation: OperationKind) -> serde_json::Value {
    serde_json::to_value(operation).unwrap()
}

use crate::workflow::plan::access_declaration::TicketStorageSource;
use crate::workflow::plan::binding::insert_storage_source;
use crate::workflow::plan::binding::media_dispatch::{
    DestinationRole, MediaDispatchSource, MediaExtractionRequest, backup_destination_locator,
    extract_audio_output_file_name, planned_output_locator, remux_output_file_name,
    render_media_dispatch_back_up_file, render_media_dispatch_extract_audio,
    render_media_dispatch_probe, render_media_dispatch_remux,
    render_media_dispatch_transcode_audio, render_media_dispatch_transcode_video,
    render_media_dispatch_verify_artifact, resolve_destination_root,
    transcode_audio_output_file_name, transcode_video_output_file_name,
};
use voom_core::{PROTOCOL_VERSION, ProviderRelativeLocator};
use voom_worker_protocol::{
    AudioExpectedFacts, AudioStreamRef, ExpectedFileFacts, MediaBackUpFileDispatch, MediaDispatch,
    MediaExtractAudioDispatch, MediaExtractOutput, MediaPlannedOutput, MediaProbeDispatch,
    MediaRemuxDispatch, MediaSourceRef, MediaTranscodeAudioDispatch, MediaTranscodeVideoDispatch,
    MediaVerifyArtifactDispatch, RemuxExpectedFacts, RemuxSelection, TranscodeAudioSelection,
    TranscodeAudioSettings, TranscodeVideoExpectedFacts, TranscodeVideoProfile,
    VerifyArtifactExpectedFacts, decode_media_dispatch,
};

fn location_source() -> MediaDispatchSource {
    MediaDispatchSource::Location {
        storage_root_id: StorageRootId(7),
        file_location_id: FileLocationId(11),
        provider_relative_locator: ProviderRelativeLocator::new("library/Movie.mkv".to_owned())
            .unwrap(),
    }
}

fn staged_output_source() -> MediaDispatchSource {
    MediaDispatchSource::RecordedStagedOutput {
        storage_root_id: StorageRootId(9),
        provider_relative_locator: ProviderRelativeLocator::new(
            "staging/file-001/Movie.default-hevc.hevc.mkv".to_owned(),
        )
        .unwrap(),
    }
}

fn expected_planned(root: StorageRootId, relative: &str) -> MediaPlannedOutput {
    MediaPlannedOutput {
        storage_root_id: root,
        provider_relative_locator: ProviderRelativeLocator::new(relative.to_owned()).unwrap(),
        overwrite: false,
    }
}

fn expected_file_facts() -> ExpectedFileFacts {
    ExpectedFileFacts {
        size_bytes: 4_200_000_000,
        content_hash: "blake3:file-001".to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

fn audio_facts() -> AudioExpectedFacts {
    AudioExpectedFacts {
        size_bytes: 4_200_000_000,
        content_hash: "blake3:file-001".to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

fn remux_facts() -> RemuxExpectedFacts {
    RemuxExpectedFacts {
        size_bytes: 4_200_000_000,
        content_hash: "blake3:file-001".to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

fn transcode_video_facts() -> TranscodeVideoExpectedFacts {
    TranscodeVideoExpectedFacts {
        size_bytes: 4_200_000_000,
        content_hash: "blake3:file-001".to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

fn verify_facts() -> VerifyArtifactExpectedFacts {
    VerifyArtifactExpectedFacts {
        size_bytes: 4_200_000_000,
        content_hash: "blake3:file-001".to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

#[test]
fn probe_backup_and_verify_envelopes_round_trip_through_decode() {
    let source = MediaSourceRef {
        storage_root_id: StorageRootId(7),
        provider_relative_locator: ProviderRelativeLocator::new("library/Movie.mkv".to_owned())
            .unwrap(),
    };

    let probe = render_media_dispatch_probe(&location_source(), expected_file_facts()).unwrap();
    assert_eq!(
        decode_media_dispatch(&probe).unwrap(),
        MediaDispatch::Probe(MediaProbeDispatch {
            schema: PROTOCOL_VERSION,
            source: source.clone(),
            expected: expected_file_facts(),
        })
    );
    // Scalar declaration keys live beside the envelope, not inside it.
    assert!(probe.get("source_storage_root_id").is_none());
    assert!(probe.get("source_location_id").is_none());

    let backup =
        render_media_dispatch_back_up_file(&location_source(), FileVersionId(42), StorageRootId(5))
            .unwrap();
    assert_eq!(
        decode_media_dispatch(&backup).unwrap(),
        MediaDispatch::BackUpFile(MediaBackUpFileDispatch {
            schema: PROTOCOL_VERSION,
            source: source.clone(),
            destination: expected_planned(StorageRootId(5), "v42/Movie.mkv"),
        })
    );

    let verify =
        render_media_dispatch_verify_artifact(&staged_output_source(), verify_facts()).unwrap();
    assert_eq!(
        decode_media_dispatch(&verify).unwrap(),
        MediaDispatch::VerifyArtifact(MediaVerifyArtifactDispatch {
            schema: PROTOCOL_VERSION,
            target: MediaSourceRef {
                storage_root_id: StorageRootId(9),
                provider_relative_locator: ProviderRelativeLocator::new(
                    "staging/file-001/Movie.default-hevc.hevc.mkv".to_owned()
                )
                .unwrap(),
            },
            expected: verify_facts(),
        })
    );

    for rendered in [&probe, &backup, &verify] {
        // No absolute path ever leaves the control plane in the envelope.
        assert!(!rendered.to_string().contains("/library/"));
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one round-trip table over seven envelope families"
)]
fn staging_envelopes_round_trip_with_overwrite_false_outputs() {
    let source = MediaSourceRef {
        storage_root_id: StorageRootId(7),
        provider_relative_locator: ProviderRelativeLocator::new("library/Movie.mkv".to_owned())
            .unwrap(),
    };
    let destination_root = StorageRootId(5);

    let selection = TranscodeAudioSelection {
        selected_streams: vec![AudioStreamRef {
            snapshot_stream_id: "a-1".to_owned(),
            provider_stream_index: 1,
        }],
    };
    let settings = TranscodeAudioSettings {
        target_codec: "aac".to_owned(),
        profile: "default".to_owned(),
        add_track: false,
        target_channels: None,
    };
    let audio = render_media_dispatch_transcode_audio(
        "file-001",
        &location_source(),
        audio_facts(),
        selection.clone(),
        settings.clone(),
        destination_root,
    )
    .unwrap();
    let MediaDispatch::TranscodeAudio(decoded) = decode_media_dispatch(&audio).unwrap() else {
        panic!("expected a transcode_audio envelope");
    };
    assert_eq!(
        decoded,
        MediaTranscodeAudioDispatch {
            schema: PROTOCOL_VERSION,
            source: source.clone(),
            expected: audio_facts(),
            output_container: "mkv".to_owned(),
            output: expected_planned(destination_root, "file-001/Movie.audio-aac.mkv"),
            selection,
            settings,
        }
    );
    assert!(!decoded.output.overwrite);

    let extractions = vec![
        MediaExtractionRequest {
            output_id: "track-1".to_owned(),
            selection: AudioStreamRef {
                snapshot_stream_id: "a-1".to_owned(),
                provider_stream_index: 1,
            },
            audio_codec: "opus".to_owned(),
        },
        MediaExtractionRequest {
            output_id: "track-2".to_owned(),
            selection: AudioStreamRef {
                snapshot_stream_id: "a-2".to_owned(),
                provider_stream_index: 2,
            },
            audio_codec: "opus".to_owned(),
        },
    ];
    let extract = render_media_dispatch_extract_audio(
        "file-001",
        &location_source(),
        audio_facts(),
        &extractions,
        destination_root,
    )
    .unwrap();
    let MediaDispatch::ExtractAudio(decoded) = decode_media_dispatch(&extract).unwrap() else {
        panic!("expected an extract_audio envelope");
    };
    assert_eq!(
        decoded,
        MediaExtractAudioDispatch {
            schema: PROTOCOL_VERSION,
            source: source.clone(),
            expected: audio_facts(),
            output_container: "ogg".to_owned(),
            outputs: vec![
                MediaExtractOutput {
                    output_id: "track-1".to_owned(),
                    selection: AudioStreamRef {
                        snapshot_stream_id: "a-1".to_owned(),
                        provider_stream_index: 1,
                    },
                    audio_codec: "opus".to_owned(),
                    output: expected_planned(destination_root, "file-001/Movie.a-1.opus.ogg"),
                },
                MediaExtractOutput {
                    output_id: "track-2".to_owned(),
                    selection: AudioStreamRef {
                        snapshot_stream_id: "a-2".to_owned(),
                        provider_stream_index: 2,
                    },
                    audio_codec: "opus".to_owned(),
                    output: expected_planned(destination_root, "file-001/Movie.a-2.opus.ogg"),
                },
            ],
        }
    );
    for output in &decoded.outputs {
        assert!(!output.output.overwrite);
    }

    let profile = TranscodeVideoProfile::default_hevc();
    let video = render_media_dispatch_transcode_video(
        "file-001",
        &location_source(),
        transcode_video_facts(),
        destination_root,
        profile.clone(),
        None,
        false,
    )
    .unwrap();
    let MediaDispatch::TranscodeVideo(decoded) = decode_media_dispatch(&video).unwrap() else {
        panic!("expected a transcode_video envelope");
    };
    assert_eq!(
        decoded,
        MediaTranscodeVideoDispatch {
            schema: PROTOCOL_VERSION,
            source: source.clone(),
            expected: transcode_video_facts(),
            output_container: "mkv".to_owned(),
            output_video_codec: "hevc".to_owned(),
            output: expected_planned(destination_root, "file-001/Movie.default-hevc.hevc.mkv"),
            profile,
            hardware_assignment: None,
            copy_video: false,
        }
    );
    assert!(!decoded.output.overwrite);

    let remux_selection = RemuxSelection {
        keep_streams: vec![],
        default_streams: vec![],
        clear_default_streams: vec![],
        track_order: vec![],
        head_streams: vec![],
        forced_streams: vec![],
        clear_forced_streams: vec![],
    };
    let remux = render_media_dispatch_remux(
        "file-001",
        &location_source(),
        remux_facts(),
        remux_selection.clone(),
        destination_root,
    )
    .unwrap();
    let MediaDispatch::Remux(decoded) = decode_media_dispatch(&remux).unwrap() else {
        panic!("expected a remux envelope");
    };
    assert_eq!(
        decoded,
        MediaRemuxDispatch {
            schema: PROTOCOL_VERSION,
            source,
            expected: remux_facts(),
            output_container: "mkv".to_owned(),
            output: expected_planned(destination_root, "file-001/Movie.remux.mkv"),
            selection: remux_selection,
        }
    );
    assert!(!decoded.output.overwrite);
}

#[test]
fn scalar_keys_survive_nested_media_dispatch_insertion() {
    let mut payload = serde_json::json!({ "operation": "probe" });
    let object = payload.as_object_mut().unwrap();
    insert_storage_source(
        object,
        &TicketStorageSource::Location {
            storage_root_id: StorageRootId(7),
            file_location_id: FileLocationId(11),
        },
    );
    payload["media_dispatch"] =
        render_media_dispatch_probe(&location_source(), expected_file_facts()).unwrap();

    // The declaration still derives from the untouched scalar keys.
    assert_eq!(payload["source_storage_root_id"], 7);
    assert_eq!(payload["source_location_id"], 11);
    assert_eq!(
        location_source().ticket_storage_source(),
        TicketStorageSource::Location {
            storage_root_id: StorageRootId(7),
            file_location_id: FileLocationId(11),
        }
    );
    assert_eq!(
        staged_output_source().ticket_storage_source(),
        TicketStorageSource::Root {
            storage_root_id: StorageRootId(9),
        }
    );
}

#[test]
fn unset_default_destination_roots_fail_render_descriptively() {
    for role in [
        DestinationRole::Output,
        DestinationRole::Staging,
        DestinationRole::Backup,
    ] {
        let err = resolve_destination_root(role, None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(role.as_str()) && err.contains("no default"),
            "uninformative error for {role:?}: {err}"
        );
    }
    assert_eq!(
        resolve_destination_root(DestinationRole::Output, Some(StorageRootId(5))).unwrap(),
        StorageRootId(5)
    );
}

#[test]
fn planned_output_locators_mirror_current_path_based_names_and_are_deterministic() {
    let video = transcode_video_output_file_name("Movie", "default-hevc", "hevc", "mkv");
    assert_eq!(video, "Movie.default-hevc.hevc.mkv");
    let locator = planned_output_locator("file-001", &video).unwrap();
    assert_eq!(locator.as_str(), "file-001/Movie.default-hevc.hevc.mkv");
    assert_eq!(locator, planned_output_locator("file-001", &video).unwrap());

    assert_eq!(remux_output_file_name("Movie", "mkv"), "Movie.remux.mkv");
    assert_eq!(
        transcode_audio_output_file_name("Movie", "aac", "mkv"),
        "Movie.audio-aac.mkv"
    );
    assert_eq!(
        extract_audio_output_file_name("Movie", "a-1", "opus"),
        "Movie.a-1.opus.ogg"
    );
    assert_eq!(
        backup_destination_locator(FileVersionId(42), "Movie.mkv")
            .unwrap()
            .as_str(),
        "v42/Movie.mkv"
    );

    // Branch identity cannot smuggle absolute or traversing paths in.
    assert!(planned_output_locator("", "x.mkv").is_err());
    assert!(planned_output_locator("..", "x.mkv").is_err());
}

#[test]
fn recorded_staged_output_addresses_feed_verification_only() {
    let probe_err = render_media_dispatch_probe(&staged_output_source(), expected_file_facts())
        .unwrap_err()
        .to_string();
    assert!(probe_err.contains("verification"), "{probe_err}");
    let backup_err = render_media_dispatch_back_up_file(
        &staged_output_source(),
        FileVersionId(1),
        StorageRootId(5),
    )
    .unwrap_err()
    .to_string();
    assert!(backup_err.contains("live location"), "{backup_err}");
    let verify_err = render_media_dispatch_verify_artifact(&location_source(), verify_facts())
        .unwrap_err()
        .to_string();
    assert!(
        verify_err.contains("recorded staged-output"),
        "{verify_err}"
    );
}
