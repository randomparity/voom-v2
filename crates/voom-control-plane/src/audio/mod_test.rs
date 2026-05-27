use super::*;

use async_trait::async_trait;
use sqlx::Row;
use voom_core::ids::{ArtifactCommitRecordId, BundleId};
use voom_core::rng_test_support::FrozenRng;
use voom_core::{JobId, LeaseId, TicketId};
use voom_worker_protocol::{
    ExtractAudioRequest, ExtractAudioResult, TranscodeAudioRequest, TranscodeAudioResult,
    VerifyArtifactRequest, VerifyArtifactResult,
};

#[test]
fn extract_commit_recovery_required_is_not_reported_as_success() {
    let report = commit::CommitAudioExtractSidecarReport {
        commit_record_id: ArtifactCommitRecordId(9),
        result_file_version_id: FileVersionId(0),
        result_file_location_id: FileLocationId(0),
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
    assert!(err.to_string().contains("bundle membership conflict"));
}

#[test]
fn extract_commit_non_committed_state_is_not_reported_as_success() {
    let report = commit::CommitAudioExtractSidecarReport {
        commit_record_id: ArtifactCommitRecordId(10),
        result_file_version_id: FileVersionId(1),
        result_file_location_id: FileLocationId(2),
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

async fn assert_event_count(cp: &crate::ControlPlane, kind: &str, expected: i64) {
    let row = sqlx::query("SELECT COUNT(*) AS count FROM events WHERE kind = ?")
        .bind(kind)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let count: i64 = row.try_get("count").unwrap();
    assert_eq!(count, expected);
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
