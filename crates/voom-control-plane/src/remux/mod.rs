use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Serialize;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, JobId, LeaseId,
    MediaSnapshotId, TicketId, VoomError,
};
use voom_store::repo::artifacts::ArtifactVerificationStatus;
use voom_worker_protocol::{RemuxResult, RemuxSelection};

use crate::ControlPlane;
use crate::artifact::commit::{
    CommitArtifactCommandError, CommitArtifactInput, CommitArtifactReport,
};
use crate::artifact::verify::{
    NoVerifyArtifactHooks, VerifyArtifactDispatcher, VerifyArtifactInput,
    verify_artifact_with_dispatcher,
};

pub mod commit;
pub mod dispatch;
pub mod events;
pub mod selection;
pub mod source;
pub mod stage;
pub(crate) mod workflow;

#[derive(Debug, Clone)]
pub struct ExecuteRemuxInput {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub operation_payload: serde_json::Value,
    pub staging_root: PathBuf,
    pub target_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecuteRemuxReport {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub staged_artifact_handle_id: ArtifactHandleId,
    pub staged_artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct ExecuteRemuxSuccess {
    pub report: ExecuteRemuxReport,
    pub success_event: events::RemuxSucceededEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExecuteRemuxRecoveryReport {
    pub status: &'static str,
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub staged_artifact_handle_id: ArtifactHandleId,
    pub staged_artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
    pub error: ExecuteRemuxRecoveryError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExecuteRemuxRecoveryError {
    pub code: &'static str,
    pub message: String,
}

pub(crate) fn success_event_recovery_report(
    success: &ExecuteRemuxSuccess,
    source: &VoomError,
) -> ExecuteRemuxRecoveryReport {
    ExecuteRemuxRecoveryReport {
        status: "committed_success_event_failed",
        job_id: success.report.job_id,
        ticket_id: success.report.ticket_id,
        lease_id: success.report.lease_id,
        source_file_version_id: success.report.source_file_version_id,
        source_file_location_id: success.report.source_file_location_id,
        staged_artifact_handle_id: success.report.staged_artifact_handle_id,
        staged_artifact_location_id: success.report.staged_artifact_location_id,
        verification_id: success.report.verification_id,
        commit_record_id: success.report.commit_record_id,
        result_file_version_id: success.report.result_file_version_id,
        result_file_location_id: success.report.result_file_location_id,
        result_media_snapshot_id: success.report.result_media_snapshot_id,
        staging_path: success.report.staging_path.clone(),
        target_path: success.report.target_path.clone(),
        error: ExecuteRemuxRecoveryError {
            code: source.code(),
            message: source.to_string(),
        },
    }
}

#[async_trait]
pub trait RemuxDispatcher: Send + Sync {
    async fn dispatch_remux(
        &self,
        request: voom_worker_protocol::RemuxRequest,
    ) -> Result<RemuxResult, VoomError>;

    async fn dispatch_remux_with_progress(
        &self,
        request: voom_worker_protocol::RemuxRequest,
        _progress: &mut dyn dispatch::RemuxProgressSink,
    ) -> Result<RemuxResult, VoomError> {
        self.dispatch_remux(request).await
    }
}

impl ControlPlane {
    /// Execute one policy-derived `remux` ticket through source revalidation,
    /// worker staging, verification, add-only commit, and result snapshot
    /// persistence.
    ///
    /// # Errors
    /// Returns stable `VoomError` variants for source selection, staging,
    /// worker, verification, commit, and result-probe failures.
    pub async fn execute_remux(
        &self,
        input: ExecuteRemuxInput,
    ) -> Result<ExecuteRemuxReport, VoomError> {
        execute_remux_with_dispatchers(
            self,
            input,
            &dispatch::BundledRemuxDispatcher,
            &crate::artifact::verify::BundledVerifyArtifactDispatcher,
            &commit::BundledRemuxResultProbeDispatcher,
        )
        .await
    }
}

pub(crate) async fn execute_remux_with_dispatchers(
    cp: &ControlPlane,
    input: ExecuteRemuxInput,
    remux: &dyn RemuxDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::RemuxResultProbeDispatcher,
) -> Result<ExecuteRemuxReport, VoomError> {
    Ok(
        execute_remux_core(cp, input, remux, verify, result_probe, true)
            .await?
            .report,
    )
}

pub(crate) async fn execute_remux_with_deferred_success_event(
    cp: &ControlPlane,
    input: ExecuteRemuxInput,
    remux: &dyn RemuxDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::RemuxResultProbeDispatcher,
) -> Result<ExecuteRemuxSuccess, VoomError> {
    execute_remux_core(cp, input, remux, verify, result_probe, false).await
}

#[expect(
    clippy::too_many_lines,
    reason = "the remux workflow is intentionally linear so each fallible step records its failure event with the facts known at that point"
)]
async fn execute_remux_core(
    cp: &ControlPlane,
    input: ExecuteRemuxInput,
    remux: &dyn RemuxDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::RemuxResultProbeDispatcher,
    append_success_event: bool,
) -> Result<ExecuteRemuxSuccess, VoomError> {
    let failure = RemuxFailureContext::new(cp, &input);
    let selected =
        match source::select_source(cp, input.source_file_version_id, input.source_location_id)
            .await
        {
            Ok(selected) => selected,
            Err(err) => {
                failure.record_failure(&err).await?;
                return Err(err);
            }
        };
    let failure = failure.with_source_location(selected.location.id);
    let snapshot = match source::read_media_snapshot(
        cp,
        input.source_file_version_id,
        &input.operation_payload,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(err) => {
            failure.record_failure(&err).await?;
            return Err(err);
        }
    };
    let selection =
        match selection::selection_from_payload_and_snapshot(&input.operation_payload, &snapshot) {
            Ok(selection) => selection,
            Err(err) => {
                failure.record_failure(&err).await?;
                return Err(err);
            }
        };
    let failure = failure.with_selection(&selection);
    let staging = match stage::prepare_staging_path(
        &input.staging_root,
        input.ticket_id,
        input.lease_id,
        std::path::Path::new(&selected.location.value),
    )
    .await
    {
        Ok(staging) => staging,
        Err(err) => {
            failure.record_failure(&err).await?;
            return Err(err);
        }
    };
    let staging_path = staging.path.clone();
    let failure = failure.with_staging_path(&staging_path);
    let target_path = match stage::target_path(
        &input.target_dir,
        std::path::Path::new(&selected.location.value),
    )
    .await
    {
        Ok(path) => path,
        Err(err) => {
            failure.record_failure(&err).await?;
            return Err(err);
        }
    };

    events::record_started(cp, &input, selected.location.id, &selection, &staging_path).await?;
    if let Err(err) = dispatch::revalidate_source_file(&selected).await {
        failure.record_failure(&err).await?;
        return Err(err);
    }
    let request = dispatch::request_for(
        &selected,
        &selection,
        &staging.canonical_root,
        &staging_path,
    );
    let mut progress = EventRemuxProgressSink {
        cp,
        input: &input,
        source_location_id: selected.location.id,
        selection: &selection,
        staging_path: &staging_path,
    };
    let result = match remux
        .dispatch_remux_with_progress(request, &mut progress)
        .await
    {
        Ok(result) => result,
        Err(err) => {
            failure.record_failure(&err).await?;
            return Err(err);
        }
    };
    let failure = failure.with_result(&result);
    if let Err(err) = dispatch::validate_result(&selected, &selection, &result) {
        failure.record_failure(&err).await?;
        return Err(err);
    }
    if let Err(err) = dispatch::require_output_file_matches_result(&staging_path, &result).await {
        failure.record_failure(&err).await?;
        return Err(err);
    }

    let staged =
        match commit::record_staged_remux(cp, &input, selected.location.id, &staging_path, &result)
            .await
        {
            Ok(staged) => staged,
            Err(err) => {
                failure.record_failure(&err).await?;
                return Err(err);
            }
        };
    let failure = failure.with_staged(&staged);
    let verified = match verify_artifact_with_dispatcher(
        cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        verify,
        &NoVerifyArtifactHooks,
    )
    .await
    {
        Ok(verified) => verified,
        Err(err) => {
            failure.record_failure(&err).await?;
            return Err(err);
        }
    };
    if verified.status != ArtifactVerificationStatus::Succeeded {
        let err = VoomError::VerificationFailure(format!(
            "remux artifact verification failed for {}",
            staged.artifact_handle_id
        ));
        failure.record_failure(&err).await?;
        return Err(err);
    }
    let probed = match commit::probe_staged_result(cp, &staging_path, &result, result_probe).await {
        Ok(probed) => probed,
        Err(err) => {
            failure.record_failure(&err).await?;
            return Err(err);
        }
    };
    let committed = match cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target_path.clone(),
        })
        .await
    {
        Ok(committed) => committed,
        Err(err) => {
            let err = remux_commit_failure(&err);
            failure.record_failure(&err).await?;
            return Err(err);
        }
    };
    let Some(result_file_version_id) = committed.result_file_version_id else {
        let err = VoomError::Internal("committed remux missing result_file_version_id".to_owned());
        failure.record_failure(&err).await?;
        return Err(err);
    };
    let Some(result_file_location_id) = committed.result_file_location_id else {
        let err = VoomError::Internal("committed remux missing result_file_location_id".to_owned());
        failure.record_failure(&err).await?;
        return Err(err);
    };
    let snapshot = commit::record_result_snapshot_payload(cp, result_file_version_id, probed)
        .await
        .map_err(|err| {
            VoomError::ExternalSystemUnavailable(format!(
                "remux result snapshot failed after commit_record_id={} result_file_version_id={} result_file_location_id={}: {err}",
                committed.commit_record_id.0, result_file_version_id.0, result_file_location_id.0
            ))
        })?;
    let success_event =
        events::RemuxSucceededEvent::from_input(&events::RemuxSucceededEventInput {
            input: &input,
            source_location_id: selected.location.id,
            selection: &selection,
            staging_path: &staging_path,
            artifact_handle_id: staged.artifact_handle_id,
            artifact_location_id: staged.artifact_location_id,
            result: &result,
        });
    if append_success_event {
        events::record_succeeded(
            cp,
            events::RemuxSucceededEventInput {
                input: &input,
                source_location_id: selected.location.id,
                selection: &selection,
                staging_path: &staging_path,
                artifact_handle_id: staged.artifact_handle_id,
                artifact_location_id: staged.artifact_location_id,
                result: &result,
            },
        )
        .await?;
    }

    Ok(ExecuteRemuxSuccess {
        report: ExecuteRemuxReport {
            job_id: input.job_id,
            ticket_id: input.ticket_id,
            lease_id: input.lease_id,
            source_file_version_id: input.source_file_version_id,
            source_file_location_id: selected.location.id,
            staged_artifact_handle_id: staged.artifact_handle_id,
            staged_artifact_location_id: staged.artifact_location_id,
            verification_id: verified.verification_id,
            commit_record_id: committed.commit_record_id,
            result_file_version_id,
            result_file_location_id,
            result_media_snapshot_id: snapshot.id,
            staging_path,
            target_path,
        },
        success_event,
    })
}

#[derive(Clone, Copy)]
struct RemuxFailureContext<'a> {
    cp: &'a ControlPlane,
    input: &'a ExecuteRemuxInput,
    source_location_id: Option<FileLocationId>,
    selection: Option<&'a RemuxSelection>,
    staging_path: Option<&'a Path>,
    result: Option<&'a RemuxResult>,
    staged: Option<&'a commit::StagedRemuxArtifact>,
}

impl<'a> RemuxFailureContext<'a> {
    fn new(cp: &'a ControlPlane, input: &'a ExecuteRemuxInput) -> Self {
        Self {
            cp,
            input,
            source_location_id: None,
            selection: None,
            staging_path: None,
            result: None,
            staged: None,
        }
    }

    fn with_source_location(self, source_location_id: FileLocationId) -> Self {
        Self {
            source_location_id: Some(source_location_id),
            ..self
        }
    }

    fn with_selection(self, selection: &'a RemuxSelection) -> Self {
        Self {
            selection: Some(selection),
            ..self
        }
    }

    fn with_staging_path(self, staging_path: &'a Path) -> Self {
        Self {
            staging_path: Some(staging_path),
            ..self
        }
    }

    fn with_result(self, result: &'a RemuxResult) -> Self {
        Self {
            result: Some(result),
            ..self
        }
    }

    fn with_staged(self, staged: &'a commit::StagedRemuxArtifact) -> Self {
        Self {
            staged: Some(staged),
            ..self
        }
    }

    async fn record_failure(self, err: &VoomError) -> Result<(), VoomError> {
        events::record_failed(
            self.cp,
            events::RemuxFailedEventInput {
                input: self.input,
                source_location_id: self.source_location_id,
                selection: self.selection,
                staging_path: self.staging_path,
                artifact_handle_id: self.staged.map(|staged| staged.artifact_handle_id),
                artifact_location_id: self.staged.map(|staged| staged.artifact_location_id),
                result: self.result,
                error: err,
            },
        )
        .await
    }
}

struct EventRemuxProgressSink<'a> {
    cp: &'a ControlPlane,
    input: &'a ExecuteRemuxInput,
    source_location_id: FileLocationId,
    selection: &'a RemuxSelection,
    staging_path: &'a Path,
}

#[async_trait]
impl dispatch::RemuxProgressSink for EventRemuxProgressSink<'_> {
    async fn record_remux_progress(
        &mut self,
        percent: Option<voom_worker_protocol::PercentBps>,
        message: Option<String>,
    ) -> Result<(), VoomError> {
        events::record_progress(
            self.cp,
            events::RemuxProgressEventInput {
                input: self.input,
                source_location_id: self.source_location_id,
                selection: self.selection,
                staging_path: self.staging_path,
                percent,
                message,
            },
        )
        .await
    }
}

fn remux_commit_failure(err: &CommitArtifactCommandError) -> VoomError {
    VoomError::CommitFailure(format_commit_failure_message(
        &err.to_string(),
        err.commit_report(),
    ))
}

fn format_commit_failure_message(
    message: &str,
    commit_report: Option<&CommitArtifactReport>,
) -> String {
    let Some(report) = commit_report else {
        return message.to_owned();
    };

    let mut details = vec![
        format!("commit_record_id={}", report.commit_record_id.0),
        format!("artifact_handle_id={}", report.artifact_handle_id.0),
        format!("verification_id={}", report.verification_id.0),
        format!("target_path={}", report.target_path.display()),
        format!("state={}", report.state.as_str()),
    ];
    if let Some(temp_path) = &report.temp_path {
        details.push(format!("temp_path={}", temp_path.display()));
    }
    if let Some(id) = report.result_file_version_id {
        details.push(format!("result_file_version_id={}", id.0));
    }
    if let Some(id) = report.result_file_location_id {
        details.push(format!("result_file_location_id={}", id.0));
    }
    if let Some(recovery) = &report.recovery_required {
        details.push(format!("recovery_reason={}", recovery.recovery_reason));
        details.push(format!("target_path={}", recovery.target_path.display()));
        details.push(format!("target_exists={}", recovery.target_exists));
        if let Some(temp_path) = &recovery.temp_path {
            details.push(format!("temp_path={}", temp_path.display()));
        }
        details.push(format!("temp_exists={}", recovery.temp_exists));
        details.push(format!("staging_path={}", recovery.staging_path.display()));
        details.push(format!("staging_exists={}", recovery.staging_exists));
        if let Some(id) = recovery.result_file_version_id {
            details.push(format!("result_file_version_id={}", id.0));
        }
        if let Some(id) = recovery.result_file_location_id {
            details.push(format!("result_file_location_id={}", id.0));
        }
    }

    format!("{message}; {}", details.join(" "))
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
