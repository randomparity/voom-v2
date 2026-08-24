//! Staged-artifact commit driver (ADR 0074): prepare a fenced node-local
//! commit intent, then wait a bounded time for the storage-owner node to
//! authorize, promote, and report completion through the case functions in
//! [`intent`]. The control plane never opens staging or target bytes.

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::Duration;

use voom_core::ids::{
    ArtifactCommitIntentId, ArtifactCommitRecordId, ArtifactLocationId, ArtifactVerificationId,
};
use voom_core::{
    ArtifactHandleId, ErrorCode, FileLocationId, FileVersionId, ProviderRelativeLocator,
    StorageRootId, VoomError,
};
use voom_store::repo::media::artifacts::{ArtifactCommitRecord, ArtifactCommitState};

use crate::ControlPlane;

pub(crate) mod finalize;
pub(crate) mod intent;

#[cfg(test)]
pub(crate) mod commit_test_support;

mod prepare;
mod recovery;

/// Upper bound on how long [`ControlPlane::commit_artifact`] waits after the
/// durable prepare for the fenced intent to reach a terminal state. Must
/// comfortably exceed several node poll cycles: the storage-owner agent
/// discovers the pending intent through the open-intent listing, authorizes,
/// journals, promotes, and reports on its own schedule. On deadline the
/// pending record stays `pending` (recoverable) and the caller receives a
/// `CommitFailure` naming the intent.
pub const COMMIT_CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll cadence of the bounded wait. Purely local (one indexed read), so a
/// short interval keeps driver latency low without meaningful load.
const COMMIT_CONVERGENCE_POLL: Duration = Duration::from_millis(200);

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

/// Durable recovery evidence for a stuck commit. The driver is byte-blind, so
/// the `*_exists` flags here describe only what the durable record claims;
/// live path observation stays with inspection (`show_artifact`).
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
    /// Commit a verified staged artifact by preparing a fenced node-local
    /// commit intent and waiting a bounded time
    /// ([`COMMIT_CONVERGENCE_TIMEOUT`]) for the storage-owner node to drive it
    /// to a terminal state through the authorize/receipt/complete case
    /// functions.
    ///
    /// # Errors
    /// Returns `Config`/`ArtifactChecksumMismatch` before durable prepare when
    /// commit preconditions fail. Once a pending record is prepared, a
    /// `recovery_required` or `failed` terminal report is returned as a
    /// command error carrying that report; a convergence deadline elapses as a
    /// `CommitFailure` naming the pending intent (the record stays pending and
    /// remains recoverable).
    pub async fn commit_artifact(
        &self,
        input: CommitArtifactInput,
    ) -> Result<CommitArtifactReport, CommitArtifactCommandError> {
        commit_artifact_with_hooks(self, input, &NoCommitArtifactHooks).await
    }

    /// Re-drive a stuck commit from node receipts (spec step 7, ADR 0074).
    ///
    /// Classifies the non-terminal record's fenced intent:
    /// receipt-less (pending, or authorized with no journal) aborts fail-closed
    /// and prepares a fresh successor generation; an `applied` receipt with
    /// matching facts finalizes directly without further mutation; a
    /// supplemental not-applied re-observation aborts and re-drives a fresh
    /// generation; anything ambiguous (`mismatched`, unresolved
    /// `outcome_unknown`, epoch drift) is operator-required and the record
    /// stays put.
    ///
    /// # Errors
    /// `Conflict` when the artifact has no non-terminal commit or the evidence
    /// requires an operator; `NotFound`/`Config`/`Database` for missing inputs
    /// or durable failures.
    pub async fn recover_commit(
        &self,
        artifact_handle_id: ArtifactHandleId,
    ) -> Result<CommitArtifactReport, VoomError> {
        recovery::recover_commit(self, artifact_handle_id).await
    }
}

#[derive(Debug, Clone, Copy)]
#[expect(
    dead_code,
    reason = "test-only commit hooks inspect whichever context fields their failure mode needs"
)]
pub(crate) struct CommitArtifactPreparedContext<'a> {
    pub commit_record_id: ArtifactCommitRecordId,
    pub intent_id: ArtifactCommitIntentId,
    pub target_path: &'a Path,
}

pub(crate) trait CommitArtifactHooks: Send + Sync {
    fn after_prepare(&self, _context: CommitArtifactPreparedContext<'_>) -> Result<(), VoomError> {
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
        intent_id: prepared.intent_id,
        target_path: &prepared.finalize.target_path,
    }) {
        let report = recovery::abort_prepared_after_hook_failure(cp, &prepared, err).await?;
        return Err(CommitArtifactCommandError::committed_error(
            &VoomError::CommitFailure("commit failed after durable prepare".to_owned()),
            report,
        ));
    }
    wait_for_commit_convergence(
        cp,
        prepared.artifact_handle_id,
        prepared.record.id,
        &prepared.finalize.target_path,
        Some(prepared.intent_id),
    )
    .await
}

async fn wait_for_commit_convergence(
    cp: &ControlPlane,
    artifact_handle_id: ArtifactHandleId,
    record_id: ArtifactCommitRecordId,
    target_path: &Path,
    intent_id: Option<ArtifactCommitIntentId>,
) -> Result<CommitArtifactReport, CommitArtifactCommandError> {
    let deadline = tokio::time::Instant::now() + COMMIT_CONVERGENCE_TIMEOUT;
    loop {
        let record = cp
            .artifacts
            .list_commit_records(artifact_handle_id)
            .await
            .map_err(CommitArtifactCommandError::from)?
            .into_iter()
            .find(|record| record.id == record_id)
            .ok_or_else(|| {
                CommitArtifactCommandError::from(VoomError::database(format!(
                    "artifact commit record {record_id} vanished while waiting for convergence"
                )))
            })?;
        match record.state {
            ArtifactCommitState::Committed => {
                return Ok(finalize::report_from_record(&record, target_path, None));
            }
            ArtifactCommitState::Failed | ArtifactCommitState::RecoveryRequired => {
                let report = finalize::report_from_record(
                    &record,
                    target_path,
                    Some(recovery::durable_recovery_report(&record)),
                );
                let message = record
                    .message
                    .clone()
                    .unwrap_or_else(|| "commit requires recovery".to_owned());
                return Err(CommitArtifactCommandError {
                    code: record.error_code.unwrap_or(ErrorCode::CommitFailure),
                    message,
                    pre_mutation_report: None,
                    commit_report: Some(report),
                });
            }
            ArtifactCommitState::Pending => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(CommitArtifactCommandError {
                code: ErrorCode::CommitFailure,
                message: format!(
                    "artifact commit {} did not converge within {}s; \
                     artifact_commit_intent {} remains pending and recoverable",
                    record_id,
                    COMMIT_CONVERGENCE_TIMEOUT.as_secs(),
                    intent_id.map_or(record_id.0, |id| id.0),
                ),
                pre_mutation_report: None,
                commit_report: None,
            });
        }
        tokio::time::sleep(COMMIT_CONVERGENCE_POLL).await;
    }
}

/// Everything the finalize transaction needs to converge one prepared commit.
/// The control plane is byte-blind, so target facts come from node-reported
/// evidence validated against the pinned expected facts.
#[derive(Debug)]
pub(crate) struct CommitFinalizeInput {
    pub record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub source_file_asset_id: voom_core::FileAssetId,
    pub source_file_version_id: FileVersionId,
    /// The `artifact_locations` kind=staging marker retired at finalize.
    pub staging_artifact_location_id: ArtifactLocationId,
    /// The rooted `file_locations` row addressing the staged bytes, with its
    /// pinned epoch; retired at finalize (spec amendment, ADR 0074).
    pub staging_file_location: Option<(FileLocationId, u64)>,
    pub target_storage_root_id: StorageRootId,
    pub target_relative_locator: ProviderRelativeLocator,
    pub target_path: PathBuf,
    pub promotion_started_at: time::OffsetDateTime,
    /// Use-lease ids the commit safety gate evaluated at prepare time (none
    /// blocked). Recorded on the `ArtifactCommitCompleted` event for audit.
    pub gate_evaluated_lease_ids: Vec<voom_core::UseLeaseId>,
}

#[derive(Debug)]
pub(super) struct PreparedCommit {
    pub(super) record: ArtifactCommitRecord,
    pub(super) intent_id: ArtifactCommitIntentId,
    pub(super) artifact_handle_id: ArtifactHandleId,
    pub(super) finalize: CommitFinalizeInput,
}

#[cfg(test)]
#[path = "mod_test.rs"]
pub(super) mod tests;
