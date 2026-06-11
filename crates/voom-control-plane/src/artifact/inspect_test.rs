use super::*;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ErrorCode, FailureClass, FileLocationId, FileVersionId, VoomError,
    rng_test_support::FrozenRng,
};
use voom_store::repo::artifacts::{
    ArtifactCommitFailure, ArtifactCommitState, ArtifactVerificationStatus, NewArtifactCommitRecord,
};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_worker_protocol::{
    VerifyArtifactObservedFacts, VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

use crate::ControlPlane;
use crate::artifact::commit::{
    CommitArtifactHooks, CommitArtifactInput, CommitArtifactInstallContext,
    commit_artifact_with_hooks,
};
use crate::artifact::stage::{StageCopyInput, StageCopyReport};
use crate::artifact::verify::{
    NoVerifyArtifactHooks, VerifyArtifactDispatcher, VerifyArtifactInput,
    verify_artifact_with_dispatcher,
};

#[tokio::test]
async fn list_filters_by_state_from_durable_rows() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_bytes(&cp, dir.path(), b"staged bytes").await;
    let verified = stage_and_verify_bytes(&cp, dir.path(), b"verified bytes").await;
    let committed = stage_verify_and_commit_bytes(&cp, dir.path(), b"committed bytes").await;
    let failed = stage_verify_and_fail_commit_bytes(&cp, dir.path(), b"failed bytes").await;
    let recovery = stage_verify_and_recovery_commit_bytes(&cp, dir.path(), b"recovery bytes").await;

    assert_eq!(
        listed_ids(&cp, Some(ArtifactInspectionState::Staged)).await,
        vec![staged.artifact_handle_id]
    );
    assert_eq!(
        listed_ids(&cp, Some(ArtifactInspectionState::Verified)).await,
        vec![verified.artifact_handle_id]
    );
    assert_eq!(
        listed_ids(&cp, Some(ArtifactInspectionState::Committed)).await,
        vec![committed.artifact_handle_id]
    );
    assert_eq!(
        listed_ids(&cp, Some(ArtifactInspectionState::Failed)).await,
        vec![failed.artifact_handle_id]
    );
    assert_eq!(
        listed_ids(&cp, Some(ArtifactInspectionState::RecoveryRequired)).await,
        vec![recovery.artifact_handle_id]
    );
}

#[tokio::test]
async fn list_limit_returns_newest_artifacts_first() {
    let (cp, _db, dir) = fixture().await;
    let first = stage_bytes(&cp, dir.path(), b"first").await;
    let second = stage_bytes(&cp, dir.path(), b"second").await;
    let third = stage_bytes(&cp, dir.path(), b"third").await;

    let summaries = cp
        .list_artifacts(ArtifactListInput {
            state: None,
            limit: 2,
        })
        .await
        .unwrap();

    assert_eq!(
        summaries
            .iter()
            .map(|summary| summary.artifact_handle_id)
            .collect::<Vec<_>>(),
        vec![third.artifact_handle_id, second.artifact_handle_id]
    );
    assert!(
        !summaries
            .iter()
            .any(|summary| { summary.artifact_handle_id == first.artifact_handle_id })
    );
}

#[tokio::test]
async fn show_staged_artifact_reports_live_staging_facts() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_bytes(&cp, dir.path(), b"staged bytes").await;

    let detail = cp.show_artifact(staged.artifact_handle_id).await.unwrap();

    assert_eq!(detail.artifact_handle_id, staged.artifact_handle_id);
    assert_eq!(detail.state, ArtifactInspectionState::Staged);
    assert_eq!(
        detail.source_file_version_id,
        Some(staged.source_file_version_id)
    );
    assert_eq!(detail.staging_path, Some(staged.staging_path));
    assert_eq!(detail.target_path, None);
    assert_eq!(detail.size_bytes, Some(12));
    assert_eq!(detail.checksum, Some(blake3_checksum(b"staged bytes")));
    assert_eq!(detail.verifications, Vec::<VerificationSummary>::new());
    assert_eq!(detail.commits, Vec::<CommitSummary>::new());
}

#[tokio::test]
async fn show_verified_artifact_uses_latest_success_for_live_staging_without_commit_owner() {
    let (cp, _db, dir) = fixture().await;
    let verified = stage_and_verify_bytes(&cp, dir.path(), b"verified bytes").await;

    let detail = cp.show_artifact(verified.artifact_handle_id).await.unwrap();

    assert_eq!(detail.state, ArtifactInspectionState::Verified);
    assert_eq!(detail.staging_path, Some(verified.staging_path));
    assert_eq!(detail.verifications.len(), 1);
    assert_eq!(
        detail.latest_verification.as_ref().unwrap().status,
        ArtifactVerificationStatus::Succeeded
    );
    assert_eq!(
        detail
            .latest_verification
            .as_ref()
            .unwrap()
            .observed_checksum,
        Some(blake3_checksum(b"verified bytes"))
    );
    assert_eq!(detail.latest_commit, None);
}

#[tokio::test]
async fn show_failed_artifact_uses_latest_failure_for_live_staging_without_commit_owner() {
    let (cp, _db, dir) = fixture().await;
    let failed = stage_and_fail_verify_bytes(&cp, dir.path(), b"failed verify bytes").await;

    let detail = cp.show_artifact(failed.artifact_handle_id).await.unwrap();

    assert_eq!(detail.state, ArtifactInspectionState::Failed);
    assert_eq!(
        listed_ids(&cp, Some(ArtifactInspectionState::Failed)).await,
        vec![failed.artifact_handle_id]
    );
    assert_eq!(detail.staging_path, Some(failed.staging_path));
    assert_eq!(detail.verifications.len(), 1);
    let verification = detail.latest_verification.as_ref().unwrap();
    assert_eq!(verification.status, ArtifactVerificationStatus::Failed);
    assert_eq!(
        verification.failure_class.as_deref(),
        Some("artifact_checksum_mismatch")
    );
    assert_eq!(
        verification.error_code.as_deref(),
        Some(ErrorCode::ArtifactChecksumMismatch.as_str())
    );
    assert_eq!(detail.latest_commit, None);
}

#[tokio::test]
async fn show_committed_artifact_reports_commit_result_and_retired_staging() {
    let (cp, _db, dir) = fixture().await;
    let committed = stage_verify_and_commit_bytes(&cp, dir.path(), b"committed bytes").await;

    let detail = cp
        .show_artifact(committed.artifact_handle_id)
        .await
        .unwrap();

    assert_eq!(detail.state, ArtifactInspectionState::Committed);
    assert_eq!(detail.staging_path, None);
    assert_eq!(detail.target_path, Some(committed.target_path));
    let commit = detail.latest_commit.as_ref().unwrap();
    assert_eq!(commit.id, committed.commit_record_id);
    assert_eq!(commit.state, ArtifactCommitState::Committed);
    assert!(commit.result_file_version_id.is_some());
    assert!(commit.result_file_location_id.is_some());
    assert_eq!(commit.recovery, None);
}

#[tokio::test]
async fn show_failed_artifact_reports_failure_fields() {
    let (cp, _db, dir) = fixture().await;
    let failed = stage_verify_and_fail_commit_bytes(&cp, dir.path(), b"failed bytes").await;

    let detail = cp.show_artifact(failed.artifact_handle_id).await.unwrap();

    assert_eq!(detail.state, ArtifactInspectionState::Failed);
    let commit = detail.latest_commit.as_ref().unwrap();
    assert_eq!(commit.id, failed.commit_record_id);
    assert_eq!(commit.state, ArtifactCommitState::Failed);
    assert_eq!(commit.failure_class.as_deref(), Some("commit_failure"));
    assert_eq!(
        commit.error_code.as_deref(),
        Some(ErrorCode::CommitFailure.as_str())
    );
    assert_eq!(commit.message.as_deref(), Some("injected failed commit"));
}

#[tokio::test]
async fn show_recovery_required_artifact_reports_recovery_filesystem_facts() {
    let (cp, _db, dir) = fixture().await;
    let recovery = stage_verify_and_recovery_commit_bytes(&cp, dir.path(), b"recovery bytes").await;

    let detail = cp.show_artifact(recovery.artifact_handle_id).await.unwrap();

    assert_eq!(detail.state, ArtifactInspectionState::RecoveryRequired);
    let recovery = detail.latest_commit.unwrap().recovery.unwrap();
    assert_eq!(recovery.reason.as_deref(), Some("promotion_failed"));
    assert!(recovery.target.exists);
    assert!(recovery.temp.as_ref().unwrap().exists);
    assert!(recovery.staging.as_ref().unwrap().exists);
    assert_eq!(
        recovery.target.facts.as_ref().unwrap().checksum,
        blake3_checksum(b"concurrent writer")
    );
    assert_eq!(
        recovery
            .temp
            .as_ref()
            .unwrap()
            .facts
            .as_ref()
            .unwrap()
            .checksum,
        blake3_checksum(b"recovery bytes")
    );
    assert_eq!(
        recovery
            .staging
            .as_ref()
            .unwrap()
            .facts
            .as_ref()
            .unwrap()
            .checksum,
        blake3_checksum(b"recovery bytes")
    );
}

#[tokio::test]
async fn show_missing_artifact_returns_not_found() {
    let (cp, _db, _dir) = fixture().await;
    let err = cp.show_artifact(ArtifactHandleId(404)).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::NotFound);
}

#[derive(Debug, Clone)]
struct VerifiedStage {
    artifact_handle_id: ArtifactHandleId,
    source_file_version_id: FileVersionId,
    staging_path: PathBuf,
}

#[derive(Debug, Clone)]
struct CommitOutcome {
    artifact_handle_id: ArtifactHandleId,
    commit_record_id: ArtifactCommitRecordId,
    target_path: PathBuf,
}

async fn fixture() -> (ControlPlane, tempfile::NamedTempFile, tempfile::TempDir) {
    let db = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, db, artifact_tempdir())
}

fn artifact_tempdir() -> tempfile::TempDir {
    tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

async fn listed_ids(
    cp: &ControlPlane,
    state: Option<ArtifactInspectionState>,
) -> Vec<ArtifactHandleId> {
    cp.list_artifacts(ArtifactListInput { state, limit: 100 })
        .await
        .unwrap()
        .into_iter()
        .map(|summary| summary.artifact_handle_id)
        .collect()
}

async fn stage_bytes(cp: &ControlPlane, dir: &Path, bytes: &[u8]) -> StageCopyReport {
    let source = unique_path(dir, "source.bin");
    let staging = unique_path(dir, "staged.bin");
    std::fs::write(&source, bytes).unwrap();
    let seeded = seed_source(cp, &source, bytes).await;
    cp.stage_copy(StageCopyInput {
        file_version_id: seeded.file_version_id,
        source_location_id: Some(seeded.file_location_id),
        staging_path: staging,
    })
    .await
    .unwrap()
}

async fn stage_and_verify_bytes(cp: &ControlPlane, dir: &Path, bytes: &[u8]) -> VerifiedStage {
    let staged = stage_bytes(cp, dir, bytes).await;
    verify_artifact_with_dispatcher(
        cp,
        VerifyArtifactInput::for_staged_file(staged.artifact_handle_id, &staged.staging_path),
        &StaticDispatcher::success(bytes.to_vec()),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap();
    VerifiedStage {
        artifact_handle_id: staged.artifact_handle_id,
        source_file_version_id: staged.source_file_version_id,
        staging_path: staged.staging_path,
    }
}

async fn stage_and_fail_verify_bytes(cp: &ControlPlane, dir: &Path, bytes: &[u8]) -> VerifiedStage {
    let staged = stage_bytes(cp, dir, bytes).await;
    verify_artifact_with_dispatcher(
        cp,
        VerifyArtifactInput::for_staged_file(staged.artifact_handle_id, &staged.staging_path),
        &StaticDispatcher::failure(
            FailureClass::ArtifactChecksumMismatch,
            ErrorCode::ArtifactChecksumMismatch,
            "injected failed verification",
        ),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap();
    VerifiedStage {
        artifact_handle_id: staged.artifact_handle_id,
        source_file_version_id: staged.source_file_version_id,
        staging_path: staged.staging_path,
    }
}

async fn stage_verify_and_commit_bytes(
    cp: &ControlPlane,
    dir: &Path,
    bytes: &[u8],
) -> CommitOutcome {
    let staged = stage_and_verify_bytes(cp, dir, bytes).await;
    let target = unique_path(dir, "target.bin");
    let report = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target,
        })
        .await
        .unwrap();
    CommitOutcome {
        artifact_handle_id: staged.artifact_handle_id,
        commit_record_id: report.commit_record_id,
        target_path: report.target_path,
    }
}

async fn stage_verify_and_fail_commit_bytes(
    cp: &ControlPlane,
    dir: &Path,
    bytes: &[u8],
) -> CommitOutcome {
    let staged = stage_and_verify_bytes(cp, dir, bytes).await;
    let verification_id = latest_verification_id(cp, staged.artifact_handle_id).await;
    let target_path = unique_path(dir, "target.bin").display().to_string();
    let pending = create_pending_commit(cp, &staged, verification_id, &target_path).await;
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let failed = cp
        .artifacts()
        .mark_commit_failed_in_tx(
            &mut tx,
            pending.id,
            ArtifactCommitFailure {
                failure_class: "commit_failure".to_owned(),
                error_code: ErrorCode::CommitFailure.as_str().to_owned(),
                message: "injected failed commit".to_owned(),
                finished_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    CommitOutcome {
        artifact_handle_id: staged.artifact_handle_id,
        commit_record_id: failed.id,
        target_path: PathBuf::from(target_path),
    }
}

async fn stage_verify_and_recovery_commit_bytes(
    cp: &ControlPlane,
    dir: &Path,
    bytes: &[u8],
) -> CommitOutcome {
    let staged = stage_and_verify_bytes(cp, dir, bytes).await;
    let target = unique_path(dir, "target.bin");
    let err = commit_artifact_with_hooks(
        cp,
        CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target,
        },
        &CreateTargetBeforeInstall {
            bytes: b"concurrent writer",
        },
    )
    .await
    .unwrap_err();
    let report = err.commit_report().unwrap();
    CommitOutcome {
        artifact_handle_id: staged.artifact_handle_id,
        commit_record_id: report.commit_record_id,
        target_path: report.target_path.clone(),
    }
}

#[derive(Debug, Clone, Copy)]
struct SeededSource {
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
}

async fn seed_source(cp: &ControlPlane, path: &Path, bytes: &[u8]) -> SeededSource {
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

async fn latest_verification_id(
    cp: &ControlPlane,
    handle_id: ArtifactHandleId,
) -> ArtifactVerificationId {
    cp.artifacts()
        .list_verifications(handle_id)
        .await
        .unwrap()
        .last()
        .unwrap()
        .id
}

async fn create_pending_commit(
    cp: &ControlPlane,
    staged: &VerifiedStage,
    verification_id: ArtifactVerificationId,
    target_path: &str,
) -> voom_store::repo::artifacts::ArtifactCommitRecord {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let record = cp
        .artifacts()
        .create_pending_commit_in_tx(
            &mut tx,
            NewArtifactCommitRecord {
                artifact_handle_id: staged.artifact_handle_id,
                source_file_version_id: staged.source_file_version_id,
                verification_id,
                target_path: target_path.to_owned(),
                temp_path: Some(format!("{target_path}.tmp")),
                report: serde_json::json!({ "test": true }),
                started_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    record
}

fn unique_path(dir: &Path, file_name: &str) -> PathBuf {
    dir.join(format!(
        "{}-{file_name}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[derive(Debug)]
struct StaticDispatcher {
    outcome: StaticOutcome,
}

impl StaticDispatcher {
    fn success(bytes: Vec<u8>) -> Self {
        Self {
            outcome: StaticOutcome::Success(bytes),
        }
    }

    fn failure(class: FailureClass, code: ErrorCode, message: &'static str) -> Self {
        Self {
            outcome: StaticOutcome::Failure {
                class,
                code,
                message,
            },
        }
    }
}

#[derive(Debug)]
enum StaticOutcome {
    Success(Vec<u8>),
    Failure {
        class: FailureClass,
        code: ErrorCode,
        message: &'static str,
    },
}

#[async_trait]
impl VerifyArtifactDispatcher for StaticDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        _request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        match &self.outcome {
            StaticOutcome::Success(bytes) => Ok(VerifyArtifactResult {
                status: VerifyArtifactStatus::Verified,
                provider: "test-dispatcher".to_owned(),
                provider_version: "test".to_owned(),
                observed: VerifyArtifactObservedFacts {
                    size_bytes: u64::try_from(bytes.len()).unwrap(),
                    content_hash: blake3_checksum(bytes),
                    modified_at: None,
                    local_file_key: None,
                },
            }),
            StaticOutcome::Failure {
                class,
                code,
                message,
            } => Err(crate::artifact::worker::VerifyWorkerError::terminal_error(
                *class, *code, *message,
            )),
        }
    }
}

struct CreateTargetBeforeInstall {
    bytes: &'static [u8],
}

impl CommitArtifactHooks for CreateTargetBeforeInstall {
    fn before_install(&self, context: CommitArtifactInstallContext<'_>) -> Result<(), VoomError> {
        std::fs::write(context.target_path, self.bytes).unwrap();
        Ok(())
    }
}
