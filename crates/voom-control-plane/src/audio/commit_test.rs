use super::*;

use serde_json::json;
use sqlx::Row;
use time::OffsetDateTime;
use voom_core::rng_test_support::FrozenRng;
use voom_plan::audio::{AudioDispositionFact, SnapshotAudioStreamFact};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome, MediaSnapshot};
use voom_worker_protocol::{
    AudioDispositionFact as WorkerAudioDispositionFact, AudioOutputStreamFact, AudioStreamRef,
    TranscodeAudioResult, TranscodeAudioSelection, TranscodeAudioStatus,
};

use crate::audio::selection::{SelectedAudioStream, TranscodeAudioSelectionPlan};

#[tokio::test]
async fn record_staged_audio_transcode_writes_selected_stream_lineage() {
    let (cp, _db, dir) = fixture().await;
    let source = seed_source(&cp, dir.path().join("source.mkv"), b"source").await;
    let staging_path = dir.path().join("staged.mkv");

    let staged = record_staged_audio_transcode(
        &cp,
        &transcode_input(source.file_version_id),
        source.file_location_id,
        &staging_path,
        &transcode_result(),
    )
    .await
    .unwrap();

    let lineage = source_lineage(&cp, staged.artifact_handle_id).await;
    assert_eq!(lineage["operation"], "transcode_audio");
    assert_eq!(lineage["selected_snapshot_stream_ids"], json!(["a-1"]));
}

#[test]
fn synthesis_probe_preserves_source_and_binds_stable_companion_identity() {
    let source = synthesis_source_snapshot();
    let selection = synthesis_selection();
    let result = synthesis_result();
    let mut payload = synthesis_probe_payload();

    bind_synthesis_companions(&mut payload, &source, &selection, &result).unwrap();

    assert_eq!(payload["streams"][1]["id"], "stream-1");
    assert_eq!(
        payload["streams"][2]["id"],
        "synth_companion_26daba3dd2f8074c"
    );
    assert_eq!(payload["streams"][2]["channels"], 2);
    assert_eq!(payload["streams"][2]["language"], "eng");
}

#[test]
fn synthesis_probe_rejects_partial_bundle_before_identity_binding() {
    let source = synthesis_source_snapshot();
    let selection = synthesis_selection();
    let result = synthesis_result();
    let mut payload = synthesis_probe_payload();
    payload["streams"].as_array_mut().unwrap().pop();

    let error = bind_synthesis_companions(&mut payload, &source, &selection, &result).unwrap_err();

    assert_eq!(error.error_code().as_str(), "MALFORMED_WORKER_RESULT");
    assert!(
        error
            .to_string()
            .contains("every source stream and companion")
    );
}

#[test]
fn synthesis_probe_rejects_changed_source_media_facts() {
    let source = synthesis_source_snapshot();
    let selection = synthesis_selection();
    let result = synthesis_result();
    let mut payload = synthesis_probe_payload();
    payload["streams"][1]["channels"] = json!(2);

    let error = bind_synthesis_companions(&mut payload, &source, &selection, &result).unwrap_err();

    assert_eq!(error.error_code().as_str(), "MALFORMED_WORKER_RESULT");
    assert!(error.to_string().contains("changed source provider stream"));
}

#[derive(Debug, Clone, Copy)]
struct SeededSource {
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
}

async fn fixture() -> (
    crate::ControlPlane,
    tempfile::NamedTempFile,
    tempfile::TempDir,
) {
    let db = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (
        cp,
        db,
        tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap(),
    )
}

async fn seed_source(cp: &crate::ControlPlane, path: PathBuf, bytes: &[u8]) -> SeededSource {
    std::fs::write(&path, bytes).unwrap();
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.display().to_string(),
                content_hash: blake3_checksum(bytes),
                size_bytes: u64::try_from(bytes.len()).unwrap(),
                observed_at: OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id,
        file_location_id,
        ..
    } = outcome
    else {
        panic!("seed_source should create a new file asset");
    };
    SeededSource {
        file_version_id,
        file_location_id,
    }
}

async fn source_lineage(cp: &crate::ControlPlane, id: ArtifactHandleId) -> serde_json::Value {
    let row = sqlx::query("SELECT source_lineage FROM artifact_handles WHERE id = ?")
        .bind(i64::try_from(id.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let lineage: String = row.try_get("source_lineage").unwrap();
    serde_json::from_str(&lineage).unwrap()
}

fn transcode_input(source_file_version_id: FileVersionId) -> ExecuteTranscodeAudioInput {
    ExecuteTranscodeAudioInput {
        job_id: voom_core::JobId(1),
        ticket_id: voom_core::TicketId(1),
        lease_id: voom_core::LeaseId(1),
        source_file_version_id,
        source_location_id: None,
        operation_payload: json!({}),
        staging_root: PathBuf::new(),
        target_dir: PathBuf::new(),
        backup_root: None,
    }
}

fn transcode_result() -> TranscodeAudioResult {
    let input = observed(6, &blake3_checksum(b"source"));
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
        selected_output_streams: Vec::new(),
    }
}

fn synthesis_source_snapshot() -> MediaSnapshot {
    MediaSnapshot {
        id: MediaSnapshotId(9),
        file_version_id: FileVersionId(8),
        probed_by: None,
        probed_at: OffsetDateTime::UNIX_EPOCH,
        payload: json!({
            "container": "mkv",
            "streams": [
                {
                    "id": "video-0",
                    "index": 0,
                    "kind": "video",
                    "codec_name": "h264",
                    "width": 1920,
                    "height": 1080,
                    "pixel_format": "yuv420p"
                },
                {
                    "id": "audio-1",
                    "index": 1,
                    "kind": "audio",
                    "codec_name": "ac3",
                    "channels": 6,
                    "language": "eng",
                    "title": "Main",
                    "disposition": {
                        "default": true,
                        "forced": false,
                        "commentary": false
                    }
                }
            ]
        }),
    }
}

fn synthesis_selection() -> TranscodeAudioSelectionPlan {
    let companion_id = "synth_companion_26daba3dd2f8074c".to_owned();
    let source = SnapshotAudioStreamFact {
        snapshot_stream_id: "audio-1".to_owned(),
        provider_stream_index: 1,
        codec: Some("ac3".to_owned()),
        language: Some("eng".to_owned()),
        title: Some("Main".to_owned()),
        channels: Some(6),
        default: true,
        commentary: Some(false),
        disposition: AudioDispositionFact {
            default: true,
            forced: false,
            commentary: Some(false),
        },
    };
    TranscodeAudioSelectionPlan {
        selection: TranscodeAudioSelection {
            selected_streams: vec![AudioStreamRef {
                snapshot_stream_id: companion_id.clone(),
                provider_stream_index: 1,
            }],
        },
        selected_streams: vec![SelectedAudioStream {
            stream: AudioStreamRef {
                snapshot_stream_id: companion_id,
                provider_stream_index: 1,
            },
            source,
        }],
        target_codec: "aac".to_owned(),
        container: "mkv".to_owned(),
        operation_id: Some("node_0123456789abcdef".to_owned()),
        add_track: true,
        target_channels: Some(2),
    }
}

fn synthesis_result() -> TranscodeAudioResult {
    let input = observed(6, "blake3:source");
    TranscodeAudioResult {
        status: TranscodeAudioStatus::Transcoded,
        provider: "ffmpeg".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output: observed(10, "blake3:output"),
        output_container: "mkv".to_owned(),
        selected_snapshot_stream_ids: vec!["synth_companion_26daba3dd2f8074c".to_owned()],
        output_audio_codecs: vec!["aac".to_owned()],
        selected_output_streams: vec![AudioOutputStreamFact {
            snapshot_stream_id: "synth_companion_26daba3dd2f8074c".to_owned(),
            output_provider_stream_index: 2,
            codec: "aac".to_owned(),
            language: Some("eng".to_owned()),
            title: Some("Main".to_owned()),
            default: Some(true),
            disposition: Some(WorkerAudioDispositionFact {
                default: Some(true),
                forced: Some(false),
                commentary: Some(false),
            }),
            channels: Some(2),
        }],
    }
}

fn synthesis_probe_payload() -> serde_json::Value {
    json!({
        "container": "mkv",
        "streams": [
            {
                "id": "stream-0",
                "index": 0,
                "kind": "video",
                "codec_name": "h264",
                "width": 1920,
                "height": 1080,
                "pixel_format": "yuv420p"
            },
            {
                "id": "stream-1",
                "index": 1,
                "kind": "audio",
                "codec_name": "ac3",
                "channels": 6,
                "language": "eng",
                "title": "Main",
                "disposition": {
                    "default": true,
                    "forced": false,
                    "commentary": false
                }
            },
            {
                "id": "stream-2",
                "index": 2,
                "kind": "audio",
                "codec_name": "aac",
                "channels": 2,
                "language": "eng",
                "title": "Main",
                "disposition": {
                    "default": true,
                    "forced": false,
                    "commentary": false
                }
            }
        ]
    })
}

fn observed(size_bytes: u64, content_hash: &str) -> voom_worker_protocol::AudioObservedFacts {
    voom_worker_protocol::AudioObservedFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
