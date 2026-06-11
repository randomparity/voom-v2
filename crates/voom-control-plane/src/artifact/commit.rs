use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::json;
use sqlx::Row;
use tokio::fs;
use voom_artifact::commit_pipeline::{
    PendingCommitRecordError, RecoveryRequiredCommit, append_commit_event_in_tx,
    create_pending_commit_with_started_event_in_tx, mark_recovery_required_with_event_in_tx,
};
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, ErrorCode, FailureClass, FileAssetId, FileLocationId,
    FileVersionId, VoomError,
};
use voom_events::Event;
use voom_events::payload::{
    ArtifactCommitCompletedPayload, ArtifactCommitFailedPreMutationPayload,
    ArtifactCommitRecoveryRequiredPayload, ArtifactCommitStartedPayload,
};
use voom_store::repo::artifacts::{
    ArtifactCommitFailure, ArtifactCommitRecord, ArtifactCommitState, ArtifactVerification,
    NewArtifactCommitRecord,
};
use voom_store::repo::identity::{
    FileLocationKind, IdentityRepo, NewFileLocation, NewFileVersion, ProducedBy,
};

use crate::ControlPlane;
use crate::artifact::fs::{
    ArtifactFileFacts, canonical_new_leaf_no_symlink, copy_regular_file_checked,
    observe_regular_file, unique_temp_sibling_path,
};
use crate::cases::{begin_tx, commit_tx};

#[derive(Debug)]
pub struct CommitArtifactInput {
    pub artifact_handle_id: ArtifactHandleId,
    pub target_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitArtifactReport {
    pub commit_record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub verification_id: ArtifactVerificationId,
    pub target_path: PathBuf,
    pub temp_path: Option<PathBuf>,
    pub state: ArtifactCommitState,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
    pub recovery_required: Option<CommitRecoveryReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRecoveryReport {
    pub recovery_reason: String,
    pub target_path: PathBuf,
    pub target_exists: bool,
    pub temp_path: Option<PathBuf>,
    pub temp_exists: bool,
    pub staging_path: PathBuf,
    pub staging_exists: bool,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitArtifactPreMutationReport {
    pub artifact_handle_id: ArtifactHandleId,
    pub verification_id: Option<ArtifactVerificationId>,
    pub target_path: PathBuf,
    pub error_code: ErrorCode,
    pub message: String,
}

#[derive(Debug)]
pub struct CommitArtifactCommandError {
    code: ErrorCode,
    message: String,
    pre_mutation_report: Option<CommitArtifactPreMutationReport>,
    commit_report: Option<CommitArtifactReport>,
}

impl CommitArtifactCommandError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    #[must_use]
    pub const fn pre_mutation_report(&self) -> Option<&CommitArtifactPreMutationReport> {
        self.pre_mutation_report.as_ref()
    }

    #[must_use]
    pub const fn commit_report(&self) -> Option<&CommitArtifactReport> {
        self.commit_report.as_ref()
    }

    fn pre_mutation(report: CommitArtifactPreMutationReport) -> Self {
        Self {
            code: report.error_code,
            message: report.message.clone(),
            pre_mutation_report: Some(report),
            commit_report: None,
        }
    }

    fn committed_error(err: &VoomError, report: CommitArtifactReport) -> Self {
        Self {
            code: err.error_code(),
            message: err.to_string(),
            pre_mutation_report: None,
            commit_report: Some(report),
        }
    }
}

impl From<VoomError> for CommitArtifactCommandError {
    fn from(value: VoomError) -> Self {
        Self {
            code: value.error_code(),
            message: value.to_string(),
            pre_mutation_report: None,
            commit_report: None,
        }
    }
}

impl Display for CommitArtifactCommandError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for CommitArtifactCommandError {}

impl ControlPlane {
    /// Commit a verified staged artifact to a new local target path without
    /// replacing any existing target bytes.
    ///
    /// # Errors
    /// Returns `Config`/`ArtifactChecksumMismatch` before durable prepare when
    /// commit preconditions fail. Once a pending record is prepared, promotion
    /// or finalize failures transition that row to `recovery_required` and are
    /// returned as command errors carrying a recovery report.
    pub async fn commit_artifact(
        &self,
        input: CommitArtifactInput,
    ) -> Result<CommitArtifactReport, CommitArtifactCommandError> {
        commit_artifact_with_hooks(self, input, &NoCommitArtifactHooks).await
    }

    /// Re-drive a commit left in `recovery_required` back to completion.
    ///
    /// A fresh `commit_artifact` cannot recover such an artifact: the
    /// one-owner-per-artifact index reserves the slot for the stuck record. This
    /// resumes that existing record from the still-verified staging artifact. If
    /// the target was already installed (the original attempt failed at or after
    /// finalize) it re-runs finalize only; otherwise it re-promotes. A target
    /// that already exists with mismatched facts is a hard conflict and the
    /// record stays `recovery_required`.
    ///
    /// # Errors
    /// `Conflict` if the artifact has no `recovery_required` commit or the target
    /// exists with the wrong facts; `NotFound`/`Config`/`Database` for missing
    /// inputs or durable failures.
    pub async fn recover_commit(
        &self,
        artifact_handle_id: ArtifactHandleId,
    ) -> Result<CommitArtifactReport, VoomError> {
        recover_commit_inner(self, artifact_handle_id).await
    }
}

fn recovery_read_error(err: PrepareCommitError) -> VoomError {
    match err {
        PrepareCommitError::AfterPending(err) => err,
        PrepareCommitError::PreMutation(report) => {
            VoomError::CommitFailure(format!("commit recovery cannot re-read inputs: {report:?}"))
        }
    }
}

async fn recover_commit_inner(
    cp: &ControlPlane,
    artifact_handle_id: ArtifactHandleId,
) -> Result<CommitArtifactReport, VoomError> {
    let record = cp
        .artifacts
        .list_commit_records(artifact_handle_id)
        .await?
        .into_iter()
        .find(|record| record.state == ArtifactCommitState::RecoveryRequired)
        .ok_or_else(|| {
            VoomError::Conflict(format!(
                "artifact_handle {artifact_handle_id} has no commit in recovery_required state"
            ))
        })?;
    let target_path = PathBuf::from(&record.target_path);

    // Re-read the same inputs the initial prepare used; the staging artifact is
    // still live because finalize (which retires it) never completed.
    let context = PreMutationContext {
        artifact_handle_id,
        verification_id: Some(record.verification_id),
        target_path: target_path.clone(),
    };
    let mut tx = begin_tx(&cp.pool).await?;
    let source = read_commit_source_facts(cp, &mut tx, artifact_handle_id, &context)
        .await
        .map_err(recovery_read_error)?;
    let verified_staging =
        read_verified_staging_facts(cp, &mut tx, artifact_handle_id, &target_path, &context)
            .await
            .map_err(recovery_read_error)?;
    commit_tx(tx).await?;

    let staging_path = PathBuf::from(&verified_staging.staging.value);
    let expected_facts = observe_regular_file(&staging_path).await?;
    require_expected_facts(
        &source.handle,
        &verified_staging.verification,
        &expected_facts,
    )?;

    // Decide where to resume from based on the target's current state.
    let existing_target = observe_regular_file(&target_path).await.ok();
    let already_installed = match &existing_target {
        Some(facts) if same_file_facts(facts, &expected_facts) => true,
        Some(_) => {
            return Err(VoomError::Conflict(format!(
                "commit recovery: target {} exists with mismatched facts",
                target_path.display()
            )));
        }
        None => false,
    };
    let canonical_target = if already_installed {
        fs::canonicalize(&target_path).await.map_err(|err| {
            VoomError::CommitFailure(format!(
                "commit recovery cannot canonicalize installed target {}: {err}",
                target_path.display()
            ))
        })?
    } else {
        canonical_new_leaf_no_symlink(&target_path).await?
    };
    let temp_path = unique_temp_sibling_path(&canonical_target)?;

    let prepared = PreparedCommit {
        record,
        artifact_handle_id,
        source_file_version_id: source.source_file_version_id,
        source_file_asset_id: source.source_file_asset_id,
        staging_location_id: verified_staging.staging.id,
        staging_path,
        target_path: canonical_target,
        temp_path,
        expected_facts: expected_facts.clone(),
        promotion_started_at: cp.clock().now(),
    };

    let promotion = if already_installed {
        PromotionOutcome {
            target_facts: existing_target.unwrap_or(expected_facts),
        }
    } else {
        promote_prepared(cp, &prepared, &NoCommitArtifactHooks).await?
    };

    finalize_commit(cp, &prepared, &promotion).await
}

#[derive(Debug, Clone, Copy)]
#[expect(
    dead_code,
    reason = "test-only commit hooks inspect whichever context fields their failure mode needs"
)]
pub(crate) struct CommitArtifactPreparedContext<'a> {
    pub commit_record_id: ArtifactCommitRecordId,
    pub target_path: &'a Path,
    pub temp_path: &'a Path,
    pub staging_path: &'a Path,
}

#[derive(Debug, Clone, Copy)]
#[expect(
    dead_code,
    reason = "test-only commit hooks inspect whichever context fields their failure mode needs"
)]
pub(crate) struct CommitArtifactInstallContext<'a> {
    pub commit_record_id: ArtifactCommitRecordId,
    pub target_path: &'a Path,
    pub temp_path: &'a Path,
}

#[derive(Debug, Clone, Copy)]
#[expect(
    dead_code,
    reason = "test-only commit hooks inspect whichever context fields their failure mode needs"
)]
pub(crate) struct CommitArtifactFinalizeContext<'a> {
    pub commit_record_id: ArtifactCommitRecordId,
    pub target_path: &'a Path,
    pub temp_path: &'a Path,
    pub staging_path: &'a Path,
}

pub(crate) trait CommitArtifactHooks: Send + Sync {
    fn after_prepare(&self, _context: CommitArtifactPreparedContext<'_>) -> Result<(), VoomError> {
        Ok(())
    }

    fn before_temp_copy(
        &self,
        _context: CommitArtifactPreparedContext<'_>,
    ) -> Result<(), VoomError> {
        Ok(())
    }

    fn before_install(&self, _context: CommitArtifactInstallContext<'_>) -> Result<(), VoomError> {
        Ok(())
    }

    fn before_finalize(
        &self,
        _context: CommitArtifactFinalizeContext<'_>,
    ) -> Result<(), VoomError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct NoCommitArtifactHooks;

impl CommitArtifactHooks for NoCommitArtifactHooks {}

pub(crate) async fn commit_artifact_with_hooks(
    cp: &ControlPlane,
    input: CommitArtifactInput,
    hooks: &dyn CommitArtifactHooks,
) -> Result<CommitArtifactReport, CommitArtifactCommandError> {
    let prepared = prepare_commit(cp, input).await?;
    if let Err(err) = hooks.after_prepare(CommitArtifactPreparedContext {
        commit_record_id: prepared.record.id,
        target_path: &prepared.target_path,
        temp_path: &prepared.temp_path,
        staging_path: &prepared.staging_path,
    }) {
        let report = transition_recovery(cp, &prepared, err).await?;
        return Err(CommitArtifactCommandError::committed_error(
            &VoomError::CommitFailure("commit failed after durable prepare".to_owned()),
            report,
        ));
    }

    let promotion = match promote_prepared(cp, &prepared, hooks).await {
        Ok(promotion) => promotion,
        Err(err) => {
            let report = transition_recovery(cp, &prepared, err).await?;
            return Err(CommitArtifactCommandError::committed_error(
                &VoomError::CommitFailure("commit promotion requires recovery".to_owned()),
                report,
            ));
        }
    };

    if let Err(err) = hooks.before_finalize(CommitArtifactFinalizeContext {
        commit_record_id: prepared.record.id,
        target_path: &prepared.target_path,
        temp_path: &prepared.temp_path,
        staging_path: &prepared.staging_path,
    }) {
        let report = transition_recovery(cp, &prepared, err).await?;
        return Err(CommitArtifactCommandError::committed_error(
            &VoomError::database("commit finalize requires recovery"),
            report,
        ));
    }

    match finalize_commit(cp, &prepared, &promotion).await {
        Ok(report) => Ok(report),
        Err(err) => {
            let code = err.error_code();
            let report = transition_recovery(cp, &prepared, err).await?;
            Err(CommitArtifactCommandError {
                code,
                message: "commit finalize requires recovery".to_owned(),
                pre_mutation_report: None,
                commit_report: Some(report),
            })
        }
    }
}

#[derive(Debug)]
struct PreparedCommit {
    record: ArtifactCommitRecord,
    artifact_handle_id: ArtifactHandleId,
    source_file_version_id: FileVersionId,
    source_file_asset_id: voom_core::FileAssetId,
    staging_location_id: ArtifactLocationId,
    staging_path: PathBuf,
    target_path: PathBuf,
    temp_path: PathBuf,
    expected_facts: ArtifactFileFacts,
    promotion_started_at: time::OffsetDateTime,
}

#[derive(Debug)]
struct PromotionOutcome {
    target_facts: ArtifactFileFacts,
}

async fn prepare_commit(
    cp: &ControlPlane,
    input: CommitArtifactInput,
) -> Result<PreparedCommit, CommitArtifactCommandError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let prepared_result = prepare_commit_in_tx(cp, &mut tx, input, now).await;
    match prepared_result {
        Ok(prepared) => {
            commit_tx(tx).await?;
            Ok(prepared)
        }
        Err(PrepareCommitError::PreMutation(failure)) => {
            append_failed_pre_mutation(cp, &mut tx, &failure, now).await?;
            commit_tx(tx).await?;
            Err(CommitArtifactCommandError::pre_mutation(failure))
        }
        Err(PrepareCommitError::AfterPending(err)) => Err(err.into()),
    }
}

#[derive(Debug)]
enum PrepareCommitError {
    PreMutation(CommitArtifactPreMutationReport),
    AfterPending(VoomError),
}

async fn prepare_commit_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: CommitArtifactInput,
    now: time::OffsetDateTime,
) -> Result<PreparedCommit, PrepareCommitError> {
    let context = PreMutationContext {
        artifact_handle_id: input.artifact_handle_id,
        verification_id: None,
        target_path: input.target_path.clone(),
    };
    let source = read_commit_source_facts(cp, tx, input.artifact_handle_id, &context).await?;
    let verified_staging = read_verified_staging_facts(
        cp,
        tx,
        input.artifact_handle_id,
        &input.target_path,
        &context,
    )
    .await?;
    let paths = prepare_commit_paths(&input.target_path, &source.handle, &verified_staging).await?;

    let target_path_string = paths.target_path.display().to_string();
    let temp_path_string = paths.temp_path.display().to_string();
    let pending_input = NewArtifactCommitRecord {
        artifact_handle_id: input.artifact_handle_id,
        source_file_version_id: source.source_file_version_id,
        verification_id: verified_staging.verification.id,
        target_path: target_path_string.clone(),
        temp_path: Some(temp_path_string.clone()),
        report: json!({
            "phase": "prepared",
            "staging_path": paths.staging_path.display().to_string(),
            "target_path": target_path_string,
            "temp_path": temp_path_string,
            "expected_size_bytes": paths.expected_facts.size_bytes,
            "expected_checksum": paths.expected_facts.content_hash,
            "staging_local_file_key": paths.expected_facts.local_file_key,
        }),
        started_at: now,
    };
    let record = create_pending_commit_with_started_event_in_tx(
        &cp.artifacts,
        &cp.events,
        tx,
        pending_input,
        |commit_record_id| {
            Event::ArtifactCommitStarted(ArtifactCommitStartedPayload {
                commit_record_id: commit_record_id.0,
                artifact_handle_id: input.artifact_handle_id.0,
                source_file_version_id: source.source_file_version_id.0,
                verification_id: verified_staging.verification.id.0,
                target_path: paths.target_path.display().to_string(),
                temp_path: paths.temp_path.display().to_string(),
            })
        },
    )
    .await
    .map_err(|err| match err {
        PendingCommitRecordError::BeforePending(err) => {
            PrepareCommitError::PreMutation(pre_mutation(&verified_staging.context, &err))
        }
        PendingCommitRecordError::AfterPending(err) => PrepareCommitError::AfterPending(err),
    })?;

    Ok(PreparedCommit {
        record,
        artifact_handle_id: input.artifact_handle_id,
        source_file_version_id: source.source_file_version_id,
        source_file_asset_id: source.source_file_asset_id,
        staging_location_id: verified_staging.staging.id,
        staging_path: paths.staging_path,
        target_path: paths.target_path,
        temp_path: paths.temp_path,
        expected_facts: paths.expected_facts,
        promotion_started_at: now,
    })
}

#[derive(Debug)]
struct CommitSourceFacts {
    handle: HandleFacts,
    source_file_version_id: FileVersionId,
    source_file_asset_id: FileAssetId,
}

#[derive(Debug)]
struct VerifiedStagingFacts {
    staging: LiveStagingLocation,
    verification: ArtifactVerification,
    context: PreMutationContext,
}

#[derive(Debug)]
struct CommitPreparedPaths {
    target_path: PathBuf,
    staging_path: PathBuf,
    temp_path: PathBuf,
    expected_facts: ArtifactFileFacts,
}

async fn read_commit_source_facts(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    artifact_handle_id: ArtifactHandleId,
    context: &PreMutationContext,
) -> Result<CommitSourceFacts, PrepareCommitError> {
    let handle = read_handle_facts_in_tx(tx, artifact_handle_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    let Some(source_file_version_id) = handle.source_file_version_id else {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "artifact_handle {artifact_handle_id} is not linked to a source file_version"
            )),
        ));
    };
    let Some(source) = cp
        .identity
        .get_file_version_in_tx(tx, source_file_version_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?
    else {
        return Err(pre_mutation_error(
            context,
            &VoomError::NotFound(format!("file_versions {source_file_version_id} missing")),
        ));
    };
    if source.retired_at.is_some() {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!("file_versions {source_file_version_id} is retired")),
        ));
    }

    Ok(CommitSourceFacts {
        handle,
        source_file_version_id,
        source_file_asset_id: source.file_asset_id,
    })
}

async fn read_verified_staging_facts(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    artifact_handle_id: ArtifactHandleId,
    target_path: &Path,
    context: &PreMutationContext,
) -> Result<VerifiedStagingFacts, PrepareCommitError> {
    let staging = live_staging_location_in_tx(tx, artifact_handle_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    let Some(verification) = cp
        .artifacts
        .latest_successful_verification_for_live_staging_in_tx(tx, artifact_handle_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?
    else {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "artifact_handle {artifact_handle_id} has no successful verification for its live staging location"
            )),
        ));
    };
    let context = PreMutationContext {
        artifact_handle_id,
        verification_id: Some(verification.id),
        target_path: target_path.to_owned(),
    };
    if verification.artifact_location_id != staging.id || verification.path != staging.value {
        return Err(pre_mutation_error(
            &context,
            &VoomError::Config(format!(
                "artifact verification {} is stale for live staging location {}",
                verification.id, staging.id
            )),
        ));
    }

    Ok(VerifiedStagingFacts {
        staging,
        verification,
        context,
    })
}

async fn prepare_commit_paths(
    target_path: &Path,
    handle: &HandleFacts,
    verified_staging: &VerifiedStagingFacts,
) -> Result<CommitPreparedPaths, PrepareCommitError> {
    let context = &verified_staging.context;
    let target_path = canonical_new_leaf_no_symlink(target_path)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    let staging_path = PathBuf::from(&verified_staging.staging.value);
    let expected_facts = observe_regular_file(&staging_path)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    require_expected_facts(handle, &verified_staging.verification, &expected_facts)
        .map_err(|err| pre_mutation_error(context, &err))?;
    let temp_path =
        unique_temp_sibling_path(&target_path).map_err(|err| pre_mutation_error(context, &err))?;

    Ok(CommitPreparedPaths {
        target_path,
        staging_path,
        temp_path,
        expected_facts,
    })
}

fn pre_mutation_error(context: &PreMutationContext, err: &VoomError) -> PrepareCommitError {
    PrepareCommitError::PreMutation(pre_mutation(context, err))
}

#[derive(Debug, Clone)]
struct PreMutationContext {
    artifact_handle_id: ArtifactHandleId,
    verification_id: Option<ArtifactVerificationId>,
    target_path: PathBuf,
}

#[derive(Debug)]
struct HandleFacts {
    source_file_version_id: Option<FileVersionId>,
    size_bytes: u64,
    checksum: String,
}

#[derive(Debug)]
struct LiveStagingLocation {
    id: ArtifactLocationId,
    value: String,
}

async fn read_handle_facts_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactHandleId,
) -> Result<HandleFacts, VoomError> {
    let row = sqlx::query(
        "SELECT file_version_id, size_bytes, checksum FROM artifact_handles WHERE id = ?",
    )
    .bind(i64::try_from(id.0).map_err(|err| {
        VoomError::Internal(format!("artifact handle id exceeds SQLite integer: {err}"))
    })?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("artifact_handles commit lookup", err))?;
    let Some(row) = row else {
        return Err(VoomError::NotFound(format!(
            "artifact_handles {id} missing"
        )));
    };
    let file_version_id: Option<i64> = row
        .try_get("file_version_id")
        .map_err(|err| VoomError::database_context("artifact_handles.file_version_id", err))?;
    let size_bytes: Option<i64> = row
        .try_get("size_bytes")
        .map_err(|err| VoomError::database_context("artifact_handles.size_bytes", err))?;
    let checksum: Option<String> = row
        .try_get("checksum")
        .map_err(|err| VoomError::database_context("artifact_handles.checksum", err))?;
    let source_file_version_id = file_version_id
        .map(|v| {
            u64::try_from(v).map(FileVersionId).map_err(|err| {
                VoomError::database_context("artifact_handles.file_version_id negative", err)
            })
        })
        .transpose()?;
    Ok(HandleFacts {
        source_file_version_id,
        size_bytes: u64::try_from(size_bytes.ok_or_else(|| {
            VoomError::Config(format!("artifact_handle {id} missing expected size_bytes"))
        })?)
        .map_err(|err| VoomError::database_context("artifact_handles.size_bytes negative", err))?,
        checksum: checksum.ok_or_else(|| {
            VoomError::Config(format!("artifact_handle {id} missing expected checksum"))
        })?,
    })
}

async fn live_staging_location_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    handle_id: ArtifactHandleId,
) -> Result<LiveStagingLocation, VoomError> {
    let rows = sqlx::query(
        "SELECT id, value FROM artifact_locations \
         WHERE artifact_handle_id = ? AND kind = 'staging' AND retired_at IS NULL \
         ORDER BY id ASC",
    )
    .bind(i64::try_from(handle_id.0).map_err(|err| {
        VoomError::Internal(format!("artifact handle id exceeds SQLite integer: {err}"))
    })?)
    .fetch_all(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("artifact_locations commit live staging", err))?;
    let [row] = rows.as_slice() else {
        return Err(VoomError::Config(format!(
            "artifact_handle {handle_id} must have exactly one live staging location; found {}",
            rows.len()
        )));
    };
    let id: i64 = row
        .try_get("id")
        .map_err(|err| VoomError::database_context("artifact_locations.id", err))?;
    let value = row
        .try_get("value")
        .map_err(|err| VoomError::database_context("artifact_locations.value", err))?;
    let id = u64::try_from(id)
        .map(ArtifactLocationId)
        .map_err(|err| VoomError::database_context("artifact_locations.id negative", err))?;
    Ok(LiveStagingLocation { id, value })
}

fn require_expected_facts(
    handle: &HandleFacts,
    verification: &ArtifactVerification,
    staged: &ArtifactFileFacts,
) -> Result<(), VoomError> {
    if handle.size_bytes != staged.size_bytes
        || handle.checksum != staged.content_hash
        || verification.expected_size_bytes != staged.size_bytes
        || verification.expected_checksum != staged.content_hash
        || verification.observed_size_bytes != Some(staged.size_bytes)
        || verification.observed_checksum.as_deref() != Some(staged.content_hash.as_str())
    {
        return Err(VoomError::ArtifactChecksumMismatch(
            "staged artifact facts no longer match the successful verification".to_owned(),
        ));
    }
    Ok(())
}

fn pre_mutation(context: &PreMutationContext, err: &VoomError) -> CommitArtifactPreMutationReport {
    CommitArtifactPreMutationReport {
        artifact_handle_id: context.artifact_handle_id,
        verification_id: context.verification_id,
        target_path: context.target_path.clone(),
        error_code: err.error_code(),
        message: err.to_string(),
    }
}

async fn append_failed_pre_mutation(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    failure: &CommitArtifactPreMutationReport,
    occurred_at: time::OffsetDateTime,
) -> Result<(), VoomError> {
    append_commit_event_in_tx(
        &cp.events,
        tx,
        failure.artifact_handle_id,
        occurred_at,
        Event::ArtifactCommitFailedPreMutation(ArtifactCommitFailedPreMutationPayload {
            artifact_handle_id: failure.artifact_handle_id.0,
            commit_record_id: None,
            target_path: failure.target_path.display().to_string(),
            error_code: failure.error_code.as_str().to_owned(),
            message: failure.message.clone(),
        }),
    )
    .await
}

async fn promote_prepared(
    _cp: &ControlPlane,
    prepared: &PreparedCommit,
    hooks: &dyn CommitArtifactHooks,
) -> Result<PromotionOutcome, VoomError> {
    let staging_facts = observe_regular_file(&prepared.staging_path).await?;
    if !same_file_facts(&staging_facts, &prepared.expected_facts) {
        return Err(VoomError::ArtifactChecksumMismatch(
            "staged artifact facts drifted after durable prepare".to_owned(),
        ));
    }
    hooks.before_temp_copy(CommitArtifactPreparedContext {
        commit_record_id: prepared.record.id,
        target_path: &prepared.target_path,
        temp_path: &prepared.temp_path,
        staging_path: &prepared.staging_path,
    })?;
    let staging_facts = observe_regular_file(&prepared.staging_path).await?;
    if !same_file_facts(&staging_facts, &prepared.expected_facts) {
        return Err(VoomError::ArtifactChecksumMismatch(
            "staged artifact facts drifted after durable prepare".to_owned(),
        ));
    }
    let temp_facts = copy_regular_file_checked(&prepared.staging_path, &prepared.temp_path).await?;
    if !same_file_facts(&temp_facts, &prepared.expected_facts) {
        let _cleanup = remove_file_if_exists(&prepared.temp_path).await;
        return Err(VoomError::ArtifactChecksumMismatch(
            "temporary artifact facts do not match verified staged artifact".to_owned(),
        ));
    }
    hooks.before_install(CommitArtifactInstallContext {
        commit_record_id: prepared.record.id,
        target_path: &prepared.target_path,
        temp_path: &prepared.temp_path,
    })?;
    install_temp_no_replace(&prepared.temp_path, &prepared.target_path).await?;
    let target_facts = observe_regular_file(&prepared.target_path).await?;
    if !same_file_facts(&target_facts, &prepared.expected_facts) {
        return Err(VoomError::VerificationFailure(
            "committed target facts do not match verified staged artifact".to_owned(),
        ));
    }
    Ok(PromotionOutcome { target_facts })
}

async fn install_temp_no_replace(temp_path: &Path, target_path: &Path) -> Result<(), VoomError> {
    fs::hard_link(temp_path, target_path)
        .await
        .map_err(|err| match err.kind() {
            ErrorKind::AlreadyExists => VoomError::CommitFailure(format!(
                "artifact target already exists: {}",
                target_path.display()
            )),
            _ => VoomError::CommitFailure(format!(
                "cannot install artifact {} to {} without replacement: {err}",
                temp_path.display(),
                target_path.display()
            )),
        })?;
    if let Err(err) = fsync_parent_dir(target_path).await {
        let _ = remove_file_if_exists(target_path).await;
        return Err(err);
    }
    if let Err(err) = fs::remove_file(temp_path).await {
        return Err(VoomError::CommitFailure(format!(
            "cannot remove temporary artifact path {} after install: {err}",
            temp_path.display()
        )));
    }
    fsync_parent_dir(target_path).await
}

#[cfg(unix)]
async fn fsync_parent_dir(path: &Path) -> Result<(), VoomError> {
    let parent = path.parent().unwrap_or_else(|| Path::new(".")).to_owned();
    tokio::task::spawn_blocking(move || {
        std::fs::File::open(&parent)
            .and_then(|file| file.sync_all())
            .map_err(|err| {
                VoomError::CommitFailure(format!(
                    "cannot fsync artifact parent directory {}: {err}",
                    parent.display()
                ))
            })
    })
    .await
    .map_err(|err| VoomError::Internal(format!("artifact directory fsync task failed: {err}")))?
}

#[cfg(not(unix))]
async fn fsync_parent_dir(_path: &Path) -> Result<(), VoomError> {
    Ok(())
}

async fn remove_file_if_exists(path: &Path) -> Result<(), VoomError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(VoomError::CommitFailure(format!(
            "cannot remove artifact path {}: {err}",
            path.display()
        ))),
    }
}

async fn finalize_commit(
    cp: &ControlPlane,
    prepared: &PreparedCommit,
    promotion: &PromotionOutcome,
) -> Result<CommitArtifactReport, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let result_version = cp
        .identity
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: prepared.source_file_asset_id,
                content_hash: promotion.target_facts.content_hash.clone(),
                size_bytes: promotion.target_facts.size_bytes,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: Some(prepared.source_file_version_id),
                created_at: now,
            },
        )
        .await?;
    let result_location = cp
        .identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: result_version.id,
                kind: FileLocationKind::LocalPath,
                value: prepared.target_path.display().to_string(),
                proof: None,
                observed_at: now,
            },
        )
        .await?;
    cp.artifacts
        .retire_location_in_tx(&mut tx, prepared.staging_location_id, now)
        .await?;
    let committed = cp
        .artifacts
        .mark_commit_committed_in_tx(
            &mut tx,
            prepared.record.id,
            result_version.id,
            result_location.id,
            prepared.promotion_started_at,
            now,
        )
        .await?;
    append_commit_event_in_tx(
        &cp.events,
        &mut tx,
        prepared.artifact_handle_id,
        now,
        Event::ArtifactCommitCompleted(ArtifactCommitCompletedPayload {
            commit_record_id: committed.id.0,
            artifact_handle_id: prepared.artifact_handle_id.0,
            result_file_version_id: result_version.id.0,
            result_file_location_id: result_location.id.0,
            target_path: prepared.target_path.display().to_string(),
        }),
    )
    .await?;
    commit_tx(tx).await?;
    Ok(report_from_record(&committed, &prepared.target_path, None))
}

async fn transition_recovery(
    cp: &ControlPlane,
    prepared: &PreparedCommit,
    err: VoomError,
) -> Result<CommitArtifactReport, CommitArtifactCommandError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let recovery = observe_recovery(prepared, recovery_reason(&err)).await;
    update_commit_report_in_tx(&mut tx, prepared.record.id, &recovery)
        .await
        .map_err(CommitArtifactCommandError::from)?;
    let error_code = err.error_code().as_str().to_owned();
    let message = err.to_string();
    let recovered = mark_recovery_required_with_event_in_tx(
        &cp.artifacts,
        &cp.events,
        &mut tx,
        RecoveryRequiredCommit {
            commit_record_id: prepared.record.id,
            artifact_handle_id: prepared.artifact_handle_id,
            failure: ArtifactCommitFailure {
                failure_class: failure_class_for_error(&err),
                error_code: error_code.clone(),
                message: message.clone(),
                finished_at: now,
            },
            recovery_reason: recovery.recovery_reason.clone(),
            event: Event::ArtifactCommitRecoveryRequired(ArtifactCommitRecoveryRequiredPayload {
                commit_record_id: prepared.record.id.0,
                artifact_handle_id: prepared.artifact_handle_id.0,
                target_path: prepared.target_path.display().to_string(),
                temp_path: prepared.temp_path.display().to_string(),
                recovery_reason: recovery.recovery_reason.clone(),
                error_code,
                message,
            }),
            occurred_at: now,
        },
    )
    .await
    .map_err(CommitArtifactCommandError::from)?;
    commit_tx(tx)
        .await
        .map_err(CommitArtifactCommandError::from)?;
    Ok(report_from_record(
        &recovered,
        &prepared.target_path,
        Some(recovery),
    ))
}

async fn observe_recovery(
    prepared: &PreparedCommit,
    recovery_reason: String,
) -> CommitRecoveryReport {
    let target_exists = path_exists(&prepared.target_path).await;
    let temp_exists = path_exists(&prepared.temp_path).await;
    let staging_exists = path_exists(&prepared.staging_path).await;
    CommitRecoveryReport {
        recovery_reason,
        target_path: prepared.target_path.clone(),
        target_exists,
        temp_path: temp_exists.then(|| prepared.temp_path.clone()),
        temp_exists,
        staging_path: prepared.staging_path.clone(),
        staging_exists,
        result_file_version_id: None,
        result_file_location_id: None,
    }
}

async fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).await.is_ok()
}

async fn update_commit_report_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactCommitRecordId,
    recovery: &CommitRecoveryReport,
) -> Result<(), VoomError> {
    let report = serde_json::to_string(&json!({
        "phase": "recovery_required",
        "recovery_reason": recovery.recovery_reason,
        "target_path": recovery.target_path.display().to_string(),
        "target_exists": recovery.target_exists,
        "temp_path": recovery.temp_path.as_ref().map(|p| p.display().to_string()),
        "temp_exists": recovery.temp_exists,
        "staging_path": recovery.staging_path.display().to_string(),
        "staging_exists": recovery.staging_exists,
        "result_file_version_id": recovery.result_file_version_id.map(|id| id.0),
        "result_file_location_id": recovery.result_file_location_id.map(|id| id.0),
    }))
    .map_err(|err| VoomError::Internal(format!("commit recovery report encode: {err}")))?;
    sqlx::query("UPDATE artifact_commit_records SET report = ? WHERE id = ? AND state = 'pending'")
        .bind(report)
        .bind(i64::try_from(id.0).map_err(|err| {
            VoomError::Internal(format!("artifact commit id exceeds SQLite integer: {err}"))
        })?)
        .execute(&mut **tx)
        .await
        .map_err(|err| VoomError::database_context("artifact_commit_records report update", err))?;
    Ok(())
}

fn recovery_reason(err: &VoomError) -> String {
    match err {
        VoomError::ArtifactChecksumMismatch(_) => "staged_bytes_drifted",
        VoomError::VerificationFailure(_) => "target_verification_failed",
        VoomError::Database { .. } => "finalize_failed",
        _ => "promotion_failed",
    }
    .to_owned()
}

fn failure_class_for_error(err: &VoomError) -> String {
    let class = match err {
        VoomError::ArtifactChecksumMismatch(_) => FailureClass::ArtifactChecksumMismatch,
        VoomError::ArtifactUnavailable(_) => FailureClass::ArtifactUnavailable,
        VoomError::VerificationFailure(_) => FailureClass::VerificationFailure,
        _ => FailureClass::CommitFailure,
    };
    serde_json::to_value(class)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "commit_failure".to_owned())
}

fn report_from_record(
    record: &ArtifactCommitRecord,
    target_path: &Path,
    recovery: Option<CommitRecoveryReport>,
) -> CommitArtifactReport {
    CommitArtifactReport {
        commit_record_id: record.id,
        artifact_handle_id: record.artifact_handle_id,
        verification_id: record.verification_id,
        target_path: target_path.to_path_buf(),
        temp_path: record.temp_path.as_ref().map(PathBuf::from),
        state: record.state,
        result_file_version_id: record.result_file_version_id,
        result_file_location_id: record.result_file_location_id,
        recovery_required: recovery,
    }
}

fn same_file_facts(left: &ArtifactFileFacts, right: &ArtifactFileFacts) -> bool {
    left.size_bytes == right.size_bytes && left.content_hash == right.content_hash
}

#[cfg(test)]
#[path = "commit_test.rs"]
mod tests;
