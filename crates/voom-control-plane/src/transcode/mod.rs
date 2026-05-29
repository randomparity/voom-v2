use std::path::PathBuf;

use async_trait::async_trait;
use serde::Serialize;
use voom_core::ids::ArtifactCommitRecordId;
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, JobId, LeaseId,
    MediaSnapshotId, TicketId, VoomError,
};
use voom_store::repo::artifacts::ArtifactVerificationStatus;
use voom_store::repo::identity::IdentityRepo;
use voom_worker_protocol::TranscodeVideoResult;

use crate::ControlPlane;
use crate::artifact::commit::CommitArtifactInput;
use crate::artifact::verify::{
    NoVerifyArtifactHooks, VerifyArtifactDispatcher, VerifyArtifactInput,
    verify_artifact_with_dispatcher,
};

pub mod commit;
pub mod dispatch;
pub mod events;
pub mod resolve;
pub mod source;
pub mod stage;

#[derive(Debug, Clone)]
pub struct ExecuteTranscodeVideoInput {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub staging_root: PathBuf,
    pub target_dir: PathBuf,
    /// The resolved video encode profile plus output container, threaded from
    /// the ticket payload (binding.rs embeds it from the planner node payload).
    pub resolved: resolve::ResolvedProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecuteTranscodeVideoReport {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub staged_artifact_handle_id: ArtifactHandleId,
    pub staged_artifact_location_id: ArtifactLocationId,
    pub verification_id: voom_core::ids::ArtifactVerificationId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
}

#[async_trait]
pub trait TranscodeVideoDispatcher: Send + Sync {
    async fn dispatch_transcode_video(
        &self,
        request: voom_worker_protocol::TranscodeVideoRequest,
    ) -> Result<TranscodeVideoResult, VoomError>;
}

impl ControlPlane {
    /// Execute one policy-derived `transcode_video` ticket through source
    /// revalidation, worker staging, verification, add-only commit, and result
    /// media-snapshot persistence.
    ///
    /// # Errors
    /// Returns stable `VoomError` variants for source selection, staging,
    /// worker, verification, commit, and result-probe failures.
    pub async fn execute_transcode_video(
        &self,
        input: ExecuteTranscodeVideoInput,
    ) -> Result<ExecuteTranscodeVideoReport, VoomError> {
        execute_transcode_video_with_dispatchers(
            self,
            input,
            &dispatch::BundledTranscodeVideoDispatcher,
            &crate::artifact::verify::BundledVerifyArtifactDispatcher,
        )
        .await
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "transcode execution flow keeps all I/O steps sequential and together; \
              splitting would distribute context across multiple fn boundaries"
)]
pub(crate) async fn execute_transcode_video_with_dispatchers(
    cp: &ControlPlane,
    input: ExecuteTranscodeVideoInput,
    transcode: &dyn TranscodeVideoDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
) -> Result<ExecuteTranscodeVideoReport, VoomError> {
    let selected =
        source::select_source(cp, input.source_file_version_id, input.source_location_id).await?;

    // Re-read the latest media snapshot for the source file version to decide
    // copy_video. Uses the most recent snapshot (highest id), mirroring how
    // the planner reads facts. If no snapshot exists, no copy (we cannot verify
    // source compliance without observable facts).
    let source_snapshot = {
        let snapshots = cp
            .identity
            .list_media_snapshots_by_version(input.source_file_version_id)
            .await?;
        snapshots.into_iter().max_by_key(|s| s.id)
    };
    let copy_video = source_snapshot.as_ref().is_some_and(|s| {
        let snapshot_input = crate::media_snapshot::planning_input(s);
        resolve::decide_copy_video(&input.resolved.profile, &snapshot_input)
    });

    let staging_path = stage::staging_path(
        &input.staging_root,
        input.ticket_id,
        input.lease_id,
        &selected.location.value,
        &input.resolved.profile.name,
        &input.resolved.profile.target_codec,
        &input.resolved.output_container,
    )
    .await?;
    let target_path = stage::target_path(
        &input.target_dir,
        &selected.location.value,
        &input.resolved.profile.name,
        &input.resolved.profile.target_codec,
        &input.resolved.output_container,
    )
    .await?;

    events::record_started(cp, &input, selected.location.id, &staging_path).await?;
    let request = dispatch::request_for(
        &selected,
        &input.resolved,
        copy_video,
        &input.staging_root,
        &staging_path,
    )?;
    let result = transcode.dispatch_transcode_video(request.clone()).await?;
    dispatch::validate_result(&selected, &request, &result)?;
    dispatch::require_output_file_matches_result(&staging_path, &result).await?;

    let staged =
        commit::record_staged_transcode(cp, &input, selected.location.id, &staging_path, &result)
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
            "transcode artifact verification failed for {}",
            staged.artifact_handle_id
        )));
    }
    let committed = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target_path.clone(),
        })
        .await
        .map_err(|err| VoomError::CommitFailure(err.to_string()))?;
    let result_file_version_id = committed.result_file_version_id.ok_or_else(|| {
        VoomError::Internal("committed transcode missing result_file_version_id".to_owned())
    })?;
    let result_file_location_id = committed.result_file_location_id.ok_or_else(|| {
        VoomError::Internal("committed transcode missing result_file_location_id".to_owned())
    })?;
    let snapshot = commit::record_result_snapshot(cp, result_file_version_id, &result).await?;
    events::record_succeeded(
        cp,
        &input,
        selected.location.id,
        staged.artifact_handle_id,
        staged.artifact_location_id,
        &staging_path,
        &result,
    )
    .await?;

    Ok(ExecuteTranscodeVideoReport {
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

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
