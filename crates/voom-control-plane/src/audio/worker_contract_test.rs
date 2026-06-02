use super::*;

use std::path::PathBuf;

use voom_plan::audio::{AudioBundleRole, AudioDispositionFact, SnapshotAudioStreamFact};
use voom_store::repo::identity::{FileLocation, FileLocationKind, FileVersion, ProducedBy};
use voom_worker_protocol::{
    AudioDispositionFact as WorkerDisposition, AudioOutputStreamFact, ExtractAudioResult,
    ExtractAudioStatus, TranscodeAudioResult, TranscodeAudioStatus,
};

use crate::audio::selection::SelectedAudioStream;

#[test]
fn input_pre_post_drift_returns_artifact_checksum_mismatch() {
    let selection = transcode_selection();
    let mut result = transcode_result();
    result.input_post.content_hash = "blake3:changed".to_owned();

    let err = validate_transcode_result(&selected_source(), &selection, &result).unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::ArtifactChecksumMismatch
    );
}

#[test]
fn selected_stream_id_mismatch_returns_malformed_worker_result() {
    let selection = transcode_selection();
    let mut result = transcode_result();
    result.selected_snapshot_stream_ids = vec!["other".to_owned()];

    let err = validate_transcode_result(&selected_source(), &selection, &result).unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
}

#[test]
fn selected_output_ordering_mismatch_returns_malformed_worker_result() {
    let selection = transcode_selection_two();
    let mut result = transcode_result();
    result.selected_snapshot_stream_ids = vec!["a-1".to_owned(), "a-2".to_owned()];
    result.output_audio_codecs = vec!["aac".to_owned(), "aac".to_owned()];
    result.selected_output_streams = vec![output_stream("a-2"), output_stream("a-1")];

    let err = validate_transcode_result(&selected_source(), &selection, &result).unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
}

#[test]
fn transcode_output_codec_mismatch_returns_malformed_worker_result() {
    let selection = transcode_selection();
    let mut result = transcode_result();
    result.output_audio_codecs = vec!["opus".to_owned()];

    let err = validate_transcode_result(&selected_source(), &selection, &result).unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
}

#[test]
fn transcode_preserved_output_facts_that_differ_from_source_return_malformed_worker_result() {
    let selection = transcode_selection();
    for mutate in [
        |stream: &mut AudioOutputStreamFact| stream.language = Some("jpn".to_owned()),
        |stream: &mut AudioOutputStreamFact| stream.title = Some("Dub".to_owned()),
        |stream: &mut AudioOutputStreamFact| stream.default = Some(false),
        |stream: &mut AudioOutputStreamFact| {
            stream.disposition = Some(WorkerDisposition {
                default: Some(true),
                forced: Some(true),
                commentary: Some(false),
            });
        },
        |stream: &mut AudioOutputStreamFact| stream.channels = Some(6),
    ] {
        let mut result = transcode_result();
        mutate(&mut result.selected_output_streams[0]);

        let err = validate_transcode_result(&selected_source(), &selection, &result).unwrap_err();

        assert_eq!(
            err.error_code(),
            voom_core::ErrorCode::MalformedWorkerResult
        );
    }
}

#[test]
fn extraction_output_container_or_codec_mismatch_returns_malformed_worker_result() {
    let selection = extract_selection();
    let mut result = extract_result();
    result.output_container = "mkv".to_owned();

    let err = validate_extract_result(&selected_source(), &selection, &result).unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );

    let mut result = extract_result();
    result.output_audio_codec = "aac".to_owned();
    let err = validate_extract_result(&selected_source(), &selection, &result).unwrap_err();
    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
}

#[test]
fn extraction_missing_language_or_title_present_on_source_returns_malformed_worker_result() {
    let selection = extract_selection();
    let mut result = extract_result();
    result.output_language = None;

    let err = validate_extract_result(&selected_source(), &selection, &result).unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );

    let mut result = extract_result();
    result.output_title = None;
    let err = validate_extract_result(&selected_source(), &selection, &result).unwrap_err();
    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
}

#[tokio::test]
async fn output_file_facts_must_match_result_facts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.ogg");
    tokio::fs::write(&path, b"actual").await.unwrap();
    let result = extract_result();

    let err = require_extract_output_file_matches_result(&path, &result)
        .await
        .unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::ArtifactChecksumMismatch
    );
}

fn selected_source() -> crate::audio::source::SelectedSource {
    crate::audio::source::SelectedSource {
        version: FileVersion {
            id: voom_core::FileVersionId(1),
            file_asset_id: voom_core::FileAssetId(1),
            content_hash: "blake3:source".to_owned(),
            size_bytes: 12,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            retired_at: None,
            epoch: 0,
        },
        location: FileLocation {
            id: voom_core::FileLocationId(1),
            file_version_id: voom_core::FileVersionId(1),
            kind: FileLocationKind::LocalPath,
            value: "/library/source.mkv".to_owned(),
            proof_kind: None,
            proof_value: None,
            observed_at: time::OffsetDateTime::UNIX_EPOCH,
            retired_at: None,
            epoch: 0,
        },
        canonical_path: PathBuf::from("/library/source.mkv"),
    }
}

fn transcode_selection() -> TranscodeAudioSelectionPlan {
    TranscodeAudioSelectionPlan {
        selection: voom_worker_protocol::TranscodeAudioSelection {
            selected_streams: vec![voom_worker_protocol::AudioStreamRef {
                snapshot_stream_id: "a-1".to_owned(),
                provider_stream_index: 1,
            }],
        },
        selected_streams: vec![SelectedAudioStream {
            stream: voom_worker_protocol::AudioStreamRef {
                snapshot_stream_id: "a-1".to_owned(),
                provider_stream_index: 1,
            },
            source: source_fact("a-1"),
        }],
        target_codec: "aac".to_owned(),
        container: "mkv".to_owned(),
    }
}

fn transcode_selection_two() -> TranscodeAudioSelectionPlan {
    let mut selection = transcode_selection();
    selection
        .selection
        .selected_streams
        .push(voom_worker_protocol::AudioStreamRef {
            snapshot_stream_id: "a-2".to_owned(),
            provider_stream_index: 2,
        });
    selection.selected_streams.push(SelectedAudioStream {
        stream: voom_worker_protocol::AudioStreamRef {
            snapshot_stream_id: "a-2".to_owned(),
            provider_stream_index: 2,
        },
        source: source_fact("a-2"),
    });
    selection
}

fn extract_selection() -> ExtractAudioSelectionPlan {
    ExtractAudioSelectionPlan {
        stream: voom_worker_protocol::AudioStreamRef {
            snapshot_stream_id: "a-1".to_owned(),
            provider_stream_index: 1,
        },
        source: source_fact("a-1"),
        role: AudioBundleRole::ExternalAudio,
        target_codec: "opus".to_owned(),
        container: "ogg".to_owned(),
    }
}

fn source_fact(id: &str) -> SnapshotAudioStreamFact {
    SnapshotAudioStreamFact {
        snapshot_stream_id: id.to_owned(),
        provider_stream_index: 1,
        codec: Some("ac3".to_owned()),
        language: Some("eng".to_owned()),
        title: Some("Main".to_owned()),
        channels: Some(2),
        default: true,
        commentary: Some(false),
        disposition: AudioDispositionFact {
            default: true,
            forced: false,
            commentary: Some(false),
        },
    }
}

fn transcode_result() -> TranscodeAudioResult {
    let input = observed(12, "blake3:source");
    TranscodeAudioResult {
        status: TranscodeAudioStatus::Transcoded,
        provider: "ffmpeg".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output: observed(10, "blake3:output"),
        output_container: "mkv".to_owned(),
        selected_snapshot_stream_ids: vec!["a-1".to_owned()],
        output_audio_codecs: vec!["aac".to_owned()],
        selected_output_streams: vec![output_stream("a-1")],
    }
}

fn extract_result() -> ExtractAudioResult {
    let input = observed(12, "blake3:source");
    ExtractAudioResult {
        status: ExtractAudioStatus::Extracted,
        provider: "ffmpeg".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output: observed(10, "blake3:output"),
        output_container: "ogg".to_owned(),
        output_audio_codec: "opus".to_owned(),
        selected_snapshot_stream_id: "a-1".to_owned(),
        output_language: Some("eng".to_owned()),
        output_title: Some("Main".to_owned()),
    }
}

fn output_stream(id: &str) -> AudioOutputStreamFact {
    AudioOutputStreamFact {
        snapshot_stream_id: id.to_owned(),
        output_provider_stream_index: 0,
        codec: "aac".to_owned(),
        language: Some("eng".to_owned()),
        title: Some("Main".to_owned()),
        default: Some(true),
        disposition: Some(WorkerDisposition {
            default: Some(true),
            forced: Some(false),
            commentary: Some(false),
        }),
        channels: Some(2),
    }
}

fn observed(size_bytes: u64, content_hash: &str) -> voom_worker_protocol::AudioObservedFacts {
    voom_worker_protocol::AudioObservedFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}
