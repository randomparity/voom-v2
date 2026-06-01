use super::*;
use crate::payload::{Event, EventKind};
use voom_core::FailureClass;
#[test]
fn artifact_staged_payload_round_trip() {
    let p = ArtifactStagedPayload {
        artifact_handle_id: 10,
        artifact_location_id: 11,
        source_file_version_id: 12,
        source_file_location_id: Some(13),
        staging_path: "/var/lib/voom/staging/10".to_owned(),
        size_bytes: 4096,
        checksum: "blake3:abc123".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactStaged(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.staged");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactStaged(q) if q == p));
    assert_eq!(Event::ArtifactStaged(p).kind(), EventKind::ArtifactStaged);
}

#[test]
fn artifact_verification_started_payload_round_trip() {
    let p = ArtifactVerificationStartedPayload {
        artifact_handle_id: 10,
        artifact_location_id: 11,
        worker_id: 12,
        path: "/var/lib/voom/staging/10".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactVerificationStarted(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.verification_started");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactVerificationStarted(q) if q == p));
    assert_eq!(
        Event::ArtifactVerificationStarted(p).kind(),
        EventKind::ArtifactVerificationStarted
    );
}

#[test]
fn artifact_verification_succeeded_payload_round_trip() {
    let p = ArtifactVerificationSucceededPayload {
        verification_id: 20,
        artifact_handle_id: 10,
        artifact_location_id: 11,
        worker_id: 12,
        observed_size_bytes: 4096,
        observed_checksum: "blake3:abc123".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactVerificationSucceeded(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.verification_succeeded");
    assert_eq!(json["payload"]["observed_size_bytes"], 4096);
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactVerificationSucceeded(q) if q == p));
    assert_eq!(
        Event::ArtifactVerificationSucceeded(p).kind(),
        EventKind::ArtifactVerificationSucceeded
    );
}

#[test]
fn artifact_verification_failed_payload_round_trip() {
    let p = ArtifactVerificationFailedPayload {
        verification_id: 20,
        artifact_handle_id: 10,
        artifact_location_id: 11,
        worker_id: 12,
        error_code: "ARTIFACT_CHECKSUM_MISMATCH".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactVerificationFailed(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.verification_failed");
    assert_eq!(json["payload"]["error_code"], "ARTIFACT_CHECKSUM_MISMATCH");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactVerificationFailed(q) if q == p));
    assert_eq!(
        Event::ArtifactVerificationFailed(p).kind(),
        EventKind::ArtifactVerificationFailed
    );
}

#[test]
fn artifact_transcode_started_payload_serializes_correlation_fields() {
    let p = ArtifactTranscodeStartedPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: 5,
        staging_path: "/tmp/voom-stage/2/3/out.mkv".to_owned(),
        profile_name: "default-hevc".to_owned(),
        encoder: "libx265".to_owned(),
        target_codec: "hevc".to_owned(),
        output_container: "mkv".to_owned(),
        provider: Some("ffmpeg".to_owned()),
        provider_version: None,
    };

    let json = serde_json::to_value(Event::ArtifactTranscodeStarted(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.transcode_started");
    assert_eq!(json["payload"]["job_id"], 1);
    assert_eq!(json["payload"]["ticket_id"], 2);
    assert_eq!(json["payload"]["lease_id"], 3);
    assert_eq!(json["payload"]["source_file_version_id"], 4);
    assert_eq!(json["payload"]["source_file_location_id"], 5);
    assert_eq!(
        json["payload"]["staging_path"],
        "/tmp/voom-stage/2/3/out.mkv"
    );
    assert_eq!(json["payload"]["profile_name"], "default-hevc");
    assert_eq!(json["payload"]["encoder"], "libx265");
    assert_eq!(json["payload"]["target_codec"], "hevc");
    assert_eq!(json["payload"]["output_container"], "mkv");
    assert_eq!(json["payload"]["provider"], "ffmpeg");
    assert_eq!(json["payload"]["provider_version"], serde_json::Value::Null);

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactTranscodeStarted(q) if q == p));
    assert_eq!(
        Event::ArtifactTranscodeStarted(p).kind(),
        EventKind::ArtifactTranscodeStarted
    );
}

#[test]
fn artifact_transcode_succeeded_payload_carries_profile_and_observed_output_facts() {
    let p = ArtifactTranscodeSucceededPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: 5,
        artifact_handle_id: 6,
        artifact_location_id: 7,
        staging_path: "/tmp/voom-stage/2/3/out.mp4".to_owned(),
        profile_name: "av1-1080p".to_owned(),
        encoder: "libsvtav1".to_owned(),
        target_codec: "av1".to_owned(),
        output_container: "mp4".to_owned(),
        output_video_codec: "av1".to_owned(),
        copied_video: false,
        output_width: 1920,
        output_height: 1080,
        output_pixel_format: "yuv420p".to_owned(),
        provider: "ffmpeg".to_owned(),
        provider_version: "6.1".to_owned(),
    };

    let json = serde_json::to_value(Event::ArtifactTranscodeSucceeded(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.transcode_succeeded");
    assert_eq!(json["payload"]["profile_name"], "av1-1080p");
    assert_eq!(json["payload"]["encoder"], "libsvtav1");
    assert_eq!(json["payload"]["target_codec"], "av1");
    assert_eq!(json["payload"]["output_container"], "mp4");
    assert_eq!(json["payload"]["output_video_codec"], "av1");
    assert_eq!(json["payload"]["copied_video"], false);
    assert_eq!(json["payload"]["output_width"], 1920);
    assert_eq!(json["payload"]["output_height"], 1080);
    assert_eq!(json["payload"]["output_pixel_format"], "yuv420p");

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactTranscodeSucceeded(q) if q == p));
}

#[test]
fn legacy_artifact_transcode_started_row_decodes_with_defaulted_fields() {
    let json = serde_json::json!({
        "kind": "artifact.transcode_started",
        "payload": {
            "job_id": 1,
            "ticket_id": 2,
            "lease_id": 3,
            "source_file_version_id": 4,
            "source_file_location_id": 5,
            "staging_path": "/tmp/voom-stage/2/3/out.mkv",
            "provider": "ffmpeg",
            "provider_version": null
        }
    });

    let back: Event = serde_json::from_value(json).unwrap();

    let expected = ArtifactTranscodeStartedPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: 5,
        staging_path: "/tmp/voom-stage/2/3/out.mkv".to_owned(),
        profile_name: String::new(),
        encoder: String::new(),
        target_codec: String::new(),
        output_container: String::new(),
        provider: Some("ffmpeg".to_owned()),
        provider_version: None,
    };
    assert_eq!(back, Event::ArtifactTranscodeStarted(expected));
}

#[test]
fn legacy_artifact_transcode_succeeded_row_decodes_with_defaulted_fields() {
    let json = serde_json::json!({
        "kind": "artifact.transcode_succeeded",
        "payload": {
            "job_id": 1,
            "ticket_id": 2,
            "lease_id": 3,
            "source_file_version_id": 4,
            "source_file_location_id": 5,
            "artifact_handle_id": 6,
            "artifact_location_id": 7,
            "staging_path": "/tmp/voom-stage/2/3/out.mkv",
            "output_container": "mkv",
            "output_video_codec": "hevc",
            "provider": "ffmpeg",
            "provider_version": "6.1"
        }
    });

    let back: Event = serde_json::from_value(json).unwrap();

    let expected = ArtifactTranscodeSucceededPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: 5,
        artifact_handle_id: 6,
        artifact_location_id: 7,
        staging_path: "/tmp/voom-stage/2/3/out.mkv".to_owned(),
        profile_name: String::new(),
        encoder: String::new(),
        target_codec: String::new(),
        output_container: "mkv".to_owned(),
        output_video_codec: "hevc".to_owned(),
        copied_video: false,
        output_width: 0,
        output_height: 0,
        output_pixel_format: String::new(),
        provider: "ffmpeg".to_owned(),
        provider_version: "6.1".to_owned(),
    };
    assert_eq!(back, Event::ArtifactTranscodeSucceeded(expected));
}

#[test]
fn artifact_transcode_failed_payload_carries_profile_facts() {
    let p = ArtifactTranscodeFailedPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: Some(5),
        staging_path: Some("/tmp/voom-stage/2/3/out.mkv".to_owned()),
        profile_name: "default-av1".to_owned(),
        encoder: "libsvtav1".to_owned(),
        target_codec: "av1".to_owned(),
        output_container: "mkv".to_owned(),
        failure_class: FailureClass::WorkerCrash,
        error_code: "EXTERNAL_SYSTEM_UNAVAILABLE".to_owned(),
        message: "ffmpeg exited 1".to_owned(),
        provider: Some("ffmpeg".to_owned()),
        provider_version: None,
    };

    let json = serde_json::to_value(Event::ArtifactTranscodeFailed(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.transcode_failed");
    assert_eq!(json["payload"]["profile_name"], "default-av1");
    assert_eq!(json["payload"]["encoder"], "libsvtav1");
    assert_eq!(json["payload"]["target_codec"], "av1");
    assert_eq!(json["payload"]["output_container"], "mkv");

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactTranscodeFailed(q) if q == p));
}

#[test]
fn artifact_remux_started_payload_serializes_selection_correlation_fields() {
    let p = ArtifactRemuxStartedPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: 5,
        staging_path: "/tmp/voom-stage/2/3/out.mkv".to_owned(),
        selected_streams: vec![
            ArtifactRemuxStreamPayload {
                snapshot_stream_id: "stream-0".to_owned(),
                provider_stream_index: 0,
            },
            ArtifactRemuxStreamPayload {
                snapshot_stream_id: "stream-1".to_owned(),
                provider_stream_index: 1,
            },
        ],
        default_streams: vec![ArtifactRemuxStreamPayload {
            snapshot_stream_id: "stream-1".to_owned(),
            provider_stream_index: 1,
        }],
        clear_default_streams: Vec::new(),
        track_order: vec!["video".to_owned(), "audio".to_owned()],
        provider: Some("mkvtoolnix".to_owned()),
        provider_version: None,
    };

    let json = serde_json::to_value(Event::ArtifactRemuxStarted(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.remux_started");
    assert_eq!(json["payload"]["job_id"], 1);
    assert_eq!(json["payload"]["ticket_id"], 2);
    assert_eq!(json["payload"]["lease_id"], 3);
    assert_eq!(json["payload"]["source_file_version_id"], 4);
    assert_eq!(json["payload"]["source_file_location_id"], 5);
    assert_eq!(
        json["payload"]["selected_streams"][1]["snapshot_stream_id"],
        "stream-1"
    );
    assert_eq!(
        json["payload"]["selected_streams"][1]["provider_stream_index"],
        1
    );
    assert_eq!(
        json["payload"]["default_streams"][0]["snapshot_stream_id"],
        "stream-1"
    );
    assert_eq!(
        json["payload"]["track_order"],
        serde_json::json!(["video", "audio"])
    );

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactRemuxStarted(q) if q == p));
    assert_eq!(
        Event::ArtifactRemuxStarted(p).kind(),
        EventKind::ArtifactRemuxStarted
    );
}

#[test]
fn artifact_audio_transcode_started_payload_serializes_audit_correlation_fields() {
    let p = ArtifactAudioTranscodeStartedPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: 5,
        source_media_snapshot_id: 6,
        staging_path: "/tmp/voom-stage/2/3/out.mkv".to_owned(),
        selected_streams: vec![ArtifactAudioStreamPayload {
            snapshot_stream_id: "audio-1".to_owned(),
            provider_stream_index: 7,
        }],
        target_codec: "aac".to_owned(),
        output_container: "mkv".to_owned(),
        provider: Some("ffmpeg".to_owned()),
        provider_version: None,
    };

    let json = serde_json::to_value(Event::ArtifactAudioTranscodeStarted(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.audio_transcode_started");
    assert_eq!(json["payload"]["job_id"], 1);
    assert_eq!(json["payload"]["ticket_id"], 2);
    assert_eq!(json["payload"]["lease_id"], 3);
    assert_eq!(json["payload"]["source_file_version_id"], 4);
    assert_eq!(json["payload"]["source_file_location_id"], 5);
    assert_eq!(json["payload"]["source_media_snapshot_id"], 6);
    assert_eq!(
        json["payload"]["selected_streams"][0]["snapshot_stream_id"],
        "audio-1"
    );
    assert_eq!(
        json["payload"]["selected_streams"][0]["provider_stream_index"],
        7
    );
    assert_eq!(json["payload"]["provider"], "ffmpeg");
    assert_eq!(json["payload"]["provider_version"], serde_json::Value::Null);

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactAudioTranscodeStarted(q) if q == p));
    assert_eq!(
        Event::ArtifactAudioTranscodeStarted(p).kind(),
        EventKind::ArtifactAudioTranscodeStarted
    );
}

#[test]
fn artifact_audio_transcode_progress_payload_serializes_progress_and_selection() {
    let p = ArtifactAudioTranscodeProgressPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: 5,
        source_media_snapshot_id: 6,
        staging_path: "/tmp/voom-stage/2/3/out.mkv".to_owned(),
        selected_streams: vec![ArtifactAudioStreamPayload {
            snapshot_stream_id: "audio-1".to_owned(),
            provider_stream_index: 7,
        }],
        percent_bps: Some(2500),
        message: Some("encoding audio".to_owned()),
        provider: Some("ffmpeg".to_owned()),
        provider_version: Some("6.1".to_owned()),
    };

    let json = serde_json::to_value(Event::ArtifactAudioTranscodeProgress(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.audio_transcode_progress");
    assert_eq!(json["payload"]["percent_bps"], 2500);
    assert_eq!(json["payload"]["message"], "encoding audio");
    assert_eq!(
        json["payload"]["selected_streams"][0]["provider_stream_index"],
        7
    );

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactAudioTranscodeProgress(q) if q == p));
}

#[test]
fn artifact_audio_transcode_succeeded_payload_carries_artifact_result_and_provider() {
    let p = ArtifactAudioTranscodeSucceededPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: 5,
        source_media_snapshot_id: 6,
        artifact_handle_id: 8,
        artifact_location_id: 9,
        staging_path: "/tmp/voom-stage/2/3/out.mkv".to_owned(),
        selected_streams: vec![ArtifactAudioStreamPayload {
            snapshot_stream_id: "audio-1".to_owned(),
            provider_stream_index: 7,
        }],
        selected_snapshot_stream_ids: vec!["audio-1".to_owned()],
        selected_output_streams: vec![ArtifactAudioOutputStreamPayload {
            snapshot_stream_id: "audio-1".to_owned(),
            output_provider_stream_index: 0,
            codec: "aac".to_owned(),
            language: Some("eng".to_owned()),
            title: Some("Main".to_owned()),
            default: Some(true),
            disposition: Some(ArtifactAudioDispositionPayload {
                default: Some(true),
                forced: Some(false),
                commentary: Some(false),
            }),
            channels: Some(2),
        }],
        output_container: "mkv".to_owned(),
        output_audio_codecs: vec!["aac".to_owned()],
        provider: "ffmpeg".to_owned(),
        provider_version: "6.1".to_owned(),
    };

    let json = serde_json::to_value(Event::ArtifactAudioTranscodeSucceeded(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.audio_transcode_succeeded");
    assert_eq!(json["payload"]["artifact_handle_id"], 8);
    assert_eq!(json["payload"]["artifact_location_id"], 9);
    assert_eq!(
        json["payload"]["selected_snapshot_stream_ids"],
        serde_json::json!(["audio-1"])
    );
    assert_eq!(
        json["payload"]["output_audio_codecs"],
        serde_json::json!(["aac"])
    );
    assert_eq!(
        json["payload"]["selected_output_streams"][0]["output_provider_stream_index"],
        0
    );
    assert_eq!(
        json["payload"]["selected_output_streams"][0]["disposition"]["forced"],
        false
    );

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactAudioTranscodeSucceeded(q) if q == p));
}

#[test]
fn artifact_audio_transcode_failed_payload_carries_public_error_code_and_known_ids() {
    let p = ArtifactAudioTranscodeFailedPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: Some(5),
        source_media_snapshot_id: Some(6),
        artifact_handle_id: Some(8),
        artifact_location_id: Some(9),
        staging_path: Some("/tmp/voom-stage/2/3/out.mkv".to_owned()),
        selected_streams: vec![ArtifactAudioStreamPayload {
            snapshot_stream_id: "audio-1".to_owned(),
            provider_stream_index: 7,
        }],
        selected_output_streams: vec![ArtifactAudioOutputStreamPayload {
            snapshot_stream_id: "audio-1".to_owned(),
            output_provider_stream_index: 0,
            codec: "aac".to_owned(),
            language: Some("eng".to_owned()),
            title: Some("Main".to_owned()),
            default: Some(true),
            disposition: None,
            channels: Some(2),
        }],
        failure_class: FailureClass::PolicyValidationError,
        error_code: "CONFIG_INVALID".to_owned(),
        message: "unsupported audio codec".to_owned(),
        provider: Some("ffmpeg".to_owned()),
        provider_version: Some("6.1".to_owned()),
    };

    let json = serde_json::to_value(Event::ArtifactAudioTranscodeFailed(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.audio_transcode_failed");
    assert_eq!(json["payload"]["error_code"], "CONFIG_INVALID");
    assert_eq!(json["payload"]["artifact_handle_id"], 8);
    assert_eq!(json["payload"]["artifact_location_id"], 9);
    assert_eq!(
        json["payload"]["selected_output_streams"][0]["codec"],
        "aac"
    );

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactAudioTranscodeFailed(q) if q == p));
}

#[test]
fn artifact_audio_extract_started_and_progress_payloads_round_trip() {
    let selected_stream = ArtifactAudioStreamPayload {
        snapshot_stream_id: "audio-2".to_owned(),
        provider_stream_index: 11,
    };
    let started = ArtifactAudioExtractStartedPayload {
        job_id: 10,
        ticket_id: 20,
        lease_id: Some(30),
        source_file_version_id: 40,
        source_file_location_id: 50,
        source_media_snapshot_id: 60,
        source_bundle_id: 70,
        staging_path: "/tmp/voom-stage/20/30/out.ogg".to_owned(),
        selected_stream: selected_stream.clone(),
        role: "external_audio".to_owned(),
        target_codec: "opus".to_owned(),
        output_container: "ogg".to_owned(),
        provider: None,
        provider_version: None,
    };
    let progress = ArtifactAudioExtractProgressPayload {
        job_id: 10,
        ticket_id: 20,
        lease_id: Some(30),
        source_file_version_id: 40,
        source_file_location_id: 50,
        source_media_snapshot_id: 60,
        source_bundle_id: 70,
        staging_path: "/tmp/voom-stage/20/30/out.ogg".to_owned(),
        selected_stream: selected_stream.clone(),
        percent_bps: Some(5000),
        message: Some("extracting audio".to_owned()),
        provider: Some("ffmpeg".to_owned()),
        provider_version: Some("6.1".to_owned()),
    };
    let started_json =
        serde_json::to_value(Event::ArtifactAudioExtractStarted(started.clone())).unwrap();
    let progress_json =
        serde_json::to_value(Event::ArtifactAudioExtractProgress(progress.clone())).unwrap();

    assert_eq!(started_json["kind"], "artifact.audio_extract_started");
    assert_eq!(progress_json["kind"], "artifact.audio_extract_progress");
    assert_eq!(
        started_json["payload"]["selected_stream"]["snapshot_stream_id"],
        "audio-2"
    );

    assert!(matches!(
        serde_json::from_value::<Event>(started_json).unwrap(),
        Event::ArtifactAudioExtractStarted(q) if q == started
    ));
    assert!(matches!(
        serde_json::from_value::<Event>(progress_json).unwrap(),
        Event::ArtifactAudioExtractProgress(q) if q == progress
    ));
}

#[test]
fn artifact_audio_extract_succeeded_and_failed_payloads_round_trip() {
    let selected_stream = ArtifactAudioStreamPayload {
        snapshot_stream_id: "audio-2".to_owned(),
        provider_stream_index: 11,
    };
    let succeeded = ArtifactAudioExtractSucceededPayload {
        job_id: 10,
        ticket_id: 20,
        lease_id: Some(30),
        source_file_version_id: 40,
        source_file_location_id: 50,
        source_media_snapshot_id: 60,
        source_bundle_id: 70,
        artifact_handle_id: 80,
        artifact_location_id: 90,
        staging_path: "/tmp/voom-stage/20/30/out.ogg".to_owned(),
        selected_stream: selected_stream.clone(),
        selected_snapshot_stream_id: "audio-2".to_owned(),
        role: "external_audio".to_owned(),
        output_container: "ogg".to_owned(),
        output_audio_codec: "opus".to_owned(),
        provider: "ffmpeg".to_owned(),
        provider_version: "6.1".to_owned(),
    };
    let failed = ArtifactAudioExtractFailedPayload {
        job_id: 10,
        ticket_id: 20,
        lease_id: Some(30),
        source_file_version_id: 40,
        source_file_location_id: Some(50),
        source_media_snapshot_id: Some(60),
        source_bundle_id: 70,
        artifact_handle_id: Some(80),
        artifact_location_id: Some(90),
        staging_path: Some("/tmp/voom-stage/20/30/out.ogg".to_owned()),
        selected_stream: Some(selected_stream),
        role: Some("external_audio".to_owned()),
        failure_class: FailureClass::PolicyValidationError,
        error_code: "CONFIG_INVALID".to_owned(),
        message: "source stream missing".to_owned(),
        provider: Some("ffmpeg".to_owned()),
        provider_version: Some("6.1".to_owned()),
    };
    let succeeded_json =
        serde_json::to_value(Event::ArtifactAudioExtractSucceeded(succeeded.clone())).unwrap();
    let failed_json =
        serde_json::to_value(Event::ArtifactAudioExtractFailed(failed.clone())).unwrap();

    assert_eq!(succeeded_json["kind"], "artifact.audio_extract_succeeded");
    assert_eq!(failed_json["kind"], "artifact.audio_extract_failed");
    assert_eq!(
        succeeded_json["payload"]["selected_snapshot_stream_id"],
        "audio-2"
    );
    assert_eq!(failed_json["payload"]["error_code"], "CONFIG_INVALID");

    assert!(matches!(
        serde_json::from_value::<Event>(succeeded_json).unwrap(),
        Event::ArtifactAudioExtractSucceeded(q) if q == succeeded
    ));
    assert!(matches!(
        serde_json::from_value::<Event>(failed_json).unwrap(),
        Event::ArtifactAudioExtractFailed(q) if q == failed
    ));
}

#[test]
fn artifact_remux_failed_payload_serializes_public_error_code() {
    let p = ArtifactRemuxFailedPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: Some(5),
        artifact_handle_id: Some(6),
        artifact_location_id: Some(7),
        staging_path: Some("/tmp/voom-stage/2/3/out.mkv".to_owned()),
        selected_streams: Vec::new(),
        default_streams: Vec::new(),
        clear_default_streams: Vec::new(),
        failure_class: voom_core::FailureClass::MalformedWorkerResult,
        error_code: "MALFORMED_WORKER_RESULT".to_owned(),
        message: "worker result did not match requested remux streams".to_owned(),
        provider: Some("mkvtoolnix".to_owned()),
        provider_version: Some("test".to_owned()),
    };

    let json = serde_json::to_value(Event::ArtifactRemuxFailed(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.remux_failed");
    assert_eq!(json["payload"]["artifact_handle_id"], 6);
    assert_eq!(json["payload"]["artifact_location_id"], 7);
    assert_eq!(json["payload"]["failure_class"], "malformed_worker_result");
    assert_eq!(json["payload"]["error_code"], "MALFORMED_WORKER_RESULT");

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactRemuxFailed(q) if q == p));
    assert_eq!(
        Event::ArtifactRemuxFailed(p).kind(),
        EventKind::ArtifactRemuxFailed
    );
}

#[test]
fn artifact_remux_payloads_reject_unknown_fields() {
    let raw = serde_json::json!({
        "kind": "artifact.remux_started",
        "payload": {
            "job_id": 1,
            "ticket_id": 2,
            "lease_id": 3,
            "source_file_version_id": 4,
            "source_file_location_id": 5,
            "staging_path": "/tmp/voom-stage/2/3/out.mkv",
            "selected_streams": [
                {
                    "snapshot_stream_id": "stream-0",
                    "provider_stream_index": 0,
                    "unexpected": true
                }
            ],
            "default_streams": [],
            "clear_default_streams": [],
            "track_order": ["video"],
            "provider": null,
            "provider_version": null
        }
    });

    let err = serde_json::from_value::<Event>(raw).unwrap_err();

    assert!(
        err.to_string().contains("unknown field"),
        "unknown remux payload fields should reject: {err}"
    );
}

#[test]
fn artifact_verification_succeeded_rejects_failure_shape() {
    let raw = serde_json::json!({
        "kind": "artifact.verification_succeeded",
        "payload": {
            "verification_id": 20,
            "artifact_handle_id": 10,
            "artifact_location_id": 11,
            "worker_id": 12,
            "error_code": "ARTIFACT_CHECKSUM_MISMATCH"
        }
    });
    let err = serde_json::from_value::<Event>(raw).unwrap_err();
    assert!(
        err.to_string().contains("observed_size_bytes"),
        "missing success facts should reject: {err}"
    );
}

#[test]
fn artifact_commit_started_payload_round_trip() {
    let p = ArtifactCommitStartedPayload {
        commit_record_id: 30,
        artifact_handle_id: 10,
        source_file_version_id: 12,
        verification_id: 20,
        target_path: "/media/final.bin".to_owned(),
        temp_path: "/media/.final.bin.tmp".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactCommitStarted(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.commit_started");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactCommitStarted(q) if q == p));
    assert_eq!(
        Event::ArtifactCommitStarted(p).kind(),
        EventKind::ArtifactCommitStarted
    );
}

#[test]
fn artifact_commit_completed_payload_round_trip() {
    let p = ArtifactCommitCompletedPayload {
        commit_record_id: 30,
        artifact_handle_id: 10,
        result_file_version_id: 31,
        result_file_location_id: 32,
        target_path: "/media/final.bin".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactCommitCompleted(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.commit_completed");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactCommitCompleted(q) if q == p));
    assert_eq!(
        Event::ArtifactCommitCompleted(p).kind(),
        EventKind::ArtifactCommitCompleted
    );
}

#[test]
fn artifact_commit_failed_pre_mutation_payload_round_trip() {
    let p = ArtifactCommitFailedPreMutationPayload {
        artifact_handle_id: 10,
        commit_record_id: None,
        target_path: "/media/final.bin".to_owned(),
        error_code: "ARTIFACT_NOT_VERIFIED".to_owned(),
        message: "staged artifact has no successful verification".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactCommitFailedPreMutation(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.commit_failed_pre_mutation");
    assert_eq!(json["payload"]["commit_record_id"], serde_json::Value::Null);
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactCommitFailedPreMutation(q) if q == p));
    assert_eq!(
        Event::ArtifactCommitFailedPreMutation(p).kind(),
        EventKind::ArtifactCommitFailedPreMutation
    );
}

#[test]
fn artifact_commit_recovery_required_payload_round_trip() {
    let p = ArtifactCommitRecoveryRequiredPayload {
        commit_record_id: 30,
        artifact_handle_id: 10,
        target_path: "/media/final.bin".to_owned(),
        temp_path: "/media/.final.bin.tmp".to_owned(),
        recovery_reason: "target_appeared_after_prepare".to_owned(),
        error_code: "TARGET_EXISTS".to_owned(),
        message: "target path appeared during promotion".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactCommitRecoveryRequired(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.commit_recovery_required");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactCommitRecoveryRequired(q) if q == p));
    assert_eq!(
        Event::ArtifactCommitRecoveryRequired(p).kind(),
        EventKind::ArtifactCommitRecoveryRequired
    );
}
