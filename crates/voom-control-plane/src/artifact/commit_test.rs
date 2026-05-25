use super::*;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ErrorCode, FileLocationId, FileVersionId, VoomError,
    rng_test_support::FrozenRng,
};
use voom_events::EventKind;
use voom_store::repo::artifacts::{
    ArtifactCommitFailure, ArtifactCommitState, ArtifactRepo, NewArtifactCommitRecord,
    NewArtifactLocation,
};
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, NewFileLocation, NewFileVersion,
    ProducedBy,
};

use crate::ControlPlane;
use crate::artifact::stage::{StageCopyInput, StageCopyReport};
use crate::artifact::verify::{
    NoVerifyArtifactHooks, VerifyArtifactDispatcher, VerifyArtifactInput,
    verify_artifact_with_dispatcher,
};
use voom_worker_protocol::{
    VerifyArtifactObservedFacts, VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

#[tokio::test]
async fn unverified_commit_is_rejected_before_pending_record() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert_eq!(err.pre_mutation_report().unwrap().verification_id, None);
    assert_no_commit_records(&cp, staged.artifact_handle_id).await;
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitFailedPreMutation).await,
        1
    );
    assert!(!target.exists());
}

#[tokio::test]
async fn stale_verification_for_retired_or_different_staging_location_is_rejected() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    cp.artifacts()
        .retire_location(staged.artifact_location_id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    let replacement_staging = dir.path().join("replacement-staged.bin");
    std::fs::write(&replacement_staging, b"source bytes").unwrap();
    cp.artifacts()
        .record_location(NewArtifactLocation {
            artifact_handle_id: staged.artifact_handle_id,
            kind: "staging".to_owned(),
            value: replacement_staging.display().to_string(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: dir.path().join("target.bin"),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert_no_commit_records(&cp, staged.artifact_handle_id).await;
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitFailedPreMutation).await,
        1
    );
}

#[tokio::test]
async fn staged_byte_drift_before_prepare_is_rejected_before_pending_record() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    std::fs::write(&staged.staging_path, b"changed bytes").unwrap();

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: dir.path().join("target.bin"),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ArtifactChecksumMismatch);
    assert_no_commit_records(&cp, staged.artifact_handle_id).await;
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitFailedPreMutation).await,
        1
    );
}

#[tokio::test]
async fn existing_target_is_rejected_before_pending_record() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    std::fs::write(&target, b"already here").unwrap();

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert_no_commit_records(&cp, staged.artifact_handle_id).await;
    assert_eq!(std::fs::read(&target).unwrap(), b"already here");
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitFailedPreMutation).await,
        1
    );
}

#[tokio::test]
async fn target_created_after_prepare_is_not_overwritten_and_requires_recovery() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let err = commit_artifact_with_hooks(
        &cp,
        CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        },
        &CreateTargetBeforeInstall {
            bytes: b"concurrent writer",
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), ErrorCode::CommitFailure);
    assert_eq!(std::fs::read(&target).unwrap(), b"concurrent writer");
    let report = err.commit_report().unwrap();
    assert_eq!(report.state, ArtifactCommitState::RecoveryRequired);
    let recovery = report.recovery_required.as_ref().unwrap();
    assert!(recovery.target_exists);
    assert!(recovery.temp_exists);
    assert!(recovery.staging_exists);
    assert_eq!(
        count_commit_records(&cp, staged.artifact_handle_id).await,
        1
    );
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitRecoveryRequired).await,
        1
    );
}

#[tokio::test]
async fn successful_commit_promotes_target_records_identity_retires_staging_and_emits_events() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let report = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        })
        .await
        .unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"source bytes");
    assert_eq!(report.artifact_handle_id, staged.artifact_handle_id);
    assert_eq!(report.state, ArtifactCommitState::Committed);
    assert_eq!(report.target_path, target.canonicalize().unwrap());
    assert_eq!(report.recovery_required, None);
    let result_version_id = report.result_file_version_id.unwrap();
    let result_location_id = report.result_file_location_id.unwrap();

    let version = cp
        .identity()
        .get_file_version(result_version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.produced_by, ProducedBy::StagedCommit);
    assert_eq!(
        version.produced_from_version_id,
        Some(staged.source_file_version_id)
    );
    assert_eq!(version.content_hash, blake3_checksum(b"source bytes"));
    assert_eq!(version.size_bytes, 12);

    let location = cp
        .identity()
        .get_file_location(result_location_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(location.file_version_id, result_version_id);
    assert_eq!(location.kind, FileLocationKind::LocalPath);
    assert_eq!(
        location.value,
        target.canonicalize().unwrap().display().to_string()
    );

    let locations = cp
        .artifacts()
        .list_locations_for_handle(staged.artifact_handle_id)
        .await
        .unwrap();
    assert_eq!(locations.len(), 0);
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM artifact_locations WHERE id = ?")
            .bind(i64::try_from(staged.artifact_location_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert!(retired_at.is_some());

    let records = cp
        .artifacts()
        .list_commit_records(staged.artifact_handle_id)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, report.commit_record_id);
    assert_eq!(records[0].state, ArtifactCommitState::Committed);
    assert_eq!(records[0].result_file_version_id, Some(result_version_id));
    assert_eq!(records[0].result_file_location_id, Some(result_location_id));

    assert_eq!(count_events(&cp, EventKind::ArtifactCommitStarted).await, 1);
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitCompleted).await,
        1
    );
}

#[tokio::test]
async fn injected_failure_after_prepare_marks_recovery_required() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let err = commit_artifact_with_hooks(
        &cp,
        CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        },
        &FailAfterPrepare,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), ErrorCode::CommitFailure);
    assert!(!target.exists());
    let report = err.commit_report().unwrap();
    assert_eq!(report.state, ArtifactCommitState::RecoveryRequired);
    let recovery = report.recovery_required.as_ref().unwrap();
    assert!(!recovery.target_exists);
    assert_eq!(recovery.temp_path, None);
    assert!(recovery.staging_exists);
}

#[tokio::test]
async fn staged_drift_after_prepare_before_copy_marks_recovery_without_temp_file() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let err = commit_artifact_with_hooks(
        &cp,
        CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        },
        &DriftStagingBeforeTempCopy,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), ErrorCode::CommitFailure);
    assert!(!target.exists());
    let report = err.commit_report().unwrap();
    assert_eq!(report.state, ArtifactCommitState::RecoveryRequired);
    let recovery = report.recovery_required.as_ref().unwrap();
    assert!(!recovery.target_exists);
    assert!(!recovery.temp_exists);
    assert!(recovery.staging_exists);
    let temp_path = report.temp_path.as_ref().unwrap();
    assert!(!temp_path.exists());
}

#[tokio::test]
async fn failure_after_target_install_before_finalize_keeps_target_visible_and_marks_recovery() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let err = commit_artifact_with_hooks(
        &cp,
        CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        },
        &FailBeforeFinalize,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), ErrorCode::DbUnreachable);
    assert_eq!(std::fs::read(&target).unwrap(), b"source bytes");
    let report = err.commit_report().unwrap();
    assert_eq!(report.result_file_version_id, None);
    assert_eq!(report.result_file_location_id, None);
    let recovery = report.recovery_required.as_ref().unwrap();
    assert_eq!(recovery.target_path, target.canonicalize().unwrap());
    assert!(recovery.target_exists);
    assert!(!recovery.temp_exists);
    assert!(recovery.staging_exists);
}

#[tokio::test]
async fn duplicate_pending_committed_and_recovery_owners_are_rejected_by_repo_constraints() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let verification_id = cp
        .artifacts()
        .list_verifications(staged.artifact_handle_id)
        .await
        .unwrap()[0]
        .id;
    let target_a = dir.path().join("target-a.bin").display().to_string();
    let target_b = dir.path().join("target-b.bin").display().to_string();

    let pending = create_pending_commit(&cp, &staged, verification_id, &target_a).await;
    let duplicate_pending = create_pending_commit_result(&cp, &staged, verification_id, &target_b)
        .await
        .unwrap_err();
    assert_eq!(duplicate_pending.error_code(), ErrorCode::Conflict);

    mark_pending_committed(&cp, pending.id, &staged, &target_a).await;
    let duplicate_committed =
        create_pending_commit_result(&cp, &staged, verification_id, &target_b)
            .await
            .unwrap_err();
    assert_eq!(duplicate_committed.error_code(), ErrorCode::Conflict);

    let second = stage_and_verify_bytes(&cp, dir.path(), b"second bytes").await;
    let second_verification_id = cp
        .artifacts()
        .list_verifications(second.artifact_handle_id)
        .await
        .unwrap()[0]
        .id;
    let recovery = create_pending_commit(
        &cp,
        &second,
        second_verification_id,
        &dir.path().join("target-c.bin").display().to_string(),
    )
    .await;
    mark_pending_recovery(&cp, recovery.id).await;
    let duplicate_recovery = create_pending_commit_result(
        &cp,
        &second,
        second_verification_id,
        &dir.path().join("target-d.bin").display().to_string(),
    )
    .await
    .unwrap_err();
    assert_eq!(duplicate_recovery.error_code(), ErrorCode::Conflict);
}

#[derive(Debug, Clone)]
struct VerifiedStage {
    artifact_handle_id: ArtifactHandleId,
    artifact_location_id: voom_core::ArtifactLocationId,
    source_file_version_id: FileVersionId,
    staging_path: PathBuf,
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
    (cp, db, tempfile::tempdir().unwrap())
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
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::success(bytes.to_vec()),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap();
    VerifiedStage {
        artifact_handle_id: staged.artifact_handle_id,
        artifact_location_id: staged.artifact_location_id,
        source_file_version_id: staged.source_file_version_id,
        staging_path: staged.staging_path,
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

async fn create_pending_commit(
    cp: &ControlPlane,
    staged: &VerifiedStage,
    verification_id: ArtifactVerificationId,
    target_path: &str,
) -> voom_store::repo::artifacts::ArtifactCommitRecord {
    create_pending_commit_result(cp, staged, verification_id, target_path)
        .await
        .unwrap()
}

async fn create_pending_commit_result(
    cp: &ControlPlane,
    staged: &VerifiedStage,
    verification_id: ArtifactVerificationId,
    target_path: &str,
) -> Result<voom_store::repo::artifacts::ArtifactCommitRecord, VoomError> {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let result = cp
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
        .await;
    match result {
        Ok(record) => {
            tx.commit().await.unwrap();
            Ok(record)
        }
        Err(err) => Err(err),
    }
}

async fn mark_pending_committed(
    cp: &ControlPlane,
    commit_id: ArtifactCommitRecordId,
    staged: &VerifiedStage,
    target_path: &str,
) {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let source = cp
        .identity()
        .get_file_version_in_tx(&mut tx, staged.source_file_version_id)
        .await
        .unwrap()
        .unwrap();
    let version = cp
        .identity()
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: source.file_asset_id,
                content_hash: blake3_checksum(b"source bytes"),
                size_bytes: 12,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: Some(source.id),
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    let location = cp
        .identity()
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version.id,
                kind: FileLocationKind::LocalPath,
                value: target_path.to_owned(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    cp.artifacts()
        .mark_commit_committed_in_tx(
            &mut tx,
            commit_id,
            version.id,
            location.id,
            OffsetDateTime::UNIX_EPOCH,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn mark_pending_recovery(cp: &ControlPlane, commit_id: ArtifactCommitRecordId) {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    cp.artifacts()
        .mark_commit_recovery_required_in_tx(
            &mut tx,
            commit_id,
            ArtifactCommitFailure {
                failure_class: "commit_failure".to_owned(),
                error_code: ErrorCode::CommitFailure.as_str().to_owned(),
                message: "injected".to_owned(),
                finished_at: OffsetDateTime::UNIX_EPOCH,
            },
            "injected".to_owned(),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn assert_no_commit_records(cp: &ControlPlane, handle_id: ArtifactHandleId) {
    assert_eq!(count_commit_records(cp, handle_id).await, 0);
}

async fn count_commit_records(cp: &ControlPlane, handle_id: ArtifactHandleId) -> usize {
    cp.artifacts()
        .list_commit_records(handle_id)
        .await
        .unwrap()
        .len()
}

async fn count_events(cp: &ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 20,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
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
    bytes: Vec<u8>,
}

impl StaticDispatcher {
    fn success(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

#[async_trait]
impl VerifyArtifactDispatcher for StaticDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        _request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        Ok(VerifyArtifactResult {
            status: VerifyArtifactStatus::Verified,
            provider: "test-dispatcher".to_owned(),
            provider_version: "test".to_owned(),
            observed: VerifyArtifactObservedFacts {
                size_bytes: u64::try_from(self.bytes.len()).unwrap(),
                content_hash: blake3_checksum(&self.bytes),
                modified_at: None,
                local_file_key: None,
            },
        })
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

struct FailAfterPrepare;

impl CommitArtifactHooks for FailAfterPrepare {
    fn after_prepare(&self, _context: CommitArtifactPreparedContext<'_>) -> Result<(), VoomError> {
        Err(VoomError::CommitFailure(
            "injected failure after durable prepare".to_owned(),
        ))
    }
}

struct DriftStagingBeforeTempCopy;

impl CommitArtifactHooks for DriftStagingBeforeTempCopy {
    fn before_temp_copy(
        &self,
        context: CommitArtifactPreparedContext<'_>,
    ) -> Result<(), VoomError> {
        std::fs::write(context.staging_path, b"changed bytes").unwrap();
        Ok(())
    }
}

struct FailBeforeFinalize;

impl CommitArtifactHooks for FailBeforeFinalize {
    fn before_finalize(
        &self,
        _context: CommitArtifactFinalizeContext<'_>,
    ) -> Result<(), VoomError> {
        Err(VoomError::Database(
            "injected finalize transaction failure".to_owned(),
        ))
    }
}
