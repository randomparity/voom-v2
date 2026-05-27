use std::path::PathBuf;

use async_trait::async_trait;
use serde::Serialize;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, JobId, LeaseId,
    MediaSnapshotId, TicketId, VoomError,
};
use voom_store::repo::artifacts::{ArtifactCommitState, ArtifactVerificationStatus};
use voom_worker_protocol::{ExtractAudioResult, TranscodeAudioResult};

use crate::ControlPlane;
use crate::artifact::commit::CommitArtifactInput;
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
pub struct ExecuteTranscodeAudioInput {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub operation_payload: serde_json::Value,
    pub staging_root: PathBuf,
    pub target_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ExecuteExtractAudioInput {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub source_bundle_id: voom_core::ids::BundleId,
    pub operation_payload: serde_json::Value,
    pub staging_root: PathBuf,
    pub target_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecuteTranscodeAudioReport {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecuteExtractAudioReport {
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
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
}

#[async_trait]
pub trait TranscodeAudioDispatcher: Send + Sync {
    async fn dispatch_transcode_audio(
        &self,
        request: voom_worker_protocol::TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError>;
}

#[async_trait]
pub trait ExtractAudioDispatcher: Send + Sync {
    async fn dispatch_extract_audio(
        &self,
        request: voom_worker_protocol::ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError>;
}

impl ControlPlane {
    pub async fn execute_transcode_audio(
        &self,
        input: ExecuteTranscodeAudioInput,
    ) -> Result<ExecuteTranscodeAudioReport, VoomError> {
        execute_transcode_audio_with_dispatchers(
            self,
            input,
            &dispatch::BundledTranscodeAudioDispatcher,
            &crate::artifact::verify::BundledVerifyArtifactDispatcher,
            &commit::BundledAudioResultProbeDispatcher,
        )
        .await
    }

    pub async fn execute_extract_audio(
        &self,
        input: ExecuteExtractAudioInput,
    ) -> Result<ExecuteExtractAudioReport, VoomError> {
        execute_extract_audio_with_dispatchers(
            self,
            input,
            &dispatch::BundledExtractAudioDispatcher,
            &crate::artifact::verify::BundledVerifyArtifactDispatcher,
        )
        .await
    }
}

pub(crate) async fn execute_transcode_audio_with_dispatchers(
    cp: &ControlPlane,
    input: ExecuteTranscodeAudioInput,
    transcode: &dyn TranscodeAudioDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::AudioResultProbeDispatcher,
) -> Result<ExecuteTranscodeAudioReport, VoomError> {
    let selected =
        source::select_source(cp, input.source_file_version_id, input.source_location_id).await?;
    let snapshot =
        source::read_media_snapshot(cp, input.source_file_version_id, &input.operation_payload)
            .await?;
    let selection = selection::transcode_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )?;
    let staging = stage::prepare_transcode_staging_path(
        &input.staging_root,
        input.ticket_id,
        input.lease_id,
        std::path::Path::new(&selected.location.value),
        &selection.target_codec,
    )
    .await?;
    let target_path = stage::transcode_target_path(
        &input.target_dir,
        std::path::Path::new(&selected.location.value),
        &selection.target_codec,
    )
    .await?;

    events::record_transcode_started(cp, &input, selected.location.id, &staging.path).await?;
    dispatch::revalidate_source_file(&selected).await?;
    let request = dispatch::transcode_request_for(
        &selected,
        &selection,
        &staging.canonical_root,
        &staging.path,
    )?;
    let result = transcode.dispatch_transcode_audio(request).await?;
    dispatch::validate_transcode_result(&selected, &selection, &result)?;
    dispatch::require_transcode_output_file_matches_result(&staging.path, &result).await?;
    let staged = commit::record_staged_audio_transcode(
        cp,
        &input,
        selected.location.id,
        &staging.path,
        &result,
    )
    .await?;
    commit_verified_transcode_audio(
        cp,
        TranscodeCommitRequest {
            input,
            source_location_id: selected.location.id,
            staged,
            staging_path: staging.path,
            target_path,
            result,
        },
        verify,
        result_probe,
    )
    .await
}

async fn commit_verified_transcode_audio(
    cp: &ControlPlane,
    request: TranscodeCommitRequest,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::AudioResultProbeDispatcher,
) -> Result<ExecuteTranscodeAudioReport, VoomError> {
    let verified = verify_artifact_with_dispatcher(
        cp,
        VerifyArtifactInput {
            artifact_handle_id: request.staged.artifact_handle_id,
        },
        verify,
        &NoVerifyArtifactHooks,
    )
    .await?;
    if verified.status != ArtifactVerificationStatus::Succeeded {
        return Err(VoomError::VerificationFailure(format!(
            "audio transcode artifact verification failed for {}",
            request.staged.artifact_handle_id
        )));
    }
    let committed = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: request.staged.artifact_handle_id,
            target_path: request.target_path.clone(),
        })
        .await
        .map_err(|err| VoomError::CommitFailure(err.to_string()))?;
    let result_file_version_id = committed.result_file_version_id.ok_or_else(|| {
        VoomError::Internal("committed audio transcode missing result_file_version_id".to_owned())
    })?;
    let result_file_location_id = committed.result_file_location_id.ok_or_else(|| {
        VoomError::Internal("committed audio transcode missing result_file_location_id".to_owned())
    })?;
    let result_snapshot = commit::record_transcode_result_snapshot_with_dispatcher(
        cp,
        result_file_version_id,
        &request.target_path,
        &request.result,
        result_probe,
    )
    .await?;
    events::record_transcode_succeeded(
        cp,
        &request.input,
        request.source_location_id,
        request.staged.artifact_handle_id,
        request.staged.artifact_location_id,
        &request.result,
    )
    .await?;
    Ok(ExecuteTranscodeAudioReport {
        job_id: request.input.job_id,
        ticket_id: request.input.ticket_id,
        lease_id: request.input.lease_id,
        source_file_version_id: request.input.source_file_version_id,
        source_file_location_id: request.source_location_id,
        staged_artifact_handle_id: request.staged.artifact_handle_id,
        staged_artifact_location_id: request.staged.artifact_location_id,
        verification_id: verified.verification_id,
        commit_record_id: committed.commit_record_id,
        result_file_version_id,
        result_file_location_id,
        result_media_snapshot_id: result_snapshot.id,
        staging_path: request.staging_path,
        target_path: request.target_path,
    })
}

struct TranscodeCommitRequest {
    input: ExecuteTranscodeAudioInput,
    source_location_id: FileLocationId,
    staged: commit::StagedAudioArtifact,
    staging_path: PathBuf,
    target_path: PathBuf,
    result: TranscodeAudioResult,
}

pub(crate) async fn execute_extract_audio_with_dispatchers(
    cp: &ControlPlane,
    input: ExecuteExtractAudioInput,
    extract: &dyn ExtractAudioDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let selected =
        source::select_source(cp, input.source_file_version_id, input.source_location_id).await?;
    let snapshot =
        source::read_media_snapshot(cp, input.source_file_version_id, &input.operation_payload)
            .await?;
    let selection = selection::extract_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )?;
    let staging = stage::prepare_extract_staging_path(
        &input.staging_root,
        input.ticket_id,
        input.lease_id,
        std::path::Path::new(&selected.location.value),
        &selection.stream.snapshot_stream_id,
        &selection.target_codec,
    )
    .await?;
    let target_path = stage::extract_target_path(
        &input.target_dir,
        std::path::Path::new(&selected.location.value),
        &selection.stream.snapshot_stream_id,
        &selection.target_codec,
    )
    .await?;

    events::record_extract_started(cp, &input, selected.location.id, &staging.path).await?;
    dispatch::revalidate_source_file(&selected).await?;
    let request = dispatch::extract_request_for(
        &selected,
        &selection,
        &staging.canonical_root,
        &staging.path,
    )?;
    let result = extract.dispatch_extract_audio(request).await?;
    dispatch::validate_extract_result(&selected, &selection, &result)?;
    dispatch::require_extract_output_file_matches_result(&staging.path, &result).await?;
    let staged = commit::record_staged_audio_extract(
        cp,
        &input,
        selected.location.id,
        &staging.path,
        &selection,
        &result,
    )
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
            "audio extraction artifact verification failed for {}",
            staged.artifact_handle_id
        )));
    }
    commit_verified_extract_audio(
        cp,
        ExtractCommitRequest {
            input,
            source_location_id: selected.location.id,
            staged,
            staging_path: staging.path,
            target_path,
            selection_role: selection.role,
            result,
            verification_id: verified.verification_id,
        },
    )
    .await
}

struct ExtractCommitRequest {
    input: ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    staged: commit::StagedAudioArtifact,
    staging_path: PathBuf,
    target_path: PathBuf,
    selection_role: voom_plan::audio::AudioBundleRole,
    result: ExtractAudioResult,
    verification_id: ArtifactVerificationId,
}

async fn commit_verified_extract_audio(
    cp: &ControlPlane,
    request: ExtractCommitRequest,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let committed = commit::commit_audio_extract_sidecar(
        cp,
        commit::CommitAudioExtractSidecarInput {
            artifact_handle_id: request.staged.artifact_handle_id,
            verification_id: request.verification_id,
            source_file_version_id: request.input.source_file_version_id,
            source_bundle_id: request.input.source_bundle_id,
            role: request.selection_role,
            staging_path: request.staging_path.clone(),
            target_path: request.target_path.clone(),
            output: request.result.output.clone(),
        },
    )
    .await?;
    ensure_extract_commit_succeeded(&committed)?;
    events::record_extract_succeeded(
        cp,
        &request.input,
        request.source_location_id,
        request.staged.artifact_handle_id,
        request.staged.artifact_location_id,
        &request.result,
    )
    .await?;
    Ok(ExecuteExtractAudioReport {
        job_id: request.input.job_id,
        ticket_id: request.input.ticket_id,
        lease_id: request.input.lease_id,
        source_file_version_id: request.input.source_file_version_id,
        source_file_location_id: request.source_location_id,
        staged_artifact_handle_id: request.staged.artifact_handle_id,
        staged_artifact_location_id: request.staged.artifact_location_id,
        verification_id: request.verification_id,
        commit_record_id: committed.commit_record_id,
        result_file_version_id: committed.result_file_version_id,
        result_file_location_id: committed.result_file_location_id,
        staging_path: request.staging_path,
        target_path: request.target_path,
    })
}

fn ensure_extract_commit_succeeded(
    report: &commit::CommitAudioExtractSidecarReport,
) -> Result<(), VoomError> {
    if let Some(recovery) = &report.recovery_required {
        return Err(VoomError::CommitFailure(format!(
            "audio extraction sidecar commit {} requires recovery: {} ({})",
            report.commit_record_id, recovery.message, recovery.error_code
        )));
    }
    if report.state != ArtifactCommitState::Committed {
        return Err(VoomError::CommitFailure(format!(
            "audio extraction sidecar commit {} ended in {:?}",
            report.commit_record_id, report.state
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
