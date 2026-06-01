use super::*;

use serde_json::json;
use sqlx::Row;
use time::OffsetDateTime;
use voom_core::ErrorCode;
use voom_core::rng_test_support::FrozenRng;
use voom_store::repo::artifacts::{
    ArtifactRepo, ArtifactVerificationStatus, NewArtifactVerification,
};
use voom_store::repo::bundles::NewAssetBundle;
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::repo::identity::{MediaWorkKind, NewMediaVariant, NewMediaWork};
use voom_store::repo::workers::{NewWorker, WorkerKind};
use voom_worker_protocol::{
    ExtractAudioResult, ExtractAudioStatus, TranscodeAudioResult, TranscodeAudioStatus,
};

#[tokio::test]
async fn record_staged_audio_transcode_writes_lineage_with_selected_stream_ids() {
    let (cp, _db, dir) = fixture().await;
    let source = seed_source(&cp, dir.path().join("source.mkv"), b"source").await;
    let staging_path = dir.path().join("staged.mkv");
    let result = transcode_result();

    let staged = record_staged_audio_transcode(
        &cp,
        &transcode_input(source.file_version_id),
        source.file_location_id,
        &staging_path,
        &result,
    )
    .await
    .unwrap();

    let lineage = source_lineage(&cp, staged.artifact_handle_id).await;
    assert_eq!(lineage["operation"], "transcode_audio");
    assert_eq!(lineage["selected_snapshot_stream_ids"], json!(["a-1"]));
}

#[tokio::test]
async fn record_staged_audio_extract_writes_lineage_with_stream_id_and_role() {
    let (cp, _db, dir) = fixture().await;
    let source = seed_source(&cp, dir.path().join("source.mkv"), b"source").await;
    let staging_path = dir.path().join("staged.ogg");
    let result = extract_result();

    let staged = record_staged_audio_extract(
        &cp,
        &extract_input(source.file_version_id),
        source.file_location_id,
        &staging_path,
        &extract_selection(),
        &result,
    )
    .await
    .unwrap();

    let lineage = source_lineage(&cp, staged.artifact_handle_id).await;
    assert_eq!(lineage["operation"], "extract_audio");
    assert_eq!(lineage["selected_snapshot_stream_id"], "a-1");
    assert_eq!(lineage["intended_role"], "external_audio");
}

#[tokio::test]
async fn sidecar_prepare_rejects_missing_staging_before_pending_commit() {
    let (cp, _db, dir) = fixture().await;
    let source = seed_source(&cp, dir.path().join("source.mkv"), b"source").await;
    let staged = record_staged_audio_extract(
        &cp,
        &extract_input(source.file_version_id),
        source.file_location_id,
        &dir.path().join("missing-staged.ogg"),
        &extract_selection(),
        &extract_result(),
    )
    .await
    .unwrap();
    let verification =
        record_successful_verification(&cp, &staged, &dir.path().join("missing-staged.ogg")).await;

    let err = commit_audio_extract_sidecar(
        &cp,
        CommitAudioExtractSidecarInput {
            artifact_handle_id: staged.artifact_handle_id,
            verification_id: verification,
            source_file_version_id: source.file_version_id,
            source_bundle_id: voom_core::ids::BundleId(777),
            role: voom_plan::audio::AudioBundleRole::ExternalAudio,
            staging_path: dir.path().join("missing-staged.ogg"),
            target_path: dir.path().join("target.ogg"),
            output: observed(10, "blake3:output"),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactUnavailable);
    assert_eq!(artifact_commit_record_count(&cp).await, 0);
    assert_eq!(event_count(&cp, "artifact.commit_started").await, 0);
    assert_eq!(
        event_count(&cp, "artifact.commit_recovery_required").await,
        0
    );
}

#[tokio::test]
async fn sidecar_prepare_rejects_staging_fact_mismatch_before_pending_commit() {
    let (cp, _db, dir) = fixture().await;
    let source = seed_source(&cp, dir.path().join("source.mkv"), b"source").await;
    let staging_path = dir.path().join("staged.ogg");
    std::fs::write(&staging_path, b"changed").unwrap();
    let staged = record_staged_audio_extract(
        &cp,
        &extract_input(source.file_version_id),
        source.file_location_id,
        &staging_path,
        &extract_selection(),
        &extract_result(),
    )
    .await
    .unwrap();
    let verification = record_successful_verification(&cp, &staged, &staging_path).await;

    let err = commit_audio_extract_sidecar(
        &cp,
        CommitAudioExtractSidecarInput {
            artifact_handle_id: staged.artifact_handle_id,
            verification_id: verification,
            source_file_version_id: source.file_version_id,
            source_bundle_id: voom_core::ids::BundleId(777),
            role: voom_plan::audio::AudioBundleRole::ExternalAudio,
            staging_path,
            target_path: dir.path().join("target.ogg"),
            output: observed(10, "blake3:output"),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert_eq!(artifact_commit_record_count(&cp).await, 0);
    assert_eq!(event_count(&cp, "artifact.commit_started").await, 0);
    assert_eq!(
        event_count(&cp, "artifact.commit_recovery_required").await,
        0
    );
}

#[tokio::test]
async fn sidecar_commit_emits_standard_artifact_commit_events() {
    let (cp, _db, dir) = fixture().await;
    let source = seed_source(&cp, dir.path().join("source.mkv"), b"source").await;
    let bundle = seed_bundle(&cp).await;
    let staging_path = dir.path().join("staged.ogg");
    std::fs::write(&staging_path, b"sidecar").unwrap();
    let staged = record_staged_audio_extract(
        &cp,
        &extract_input(source.file_version_id),
        source.file_location_id,
        &staging_path,
        &extract_selection(),
        &extract_result_with_bytes(b"sidecar"),
    )
    .await
    .unwrap();
    let verification = record_successful_verification(&cp, &staged, &staging_path).await;

    let report = commit_audio_extract_sidecar(
        &cp,
        CommitAudioExtractSidecarInput {
            artifact_handle_id: staged.artifact_handle_id,
            verification_id: verification,
            source_file_version_id: source.file_version_id,
            source_bundle_id: bundle.id,
            role: voom_plan::audio::AudioBundleRole::ExternalAudio,
            staging_path: staging_path.clone(),
            target_path: dir.path().join("target.ogg"),
            output: observed(
                u64::try_from(b"sidecar".len()).unwrap(),
                &blake3_checksum(b"sidecar"),
            ),
        },
    )
    .await
    .unwrap();

    assert_eq!(report.state, ArtifactCommitState::Committed);
    assert_eq!(event_count(&cp, "artifact.commit_started").await, 1);
    assert_eq!(event_count(&cp, "artifact.commit_completed").await, 1);
    let completed = latest_event_payload(&cp, "artifact.commit_completed").await;
    assert_eq!(completed["commit_record_id"], report.commit_record_id.0);
    assert_eq!(
        completed["result_file_version_id"],
        report.result_file_version_id.unwrap().0
    );
    assert_eq!(
        completed["result_file_location_id"],
        report.result_file_location_id.unwrap().0
    );
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

async fn seed_bundle(cp: &crate::ControlPlane) -> voom_store::repo::bundles::AssetBundle {
    let work = cp
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "movie".to_owned(),
            provisional: true,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let variant = cp
        .create_media_variant(NewMediaVariant {
            media_work_id: work.id,
            label: "main".to_owned(),
            provisional: true,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    cp.create_bundle(NewAssetBundle {
        media_variant_id: variant.id,
        display_name: "bundle".to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
    })
    .await
    .unwrap()
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

async fn record_successful_verification(
    cp: &crate::ControlPlane,
    staged: &StagedAudioArtifact,
    path: &std::path::Path,
) -> ArtifactVerificationId {
    let worker = cp
        .register_worker(NewWorker {
            name: "audio-verify".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
            node_id: None,
        })
        .await
        .unwrap();
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let verification = cp
        .artifacts()
        .record_verification_in_tx(
            &mut tx,
            NewArtifactVerification {
                artifact_handle_id: staged.artifact_handle_id,
                artifact_location_id: staged.artifact_location_id,
                path: path.display().to_string(),
                worker_id: worker.id,
                status: ArtifactVerificationStatus::Succeeded,
                expected_size_bytes: 10,
                expected_checksum: "blake3:output".to_owned(),
                observed_size_bytes: Some(10),
                observed_checksum: Some("blake3:output".to_owned()),
                failure_class: None,
                error_code: None,
                message: None,
                report: json!({}),
                started_at: OffsetDateTime::UNIX_EPOCH,
                finished_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    verification.id
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
    }
}

fn extract_input(source_file_version_id: FileVersionId) -> ExecuteExtractAudioInput {
    ExecuteExtractAudioInput {
        job_id: voom_core::JobId(1),
        ticket_id: voom_core::TicketId(1),
        lease_id: voom_core::LeaseId(1),
        source_file_version_id,
        source_location_id: None,
        source_bundle_id: voom_core::ids::BundleId(1),
        operation_payload: json!({}),
        staging_root: PathBuf::new(),
        target_dir: PathBuf::new(),
    }
}

fn extract_selection() -> ExtractAudioSelectionPlan {
    ExtractAudioSelectionPlan {
        stream: voom_worker_protocol::AudioStreamRef {
            snapshot_stream_id: "a-1".to_owned(),
            provider_stream_index: 1,
        },
        source: voom_plan::audio::SnapshotAudioStreamFact {
            snapshot_stream_id: "a-1".to_owned(),
            provider_stream_index: 1,
            codec: Some("aac".to_owned()),
            language: Some("eng".to_owned()),
            title: Some("Main".to_owned()),
            channels: Some(2),
            default: true,
            disposition: voom_plan::audio::AudioDispositionFact {
                default: true,
                forced: false,
                commentary: Some(false),
            },
            commentary: Some(false),
        },
        role: voom_plan::audio::AudioBundleRole::ExternalAudio,
        target_codec: "opus".to_owned(),
        container: "ogg".to_owned(),
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

fn extract_result() -> ExtractAudioResult {
    let input = observed(6, &blake3_checksum(b"source"));
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

fn extract_result_with_bytes(bytes: &[u8]) -> ExtractAudioResult {
    let mut result = extract_result();
    result.output.size_bytes = u64::try_from(bytes.len()).unwrap();
    result.output.content_hash = blake3_checksum(bytes);
    result
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

async fn event_count(cp: &crate::ControlPlane, kind: &str) -> i64 {
    let row = sqlx::query("SELECT COUNT(*) AS count FROM events WHERE kind = ?")
        .bind(kind)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    row.try_get("count").unwrap()
}

async fn artifact_commit_record_count(cp: &crate::ControlPlane) -> i64 {
    let row = sqlx::query("SELECT COUNT(*) AS count FROM artifact_commit_records")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    row.try_get("count").unwrap()
}

async fn latest_event_payload(cp: &crate::ControlPlane, kind: &str) -> serde_json::Value {
    let row =
        sqlx::query("SELECT payload FROM events WHERE kind = ? ORDER BY event_id DESC LIMIT 1")
            .bind(kind)
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    let payload: String = row.try_get("payload").unwrap();
    serde_json::from_str(&payload).unwrap()
}
