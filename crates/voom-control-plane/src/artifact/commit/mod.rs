use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, ErrorCode, FileLocationId, FileVersionId, VoomError,
};
use voom_store::repo::artifacts::{ArtifactCommitRecord, ArtifactCommitState};

use crate::ControlPlane;
use crate::artifact::fs::ArtifactFileFacts;

mod finalize;
mod prepare;
mod promote;
mod recovery;

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
        recovery::recover_commit_inner(self, artifact_handle_id).await
    }
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
pub(super) struct NoCommitArtifactHooks;

impl CommitArtifactHooks for NoCommitArtifactHooks {}

pub(crate) async fn commit_artifact_with_hooks(
    cp: &ControlPlane,
    input: CommitArtifactInput,
    hooks: &dyn CommitArtifactHooks,
) -> Result<CommitArtifactReport, CommitArtifactCommandError> {
    let prepared = prepare::prepare_commit(cp, input).await?;
    if let Err(err) = hooks.after_prepare(CommitArtifactPreparedContext {
        commit_record_id: prepared.record.id,
        target_path: &prepared.target_path,
        temp_path: &prepared.temp_path,
        staging_path: &prepared.staging_path,
    }) {
        let report = recovery::transition_recovery(cp, &prepared, err).await?;
        return Err(CommitArtifactCommandError::committed_error(
            &VoomError::CommitFailure("commit failed after durable prepare".to_owned()),
            report,
        ));
    }

    let promotion = match promote::promote_prepared(cp, &prepared, hooks).await {
        Ok(promotion) => promotion,
        Err(err) => {
            let report = recovery::transition_recovery(cp, &prepared, err).await?;
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
        let report = recovery::transition_recovery(cp, &prepared, err).await?;
        return Err(CommitArtifactCommandError::committed_error(
            &VoomError::database("commit finalize requires recovery"),
            report,
        ));
    }

    match finalize::finalize_commit(cp, &prepared, &promotion).await {
        Ok(report) => Ok(report),
        Err(err) => {
            let code = err.error_code();
            let report = recovery::transition_recovery(cp, &prepared, err).await?;
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
pub(super) struct PreparedCommit {
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
    /// Use-lease ids the commit safety gate evaluated at prepare time (none
    /// blocked). Recorded on the `ArtifactCommitCompleted` event for audit.
    gate_evaluated_lease_ids: Vec<voom_core::UseLeaseId>,
}

#[derive(Debug)]
pub(super) struct PromotionOutcome {
    target_facts: ArtifactFileFacts,
}

pub(super) fn same_file_facts(left: &ArtifactFileFacts, right: &ArtifactFileFacts) -> bool {
    left.size_bytes == right.size_bytes && left.content_hash == right.content_hash
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
