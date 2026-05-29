use super::*;

use async_trait::async_trait;
use sqlx::Row;
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitRecordId, BundleId};
use voom_core::rng_test_support::FrozenRng;
use voom_core::{JobId, LeaseId, TicketId};
use voom_store::repo::bundles::NewAssetBundle;
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::repo::identity::{MediaWorkKind, NewMediaVariant, NewMediaWork};
use voom_worker_protocol::{
    AudioObservedFacts, AudioOutputStreamFact, ExtractAudioRequest, ExtractAudioResult,
    TranscodeAudioRequest, TranscodeAudioResult, VerifyArtifactObservedFacts,
    VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

#[test]
fn extract_commit_recovery_without_target_is_not_reported_as_success() {
    let report = commit::CommitAudioExtractSidecarReport {
        commit_record_id: ArtifactCommitRecordId(9),
        result_file_version_id: None,
        result_file_location_id: None,
        state: ArtifactCommitState::RecoveryRequired,
        target_path: PathBuf::from("/tmp/target.ogg"),
        temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
        recovery_required: Some(commit::AudioExtractRecoveryReport {
            recovery_reason: "audio sidecar commit failed after durable prepare".to_owned(),
            commit_record_id: ArtifactCommitRecordId(9),
            source_bundle_id: BundleId(7),
            role: "commentary_audio",
            target_path: PathBuf::from("/tmp/target.ogg"),
            target_exists: false,
            temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
            temp_exists: false,
            staging_path: PathBuf::from("/tmp/staged.ogg"),
            staging_exists: true,
            result_file_version_id: None,
            result_file_location_id: None,
            error_code: "CONFLICT",
            message: "bundle membership conflict".to_owned(),
        }),
    };

    let err = ensure_extract_commit_succeeded(&report).unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::CommitFailure);
    assert!(err.to_string().contains("requires recovery"));
    assert!(err.to_string().contains("bundle membership conflict"));
}

#[test]
fn extract_commit_non_committed_state_is_not_reported_as_success() {
    let report = commit::CommitAudioExtractSidecarReport {
        commit_record_id: ArtifactCommitRecordId(10),
        result_file_version_id: None,
        result_file_location_id: None,
        state: ArtifactCommitState::Pending,
        target_path: PathBuf::from("/tmp/target.ogg"),
        temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
        recovery_required: None,
    };

    let err = ensure_extract_commit_succeeded(&report).unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::CommitFailure);
    assert!(err.to_string().contains("ended in Pending"));
}

#[tokio::test]
async fn transcode_failure_records_audio_failed_event() {
    let (cp, _db) = fixture().await;
    let input = transcode_input();

    let err = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &UncalledTranscodeDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::NotFound);
    assert_event_count(&cp, "artifact.audio_transcode_failed", 1).await;
}

#[tokio::test]
async fn late_transcode_failure_event_keeps_attempt_context_and_worker_result() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let input = transcode_input_for_source(&source, &dir);

    let err = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &WritingTranscodeDispatcher {
            output_bytes: b"transcoded".to_vec(),
        },
        &MismatchedVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::VerificationFailure);
    let payload = latest_event_payload(&cp, "artifact.audio_transcode_failed").await;
    assert_eq!(payload["source_file_version_id"], source.version.0);
    assert_eq!(payload["source_file_location_id"], source.location.0);
    assert_eq!(payload["source_media_snapshot_id"], source.snapshot);
    assert!(payload["artifact_handle_id"].as_u64().is_some());
    assert!(payload["artifact_location_id"].as_u64().is_some());
    assert!(
        payload["staging_path"]
            .as_str()
            .unwrap()
            .contains("voom-audio-stage")
    );
    assert_eq!(payload["selected_streams"][0]["snapshot_stream_id"], "a-1");
    assert_eq!(
        payload["selected_output_streams"][0]["output_provider_stream_index"],
        0
    );
    assert_eq!(payload["provider"], "ffmpeg");
    assert_eq!(payload["provider_version"], "test");
}

#[tokio::test]
async fn extract_failure_records_audio_failed_event() {
    let (cp, _db) = fixture().await;
    let input = extract_input();

    let err = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::NotFound);
    assert_event_count(&cp, "artifact.audio_extract_failed", 1).await;
}

#[test]
fn committed_extract_recovery_with_target_is_not_reported_as_success() {
    let report = commit::CommitAudioExtractSidecarReport {
        commit_record_id: ArtifactCommitRecordId(9),
        result_file_version_id: None,
        result_file_location_id: None,
        state: ArtifactCommitState::RecoveryRequired,
        target_path: PathBuf::from("/tmp/target.ogg"),
        temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
        recovery_required: Some(commit::AudioExtractRecoveryReport {
            recovery_reason: "audio sidecar commit failed after durable prepare".to_owned(),
            commit_record_id: ArtifactCommitRecordId(9),
            source_bundle_id: BundleId(7),
            role: "commentary_audio",
            target_path: PathBuf::from("/tmp/target.ogg"),
            target_exists: true,
            temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
            temp_exists: false,
            staging_path: PathBuf::from("/tmp/staged.ogg"),
            staging_exists: true,
            result_file_version_id: None,
            result_file_location_id: None,
            error_code: "CONFLICT",
            message: "bundle membership conflict".to_owned(),
        }),
    };

    let err = ensure_extract_commit_succeeded(&report).unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::CommitFailure);
    assert!(err.to_string().contains("requires recovery"));
}

#[tokio::test]
async fn transcode_post_commit_probe_failure_returns_recovery_report() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let input = transcode_input_for_source(&source, &dir);

    let report = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &WritingTranscodeDispatcher {
            output_bytes: b"transcoded".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &FailingProbeDispatcher,
    )
    .await
    .unwrap();

    assert!(report.commit_record_id.0 > 0);
    assert!(report.result_file_version_id.0 > 0);
    assert!(report.result_file_location_id.0 > 0);
    assert_eq!(report.result_media_snapshot_id.0, 0);
    let recovery = report.commit_recovery_required.unwrap();
    assert_eq!(recovery.commit_record_id, report.commit_record_id);
    assert_eq!(
        recovery.result_file_version_id,
        report.result_file_version_id
    );
    assert_eq!(
        recovery.result_file_location_id,
        report.result_file_location_id
    );
    assert_eq!(recovery.result_media_snapshot_id, None);
    assert_event_count(&cp, "artifact.audio_transcode_failed", 0).await;
    assert_event_count(&cp, "artifact.audio_transcode_succeeded", 0).await;
}

#[tokio::test]
async fn test_extract_post_commit_succeeded_event_failure_returns_ok_with_context() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let input = extract_input_for_source(&source, bundle.id, &dir);

    sqlx::query(
        "CREATE TRIGGER fail_extract_succeeded BEFORE INSERT ON events \
         WHEN NEW.kind = 'artifact.audio_extract_succeeded' \
         BEGIN SELECT RAISE(ABORT, 'injected post-commit event failure'); END;",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let report = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
    )
    .await
    .unwrap();

    assert!(report.commit_record_id.0 > 0);
    assert!(report.result_file_version_id.0 > 0);
    assert!(report.result_file_location_id.0 > 0);
    let recovery = report.commit_recovery_required.unwrap();
    assert_eq!(recovery.commit_record_id, report.commit_record_id);
    assert_eq!(
        recovery.result_file_version_id,
        Some(report.result_file_version_id)
    );
    assert_eq!(
        recovery.result_file_location_id,
        Some(report.result_file_location_id)
    );
    assert_eq!(
        recovery.recovery_reason,
        "audio extract post-commit reporting failed"
    );
    assert!(recovery.target_exists);
    assert_event_count(&cp, "artifact.audio_extract_failed", 0).await;
    assert_event_count(&cp, "artifact.audio_extract_succeeded", 0).await;
    assert_event_count(&cp, "artifact.commit_completed", 1).await;
}

async fn fixture() -> (crate::ControlPlane, tempfile::NamedTempFile) {
    let db = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(FrozenRng::new(1))),
    )
    .await
    .unwrap();
    (cp, db)
}

async fn fixture_with_dir() -> (
    crate::ControlPlane,
    tempfile::NamedTempFile,
    tempfile::TempDir,
) {
    let (cp, db) = fixture().await;
    (
        cp,
        db,
        tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap(),
    )
}

#[derive(Debug, Clone, Copy)]
struct SeededAudioSource {
    version: FileVersionId,
    location: FileLocationId,
    snapshot: u64,
}

async fn seed_audio_source(
    cp: &crate::ControlPlane,
    dir: &tempfile::TempDir,
    bytes: &[u8],
) -> SeededAudioSource {
    let source_path = dir.path().join("source.mkv");
    std::fs::write(&source_path, bytes).unwrap();
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: source_path.display().to_string(),
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
        panic!("seed_audio_source should create a new file asset");
    };
    let snapshot = cp
        .record_media_snapshot(
            file_version_id,
            None,
            serde_json::json!({
                "container": "mkv",
                "streams": [
                    {
                        "id": "v-1",
                        "index": 0,
                        "kind": "video",
                        "codec_name": "h264"
                    },
                    {
                        "id": "a-1",
                        "index": 1,
                        "kind": "audio",
                        "codec_name": "aac",
                        "language": "eng",
                        "title": "Main",
                        "channels": 2,
                        "disposition": {
                            "default": true,
                            "forced": false,
                            "commentary": false
                        }
                    }
                ]
            }),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    SeededAudioSource {
        version: file_version_id,
        location: file_location_id,
        snapshot: snapshot.id.0,
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

fn transcode_input() -> ExecuteTranscodeAudioInput {
    ExecuteTranscodeAudioInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: FileVersionId(999),
        source_location_id: None,
        operation_payload: serde_json::json!({
            "type": "transcode_audio",
            "target_codec": "aac",
            "container": "mkv",
            "source_media_snapshot_id": 888,
            "filter": null
        }),
        staging_root: PathBuf::from("/tmp/voom-audio-stage"),
        target_dir: PathBuf::from("/tmp/voom-audio-out"),
    }
}

fn transcode_input_for_source(
    source: &SeededAudioSource,
    dir: &tempfile::TempDir,
) -> ExecuteTranscodeAudioInput {
    ExecuteTranscodeAudioInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: source.version,
        source_location_id: Some(source.location),
        operation_payload: serde_json::json!({
            "type": "transcode_audio",
            "target_codec": "aac",
            "container": "mkv",
            "source_media_snapshot_id": source.snapshot,
            "filter": null
        }),
        staging_root: dir.path().join("voom-audio-stage"),
        target_dir: dir.path().join("voom-audio-out"),
    }
}

fn extract_input() -> ExecuteExtractAudioInput {
    ExecuteExtractAudioInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: FileVersionId(999),
        source_location_id: None,
        source_bundle_id: BundleId(777),
        operation_payload: serde_json::json!({
            "type": "extract_audio",
            "target_codec": "opus",
            "container": "ogg",
            "source_media_snapshot_id": 888,
            "filter": null
        }),
        staging_root: PathBuf::from("/tmp/voom-audio-stage"),
        target_dir: PathBuf::from("/tmp/voom-audio-out"),
    }
}

fn extract_input_for_source(
    source: &SeededAudioSource,
    source_bundle_id: BundleId,
    dir: &tempfile::TempDir,
) -> ExecuteExtractAudioInput {
    ExecuteExtractAudioInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: source.version,
        source_location_id: Some(source.location),
        source_bundle_id,
        operation_payload: serde_json::json!({
            "type": "extract_audio",
            "target_codec": "opus",
            "container": "ogg",
            "source_media_snapshot_id": source.snapshot,
            "snapshot_stream_id": "a-1",
            "filter": null
        }),
        staging_root: dir.path().join("voom-audio-stage"),
        target_dir: dir.path().join("voom-audio-out"),
    }
}

async fn assert_event_count(cp: &crate::ControlPlane, kind: &str, expected: i64) {
    let row = sqlx::query("SELECT COUNT(*) AS count FROM events WHERE kind = ?")
        .bind(kind)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let count: i64 = row.try_get("count").unwrap();
    assert_eq!(count, expected);
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

struct UncalledTranscodeDispatcher;

#[async_trait]
impl TranscodeAudioDispatcher for UncalledTranscodeDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        _request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        panic!("transcode dispatcher should not be called")
    }
}

struct UncalledExtractDispatcher;

#[async_trait]
impl ExtractAudioDispatcher for UncalledExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        _request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        panic!("extract dispatcher should not be called")
    }
}

struct UncalledVerifyDispatcher;

#[async_trait]
impl VerifyArtifactDispatcher for UncalledVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        _request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        panic!("verify dispatcher should not be called")
    }
}

struct MismatchedVerifyDispatcher;

#[async_trait]
impl VerifyArtifactDispatcher for MismatchedVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        _request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        Ok(VerifyArtifactResult {
            status: VerifyArtifactStatus::Verified,
            provider: "test-verify".to_owned(),
            provider_version: "test".to_owned(),
            observed: VerifyArtifactObservedFacts {
                size_bytes: 1,
                content_hash: "blake3:mismatch".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
        })
    }
}

struct SuccessfulVerifyDispatcher;

#[async_trait]
impl VerifyArtifactDispatcher for SuccessfulVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        Ok(VerifyArtifactResult {
            status: VerifyArtifactStatus::Verified,
            provider: "test-verify".to_owned(),
            provider_version: "test".to_owned(),
            observed: VerifyArtifactObservedFacts {
                size_bytes: request.expected.size_bytes,
                content_hash: request.expected.content_hash,
                modified_at: None,
                local_file_key: None,
            },
        })
    }
}

struct UncalledProbeDispatcher;

#[async_trait]
impl commit::AudioResultProbeDispatcher for UncalledProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        _cp: &crate::ControlPlane,
        _request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<commit::ProbedAudioResult, VoomError> {
        panic!("probe dispatcher should not be called")
    }
}

struct FailingProbeDispatcher;

#[async_trait]
impl commit::AudioResultProbeDispatcher for FailingProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        _cp: &crate::ControlPlane,
        _request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<commit::ProbedAudioResult, VoomError> {
        Err(VoomError::Internal("probe failed after commit".to_owned()))
    }
}

struct WritingTranscodeDispatcher {
    output_bytes: Vec<u8>,
}

#[async_trait]
impl TranscodeAudioDispatcher for WritingTranscodeDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        tokio::fs::write(&request.output.path, &self.output_bytes)
            .await
            .unwrap();
        let output_hash = blake3_checksum(&self.output_bytes);
        Ok(TranscodeAudioResult {
            status: voom_worker_protocol::TranscodeAudioStatus::Transcoded,
            provider: "ffmpeg".to_owned(),
            provider_version: "test".to_owned(),
            input_pre: observed(
                request.input.expected.size_bytes,
                &request.input.expected.content_hash,
            ),
            input_post: observed(
                request.input.expected.size_bytes,
                &request.input.expected.content_hash,
            ),
            output: observed(
                u64::try_from(self.output_bytes.len()).unwrap(),
                &output_hash,
            ),
            output_container: "mkv".to_owned(),
            selected_snapshot_stream_ids: vec!["a-1".to_owned()],
            output_audio_codecs: vec!["aac".to_owned()],
            selected_output_streams: vec![AudioOutputStreamFact {
                snapshot_stream_id: "a-1".to_owned(),
                output_provider_stream_index: 0,
                codec: "aac".to_owned(),
                language: Some("eng".to_owned()),
                title: Some("Main".to_owned()),
                default: Some(true),
                disposition: Some(voom_worker_protocol::AudioDispositionFact {
                    default: Some(true),
                    forced: Some(false),
                    commentary: Some(false),
                }),
                channels: Some(2),
            }],
        })
    }
}

struct WritingExtractDispatcher {
    output_bytes: Vec<u8>,
}

#[async_trait]
impl ExtractAudioDispatcher for WritingExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        tokio::fs::write(&request.output.path, &self.output_bytes)
            .await
            .unwrap();
        let output_hash = blake3_checksum(&self.output_bytes);
        Ok(ExtractAudioResult {
            status: voom_worker_protocol::ExtractAudioStatus::Extracted,
            provider: "ffmpeg".to_owned(),
            provider_version: "test".to_owned(),
            input_pre: observed(
                request.input.expected.size_bytes,
                &request.input.expected.content_hash,
            ),
            input_post: observed(
                request.input.expected.size_bytes,
                &request.input.expected.content_hash,
            ),
            output: observed(
                u64::try_from(self.output_bytes.len()).unwrap(),
                &output_hash,
            ),
            output_container: "ogg".to_owned(),
            output_audio_codec: "opus".to_owned(),
            selected_snapshot_stream_id: request.selection.snapshot_stream_id.clone(),
            output_language: Some("eng".to_owned()),
            output_title: Some("Main".to_owned()),
        })
    }
}

fn observed(size_bytes: u64, content_hash: &str) -> AudioObservedFacts {
    AudioObservedFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
