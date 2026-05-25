use super::*;

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::OffsetDateTime;
use voom_core::{
    ArtifactHandleId, ErrorCode, FailureClass, FileLocationId, FileVersionId, WorkerId,
    rng_test_support::FrozenRng,
};
use voom_events::EventKind;
use voom_store::repo::artifacts::{ArtifactRepo, ArtifactVerificationStatus, NewArtifactLocation};
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::repo::workers::WorkerRepo;
use voom_worker_protocol::{
    VerifyArtifactObservedFacts, VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

use crate::ControlPlane;
use crate::artifact::stage::{StageCopyInput, StageCopyReport};

#[tokio::test]
async fn missing_artifact_handle_returns_not_found() {
    let (cp, _db, _dir) = fixture().await;
    let err = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: ArtifactHandleId(404),
        },
        &StaticDispatcher::success(b"unused"),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn verify_requires_exactly_one_live_staging_location() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let staged = stage_source(&cp, &source, &staging, b"source bytes").await;

    cp.artifacts()
        .retire_location(staged.artifact_location_id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    let zero_err = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::success(b"source bytes"),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap_err();
    assert_eq!(zero_err.error_code(), ErrorCode::ConfigInvalid);

    cp.artifacts()
        .record_location(NewArtifactLocation {
            artifact_handle_id: staged.artifact_handle_id,
            kind: "staging".to_owned(),
            value: staging.display().to_string(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    cp.artifacts()
        .record_location(NewArtifactLocation {
            artifact_handle_id: staged.artifact_handle_id,
            kind: "staging".to_owned(),
            value: dir.path().join("second.bin").display().to_string(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();

    let multiple_err = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::success(b"source bytes"),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap_err();
    assert_eq!(multiple_err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn missing_location_during_persist_returns_not_found() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let staged = stage_source(&cp, &source, &staging, b"source bytes").await;

    let err = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::success(b"source bytes"),
        &DeleteLocationBeforePersist {
            location_id: staged.artifact_location_id,
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn worker_success_persists_verification_with_bootstrapped_worker_id() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let staged = stage_source(&cp, &source, &staging, b"source bytes").await;
    let dispatcher = StaticDispatcher::success(b"source bytes");

    let report = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &dispatcher,
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap();

    assert_eq!(
        dispatcher.worker_ids.lock().unwrap().as_slice(),
        &[report.worker_id]
    );
    assert_eq!(report.artifact_handle_id, staged.artifact_handle_id);
    assert_eq!(report.artifact_location_id, staged.artifact_location_id);
    assert_eq!(report.status, ArtifactVerificationStatus::Succeeded);
    assert_eq!(report.path, staging.canonicalize().unwrap());
    assert_eq!(report.expected_size_bytes, 12);
    assert_eq!(report.expected_checksum, blake3_checksum(b"source bytes"));
    assert_eq!(report.observed_size_bytes, Some(12));
    assert_eq!(
        report.observed_checksum,
        Some(blake3_checksum(b"source bytes"))
    );
    assert_eq!(report.error_code, None);

    let worker = cp
        .workers()
        .get_by_name("builtin.verify_artifact")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(worker.id, report.worker_id);
    let verifications = cp
        .artifacts()
        .list_verifications(staged.artifact_handle_id)
        .await
        .unwrap();
    assert_eq!(verifications.len(), 1);
    assert_eq!(verifications[0].id, report.verification_id);
    assert_eq!(verifications[0].worker_id, worker.id);
}

#[tokio::test]
async fn worker_terminal_failure_persists_failed_verification() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let staged = stage_source(&cp, &source, &staging, b"source bytes").await;
    std::fs::write(&staging, b"changed bytes").unwrap();

    let report = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::failure(
            FailureClass::ArtifactChecksumMismatch,
            ErrorCode::ArtifactChecksumMismatch,
            "observed file facts differ from expected size/hash",
        ),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap();

    assert_eq!(report.status, ArtifactVerificationStatus::Failed);
    assert_eq!(report.error_code, Some(ErrorCode::ArtifactChecksumMismatch));
    assert_eq!(
        report.message.as_deref(),
        Some("observed file facts differ from expected size/hash")
    );
    assert_eq!(report.observed_size_bytes, None);
    assert_eq!(report.observed_checksum, None);

    let verifications = cp
        .artifacts()
        .list_verifications(staged.artifact_handle_id)
        .await
        .unwrap();
    assert_eq!(verifications.len(), 1);
    assert_eq!(verifications[0].status, ArtifactVerificationStatus::Failed);
    assert_eq!(
        verifications[0].failure_class.as_deref(),
        Some("artifact_checksum_mismatch")
    );
    assert_eq!(
        verifications[0].error_code.as_deref(),
        Some("ARTIFACT_CHECKSUM_MISMATCH")
    );
}

#[tokio::test]
async fn mismatched_worker_success_persists_failed_verification() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let staged = stage_source(&cp, &source, &staging, b"source bytes").await;

    let report = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::success(b"different bytes"),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap();

    assert_eq!(report.status, ArtifactVerificationStatus::Failed);
    assert_eq!(report.error_code, Some(ErrorCode::ArtifactChecksumMismatch));
    assert_eq!(
        report.message.as_deref(),
        Some("verified artifact facts differ from expected size/hash")
    );
    assert_eq!(report.observed_size_bytes, None);
    assert_eq!(report.observed_checksum, None);
}

#[tokio::test]
async fn malformed_worker_result_persists_failed_verification() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let staged = stage_source(&cp, &source, &staging, b"source bytes").await;

    let report = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::failure(
            FailureClass::MalformedWorkerResult,
            ErrorCode::MalformedWorkerResult,
            "verify_artifact result decode: missing field observed",
        ),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap();

    assert_eq!(report.status, ArtifactVerificationStatus::Failed);
    assert_eq!(report.error_code, Some(ErrorCode::MalformedWorkerResult));
    assert_eq!(count_verifications(&cp, staged.artifact_handle_id).await, 1);
}

#[tokio::test]
async fn retired_staging_location_before_persist_records_failed_verification() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let staged = stage_source(&cp, &source, &staging, b"source bytes").await;

    let report = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::success(b"source bytes"),
        &RetireLocationBeforePersist {
            location_id: staged.artifact_location_id,
        },
    )
    .await
    .unwrap();

    assert_eq!(report.status, ArtifactVerificationStatus::Failed);
    assert_eq!(report.error_code, Some(ErrorCode::ArtifactUnavailable));
    assert_eq!(count_verifications(&cp, staged.artifact_handle_id).await, 1);
}

#[tokio::test]
async fn second_staging_location_before_persist_records_failed_verification() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let staged = stage_source(&cp, &source, &staging, b"source bytes").await;
    let second = dir.path().join("second-staging.bin");

    let report = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::success(b"source bytes"),
        &RecordSecondStagingBeforePersist {
            path: second.display().to_string(),
        },
    )
    .await
    .unwrap();

    assert_eq!(report.status, ArtifactVerificationStatus::Failed);
    assert_eq!(report.error_code, Some(ErrorCode::ArtifactUnavailable));
    assert_eq!(count_verifications(&cp, staged.artifact_handle_id).await, 1);
}

#[tokio::test]
async fn verification_events_use_same_transaction_as_persisted_verification_rows() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let staged = stage_source(&cp, &source, &staging, b"source bytes").await;

    let err = verify_artifact_with_dispatcher(
        &cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        &StaticDispatcher::success(b"source bytes"),
        &FailBeforeTerminalEvent,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
    assert_eq!(count_verifications(&cp, staged.artifact_handle_id).await, 0);
    assert_eq!(
        count_events(&cp, EventKind::ArtifactVerificationSucceeded).await,
        0
    );
    assert_eq!(
        count_events(&cp, EventKind::ArtifactVerificationStarted).await,
        1
    );
}

#[derive(Debug, Clone)]
struct SeededSource {
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
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

async fn stage_source(
    cp: &ControlPlane,
    source: &Path,
    staging: &Path,
    bytes: &[u8],
) -> StageCopyReport {
    let seeded = seed_source(cp, source, bytes).await;
    cp.stage_copy(StageCopyInput {
        file_version_id: seeded.file_version_id,
        source_location_id: Some(seeded.file_location_id),
        staging_path: staging.to_path_buf(),
    })
    .await
    .unwrap()
}

async fn count_verifications(cp: &ControlPlane, handle_id: ArtifactHandleId) -> usize {
    cp.artifacts()
        .list_verifications(handle_id)
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
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[derive(Debug)]
struct StaticDispatcher {
    outcome: StaticOutcome,
    worker_ids: Arc<Mutex<Vec<WorkerId>>>,
}

impl StaticDispatcher {
    fn success(bytes: &'static [u8]) -> Self {
        Self {
            outcome: StaticOutcome::Success(bytes),
            worker_ids: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn failure(class: FailureClass, code: ErrorCode, message: &'static str) -> Self {
        Self {
            outcome: StaticOutcome::Failure {
                class,
                code,
                message,
            },
            worker_ids: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[derive(Debug)]
enum StaticOutcome {
    Success(&'static [u8]),
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
        worker_id: WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        let _ = request;
        self.worker_ids.lock().unwrap().push(worker_id);
        match self.outcome {
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
                class, code, message,
            )),
        }
    }
}

struct DeleteLocationBeforePersist {
    location_id: voom_core::ArtifactLocationId,
}

#[async_trait]
impl VerifyArtifactHooks for DeleteLocationBeforePersist {
    async fn before_persist(
        &self,
        cp: &ControlPlane,
        _context: VerifyArtifactPersistContext<'_>,
    ) -> Result<(), VoomError> {
        sqlx::query("DELETE FROM artifact_locations WHERE id = ?")
            .bind(i64::try_from(self.location_id.0).unwrap())
            .execute(cp.pool_for_test())
            .await
            .unwrap();
        Ok(())
    }
}

struct RetireLocationBeforePersist {
    location_id: voom_core::ArtifactLocationId,
}

#[async_trait]
impl VerifyArtifactHooks for RetireLocationBeforePersist {
    async fn before_persist(
        &self,
        cp: &ControlPlane,
        _context: VerifyArtifactPersistContext<'_>,
    ) -> Result<(), VoomError> {
        cp.artifacts()
            .retire_location(self.location_id, OffsetDateTime::UNIX_EPOCH)
            .await?;
        Ok(())
    }
}

struct RecordSecondStagingBeforePersist {
    path: String,
}

#[async_trait]
impl VerifyArtifactHooks for RecordSecondStagingBeforePersist {
    async fn before_persist(
        &self,
        cp: &ControlPlane,
        context: VerifyArtifactPersistContext<'_>,
    ) -> Result<(), VoomError> {
        cp.artifacts()
            .record_location(NewArtifactLocation {
                artifact_handle_id: context.artifact_handle_id,
                kind: "staging".to_owned(),
                value: self.path.clone(),
                observed_at: OffsetDateTime::UNIX_EPOCH,
            })
            .await?;
        Ok(())
    }
}

struct FailBeforeTerminalEvent;

#[async_trait]
impl VerifyArtifactHooks for FailBeforeTerminalEvent {
    async fn before_terminal_event(
        &self,
        _context: VerifyArtifactPersistContext<'_>,
    ) -> Result<(), VoomError> {
        Err(VoomError::Database(
            "injected verification event failure".to_owned(),
        ))
    }
}
