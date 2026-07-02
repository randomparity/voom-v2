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
pub(crate) mod workflow;

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
    /// Opt-in backup-before-mutation destination root; `Some` backs up the
    /// source before dispatch (ADR 0025).
    pub backup_root: Option<PathBuf>,
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
    pub resolved_profile: String,
    pub encoder: String,
    pub target_codec: String,
    pub output_container: String,
    pub copied_video: bool,
    pub output_width: u32,
    pub output_height: u32,
    pub output_pixel_format: String,
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
            &commit::BundledTranscodeResultProbeDispatcher,
        )
        .await
    }
}

/// Decides `copy_video` for a transcode by re-reading the LATEST media snapshot
/// (`max_by_key` on snapshot id) for the source file version and running
/// [`resolve::decide_copy_video`] against the resolved profile.
///
/// Re-reading the latest snapshot is by design: any drift between the snapshot
/// the planner saw and the one observed here fails loud downstream via the
/// worker's copy-precondition revalidation plus [`dispatch::validate_result`].
/// Returns `false` when no snapshot exists (cannot verify source compliance
/// without observable facts).
///
/// # Errors
/// Returns the underlying store error if the snapshot lookup fails.
async fn decide_copy_video_for_source(
    cp: &ControlPlane,
    source_file_version_id: FileVersionId,
    resolved: &resolve::ResolvedProfile,
) -> Result<bool, VoomError> {
    let snapshots = cp
        .identity
        .list_media_snapshots_by_version(source_file_version_id)
        .await?;
    let latest = snapshots.into_iter().max_by_key(|s| s.id);
    Ok(latest.as_ref().is_some_and(|s| {
        let snapshot_input = crate::media_snapshot::planning_input(s);
        resolve::decide_copy_video(&resolved.profile, &snapshot_input)
    }))
}

pub(crate) async fn execute_transcode_video_with_dispatchers(
    cp: &ControlPlane,
    input: ExecuteTranscodeVideoInput,
    transcode: &dyn TranscodeVideoDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::TranscodeResultProbeDispatcher,
) -> Result<ExecuteTranscodeVideoReport, VoomError> {
    let selected =
        source::select_source(cp, input.source_file_version_id, input.source_location_id).await?;

    crate::backup::maybe_back_up_source(
        cp,
        input.backup_root.as_deref(),
        &selected.canonical_path,
        input.source_file_version_id,
        input.job_id,
        input.ticket_id,
    )
    .await?;

    let copy_video =
        decide_copy_video_for_source(cp, input.source_file_version_id, &input.resolved).await?;

    let output_name = stage::OutputName {
        source_path: &selected.location.value,
        profile_id: &input.resolved.profile.name,
        codec: &input.resolved.profile.target_codec,
        container: &input.resolved.output_container,
    };
    let staging_path = stage::staging_path(
        &input.staging_root,
        input.ticket_id,
        input.lease_id,
        &output_name,
    )
    .await?;
    let target_path = stage::target_path(&input.target_dir, &output_name).await?;

    events::record_started(cp, &input, selected.location.id, &staging_path).await?;
    dispatch::revalidate_source_file(&selected).await?;
    let request = dispatch::transcode_video_request_for(
        &selected,
        &input.resolved,
        copy_video,
        &input.staging_root,
        &staging_path,
    );
    let result = transcode.dispatch_transcode_video(request.clone()).await?;
    dispatch::validate_result(&selected, &request, &result)?;
    dispatch::require_output_file_matches_result(&staging_path, &result).await?;

    let staged =
        commit::record_staged_transcode(cp, &input, selected.location.id, &staging_path, &result)
            .await?;
    let verified =
        verify_staged_transcode(cp, staged.artifact_handle_id, &input.staging_root, verify).await?;
    let committed = commit_and_probe_transcode_result(
        cp,
        staged.artifact_handle_id,
        CommitTranscodePaths {
            staging_path: &staging_path,
            target_path: &target_path,
        },
        &result,
        result_probe,
    )
    .await?;
    let CommittedTranscodeResult {
        commit_record_id,
        result_file_version_id,
        result_file_location_id,
        snapshot,
    } = committed;
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
        commit_record_id,
        result_file_version_id,
        result_file_location_id,
        result_media_snapshot_id: snapshot.id,
        staging_path,
        target_path,
        resolved_profile: input.resolved.profile.name.clone(),
        encoder: input.resolved.profile.encoder.clone(),
        target_codec: input.resolved.profile.target_codec.clone(),
        output_container: result.output_container.clone(),
        copied_video: result.copied_video,
        output_width: result.output_width,
        output_height: result.output_height,
        output_pixel_format: result.output_pixel_format.clone(),
    })
}

struct CommittedTranscodeResult {
    commit_record_id: ArtifactCommitRecordId,
    result_file_version_id: FileVersionId,
    result_file_location_id: FileLocationId,
    snapshot: voom_store::repo::identity::MediaSnapshot,
}

struct CommitTranscodePaths<'a> {
    staging_path: &'a std::path::Path,
    target_path: &'a std::path::Path,
}

/// Probe the STAGED result first, then add-only commit the verified artifact,
/// then record the already-probed media snapshot against the committed version.
///
/// Probe-before-commit is deliberate: the fallible external probe runs against
/// the content-hash-verified staged file (byte-identical to the committed
/// target, since commit is an add-only promotion). A transient probe failure
/// therefore leaves nothing committed and the ticket retries cleanly from
/// staging — it can no longer orphan a committed result with no `MediaSnapshot`.
///
/// Only a local DB write (the snapshot record) remains after commit; if THAT
/// fails the returned error embeds the commit record id and result
/// FileVersion/FileLocation ids so an agent can inspect or re-record.
async fn commit_and_probe_transcode_result(
    cp: &ControlPlane,
    artifact_handle_id: ArtifactHandleId,
    paths: CommitTranscodePaths<'_>,
    result: &TranscodeVideoResult,
    result_probe: &dyn commit::TranscodeResultProbeDispatcher,
) -> Result<CommittedTranscodeResult, VoomError> {
    let probed = commit::probe_staged_result(cp, paths.staging_path, result, result_probe).await?;
    let committed = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id,
            target_path: paths.target_path.to_path_buf(),
        })
        .await
        .map_err(|err| VoomError::CommitFailure(err.to_string()))?;
    let result_file_version_id = committed.result_file_version_id.ok_or_else(|| {
        VoomError::Internal("committed transcode missing result_file_version_id".to_owned())
    })?;
    let result_file_location_id = committed.result_file_location_id.ok_or_else(|| {
        VoomError::Internal("committed transcode missing result_file_location_id".to_owned())
    })?;
    let snapshot = commit::record_result_snapshot_payload(cp, result_file_version_id, probed)
        .await
        .map_err(|err| {
            VoomError::ExternalSystemUnavailable(format!(
                "transcode result snapshot failed after commit_record_id={} result_file_version_id={} result_file_location_id={}: {err}",
                committed.commit_record_id.0, result_file_version_id.0, result_file_location_id.0
            ))
        })?;
    Ok(CommittedTranscodeResult {
        commit_record_id: committed.commit_record_id,
        result_file_version_id,
        result_file_location_id,
        snapshot,
    })
}

async fn verify_staged_transcode(
    cp: &ControlPlane,
    artifact_handle_id: ArtifactHandleId,
    staging_root: &std::path::Path,
    verify: &dyn VerifyArtifactDispatcher,
) -> Result<crate::artifact::verify::VerifyArtifactReport, VoomError> {
    let verified = verify_artifact_with_dispatcher(
        cp,
        VerifyArtifactInput {
            artifact_handle_id,
            staging_root: staging_root.to_path_buf(),
        },
        verify,
        &NoVerifyArtifactHooks,
    )
    .await?;
    if verified.status != ArtifactVerificationStatus::Succeeded {
        return Err(VoomError::VerificationFailure(format!(
            "transcode artifact verification failed for {artifact_handle_id}"
        )));
    }
    Ok(verified)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
