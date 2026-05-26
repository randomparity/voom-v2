use voom_core::{ArtifactHandleId, ArtifactLocationId, FileLocationId, VoomError};
use voom_worker_protocol::{
    AudioObservedFacts, AudioOutputStreamFact, AudioStreamRef, ExtractAudioResult,
    ExtractAudioStatus, TranscodeAudioResult, TranscodeAudioStatus,
};

use super::*;
use crate::audio::{ExecuteExtractAudioInput, ExecuteTranscodeAudioInput};

#[test]
fn audio_stream_payloads_preserve_snapshot_ids_and_provider_indexes() {
    let streams = vec![AudioStreamRef {
        snapshot_stream_id: "audio-1".to_owned(),
        provider_stream_index: 7,
    }];

    let payloads = stream_payloads(&streams);

    assert_eq!(payloads[0].snapshot_stream_id, "audio-1");
    assert_eq!(payloads[0].provider_stream_index, 7);
}

#[test]
fn transcode_succeeded_payload_carries_result_and_source_ids() {
    let input = transcode_input();
    let result = transcode_result();

    let payload = transcode_succeeded_payload(
        &input,
        TranscodeSucceededContext {
            source_location: FileLocationId(5),
            source_media_snapshot: 6,
            artifact_handle: ArtifactHandleId(8),
            artifact_location: ArtifactLocationId(9),
        },
        "/tmp/voom-stage/2/3/out.mkv".to_owned(),
        stream_payloads(&[AudioStreamRef {
            snapshot_stream_id: "audio-1".to_owned(),
            provider_stream_index: 7,
        }]),
        &result,
    );

    assert_eq!(payload.job_id, 1);
    assert_eq!(payload.ticket_id, 2);
    assert_eq!(payload.lease_id, Some(3));
    assert_eq!(payload.source_file_version_id, 4);
    assert_eq!(payload.source_file_location_id, 5);
    assert_eq!(payload.source_media_snapshot_id, 6);
    assert_eq!(payload.artifact_handle_id, 8);
    assert_eq!(payload.artifact_location_id, 9);
    assert_eq!(payload.selected_streams[0].provider_stream_index, 7);
    assert_eq!(payload.selected_snapshot_stream_ids, ["audio-1"]);
    assert_eq!(payload.output_audio_codecs, ["aac"]);
    assert_eq!(payload.provider, "ffmpeg");
    assert_eq!(payload.provider_version, "6.1");
}

#[test]
fn extract_failed_payload_uses_public_error_code_and_known_ids() {
    let input = extract_input();
    let error = VoomError::Config("source stream missing".to_owned());

    let payload = extract_failed_payload(ExtractFailedEventPayloadInput {
        input: &input,
        source_location_id: Some(FileLocationId(5)),
        source_media_snapshot_id: Some(6),
        artifact_handle_id: Some(ArtifactHandleId(8)),
        artifact_location_id: Some(ArtifactLocationId(9)),
        staging_path: Some("/tmp/voom-stage/2/3/out.ogg".to_owned()),
        selected_stream: Some(ArtifactAudioStreamPayload {
            snapshot_stream_id: "audio-2".to_owned(),
            provider_stream_index: 11,
        }),
        role: Some("external_audio".to_owned()),
        result: Some(&extract_result()),
        error: &error,
    });

    assert_eq!(payload.error_code, "CONFIG_INVALID");
    assert_eq!(payload.source_file_location_id, Some(5));
    assert_eq!(payload.source_media_snapshot_id, Some(6));
    assert_eq!(payload.artifact_handle_id, Some(8));
    assert_eq!(payload.artifact_location_id, Some(9));
    assert_eq!(payload.provider.as_deref(), Some("ffmpeg"));
    assert_eq!(payload.provider_version.as_deref(), Some("6.1"));
}

fn transcode_input() -> ExecuteTranscodeAudioInput {
    ExecuteTranscodeAudioInput {
        job_id: voom_core::JobId(1),
        ticket_id: voom_core::TicketId(2),
        lease_id: voom_core::LeaseId(3),
        source_file_version_id: voom_core::FileVersionId(4),
        source_location_id: Some(FileLocationId(5)),
        operation_payload: serde_json::json!({"source_media_snapshot_id": 6}),
        staging_root: "/tmp/stage".into(),
        target_dir: "/tmp/out".into(),
    }
}

fn extract_input() -> ExecuteExtractAudioInput {
    ExecuteExtractAudioInput {
        job_id: voom_core::JobId(1),
        ticket_id: voom_core::TicketId(2),
        lease_id: voom_core::LeaseId(3),
        source_file_version_id: voom_core::FileVersionId(4),
        source_location_id: Some(FileLocationId(5)),
        source_bundle_id: voom_core::ids::BundleId(70),
        operation_payload: serde_json::json!({"source_media_snapshot_id": 6}),
        staging_root: "/tmp/stage".into(),
        target_dir: "/tmp/out".into(),
    }
}

fn transcode_result() -> TranscodeAudioResult {
    TranscodeAudioResult {
        status: TranscodeAudioStatus::Transcoded,
        provider: "ffmpeg".to_owned(),
        provider_version: "6.1".to_owned(),
        input_pre: observed("hash-in"),
        input_post: observed("hash-in"),
        output: observed("hash-out"),
        output_container: "mkv".to_owned(),
        selected_snapshot_stream_ids: vec!["audio-1".to_owned()],
        output_audio_codecs: vec!["aac".to_owned()],
        selected_output_streams: vec![AudioOutputStreamFact {
            snapshot_stream_id: "audio-1".to_owned(),
            output_provider_stream_index: 0,
            codec: "aac".to_owned(),
            language: Some("eng".to_owned()),
            title: Some("Main".to_owned()),
            default: Some(true),
            disposition: None,
            channels: Some(2),
        }],
    }
}

fn extract_result() -> ExtractAudioResult {
    ExtractAudioResult {
        status: ExtractAudioStatus::Extracted,
        provider: "ffmpeg".to_owned(),
        provider_version: "6.1".to_owned(),
        input_pre: observed("hash-in"),
        input_post: observed("hash-in"),
        output: observed("hash-out"),
        output_container: "ogg".to_owned(),
        output_audio_codec: "opus".to_owned(),
        selected_snapshot_stream_id: "audio-2".to_owned(),
        output_language: Some("eng".to_owned()),
        output_title: Some("Commentary".to_owned()),
    }
}

fn observed(content_hash: &str) -> AudioObservedFacts {
    AudioObservedFacts {
        size_bytes: 12,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: Some("/tmp/out".to_owned()),
    }
}
