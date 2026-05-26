use std::path::PathBuf;

use async_trait::async_trait;
use serde::Serialize;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, JobId, LeaseId,
    MediaSnapshotId, TicketId, VoomError,
};
use voom_store::repo::artifacts::ArtifactVerificationStatus;
use voom_worker_protocol::RemuxResult;

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

#[async_trait]
pub trait RemuxDispatcher: Send + Sync {
    async fn dispatch_remux(
        &self,
        request: voom_worker_protocol::RemuxRequest,
    ) -> Result<RemuxResult, VoomError>;
}

impl ControlPlane {
    /// Execute one policy-derived `remux` ticket through source revalidation,
    /// worker staging, verification, add-only commit, and result snapshot
    /// persistence.
    pub async fn execute_remux(
        &self,
        input: ExecuteRemuxInput,
    ) -> Result<ExecuteRemuxReport, VoomError> {
        execute_remux_with_dispatchers(
            self,
            input,
            &dispatch::BundledRemuxDispatcher,
            &crate::artifact::verify::BundledVerifyArtifactDispatcher,
        )
        .await
    }
}

pub(crate) async fn execute_remux_with_dispatchers(
    cp: &ControlPlane,
    input: ExecuteRemuxInput,
    remux: &dyn RemuxDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
) -> Result<ExecuteRemuxReport, VoomError> {
    let selected =
        source::select_source(cp, input.source_file_version_id, input.source_location_id).await?;
    let snapshot =
        source::read_media_snapshot(cp, input.source_file_version_id, &input.operation_payload)
            .await?;
    let selection =
        selection::selection_from_payload_and_snapshot(&input.operation_payload, &snapshot)?;
    let staging = stage::prepare_staging_path(
        &input.staging_root,
        input.ticket_id,
        input.lease_id,
        std::path::Path::new(&selected.location.value),
    )
    .await?;
    let staging_path = staging.path.clone();
    let target_path = stage::target_path(
        &input.target_dir,
        std::path::Path::new(&selected.location.value),
    )
    .await?;

    events::record_started(cp, &input, selected.location.id, &selection, &staging_path)?;
    dispatch::revalidate_source_file(&selected).await?;
    let request = dispatch::request_for(
        &selected,
        &selection,
        &staging.canonical_root,
        &staging_path,
    )?;
    let result = remux.dispatch_remux(request).await?;
    dispatch::validate_result(&selected, &selection, &result)?;
    dispatch::require_output_file_matches_result(&staging_path, &result).await?;

    let staged =
        commit::record_staged_remux(cp, &input, selected.location.id, &staging_path, &result)
            .await?;
    let verified = verify_artifact_with_dispatcher(
        cp,
        VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
        },
        verify,
        &NoVerifyArtifactHooks,
    )
    .await?;
    if verified.status != ArtifactVerificationStatus::Succeeded {
        return Err(VoomError::VerificationFailure(format!(
            "remux artifact verification failed for {}",
            staged.artifact_handle_id
        )));
    }
    let committed = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target_path.clone(),
        })
        .await
        .map_err(|err| remux_commit_failure(&err))?;
    let result_file_version_id = committed.result_file_version_id.ok_or_else(|| {
        VoomError::Internal("committed remux missing result_file_version_id".to_owned())
    })?;
    let result_file_location_id = committed.result_file_location_id.ok_or_else(|| {
        VoomError::Internal("committed remux missing result_file_location_id".to_owned())
    })?;
    let snapshot = commit::record_result_snapshot(cp, result_file_version_id, &result)
        .await
        .map_err(|err| {
            VoomError::ExternalSystemUnavailable(format!(
                "remux result snapshot failed after commit_record_id={} result_file_version_id={} result_file_location_id={}: {err}",
                committed.commit_record_id.0, result_file_version_id.0, result_file_location_id.0
            ))
        })?;
    events::record_succeeded(
        cp,
        &input,
        selected.location.id,
        staged.artifact_handle_id,
        staged.artifact_location_id,
        &result,
    )?;

    Ok(ExecuteRemuxReport {
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
    })
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
