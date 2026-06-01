use std::path::PathBuf;

use async_trait::async_trait;
use serde::Serialize;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, JobId, LeaseId,
    MediaSnapshotId, TicketId, VoomError,
};
use voom_events::payload::ArtifactAudioStreamPayload;
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
mod worker_contract;
pub(crate) mod workflow;

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
    pub commit_recovery_required: Option<TranscodePostCommitRecoveryReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscodePostCommitRecoveryReport {
    pub recovery_reason: String,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_media_snapshot_id: Option<MediaSnapshotId>,
    pub target_path: PathBuf,
    pub error_code: &'static str,
    pub message: String,
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
    pub commit_recovery_required: Option<commit::AudioExtractRecoveryReport>,
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
    /// Execute one policy-derived `transcode_audio` ticket through source
    /// revalidation, worker staging, verification, add-only commit, and result
    /// media-snapshot persistence.
    ///
    /// # Errors
    /// Returns stable `VoomError` variants for source selection, staging,
    /// worker, verification, commit, and result-probe failures.
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

    /// Execute one policy-derived `extract_audio` ticket through source
    /// revalidation, worker staging, verification, and add-only sidecar commit.
    ///
    /// # Errors
    /// Returns stable `VoomError` variants for source selection, staging,
    /// worker, verification, and commit failures.
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
    let failure_input = input.clone();
    let mut context = TranscodeAttemptContext::default();
    match execute_transcode_audio_inner(cp, input, transcode, verify, result_probe, &mut context)
        .await
    {
        Ok(report) => Ok(report),
        Err(err) => {
            events::record_transcode_failed(
                cp,
                events::TranscodeFailedEventInput {
                    input: &failure_input,
                    source_location_id: context.source_location_id,
                    source_media_snapshot_id: context
                        .source_media_snapshot_id
                        .or_else(|| audio_payload_snapshot_id(&failure_input.operation_payload)),
                    artifact_handle_id: context.artifact_handle_id,
                    artifact_location_id: context.artifact_location_id,
                    staging_path: context.staging_path.as_deref(),
                    selected_streams: context.selected_streams,
                    result: context.result.as_ref(),
                    error: &err,
                },
            )
            .await?;
            Err(err)
        }
    }
}

async fn execute_transcode_audio_inner(
    cp: &ControlPlane,
    input: ExecuteTranscodeAudioInput,
    transcode: &dyn TranscodeAudioDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::AudioResultProbeDispatcher,
    context: &mut TranscodeAttemptContext,
) -> Result<ExecuteTranscodeAudioReport, VoomError> {
    let selected =
        source::select_source(cp, input.source_file_version_id, input.source_location_id).await?;
    context.source_location_id = Some(selected.location.id);
    let snapshot =
        source::read_media_snapshot(cp, input.source_file_version_id, &input.operation_payload)
            .await?;
    context.source_media_snapshot_id = Some(snapshot.id.0);
    let selection = selection::transcode_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )?;
    context.selected_streams = events::stream_payloads(&selection.selection.selected_streams);
    let staging = stage::prepare_transcode_staging_path(
        &input.staging_root,
        input.ticket_id,
        input.lease_id,
        std::path::Path::new(&selected.location.value),
        &selection.target_codec,
    )
    .await?;
    context.staging_path = Some(staging.path.clone());
    let target_path = stage::transcode_target_path(
        &input.target_dir,
        std::path::Path::new(&selected.location.value),
        &selection.target_codec,
    )
    .await?;

    events::record_transcode_started(
        cp,
        &input,
        selected.location.id,
        snapshot.id.0,
        &staging.path,
        &selection,
    )
    .await?;
    worker_contract::revalidate_source_file(&selected).await?;
    let request = worker_contract::transcode_request_for(
        &selected,
        &selection,
        &staging.canonical_root,
        &staging.path,
    );
    let result = transcode.dispatch_transcode_audio(request).await?;
    context.result = Some(result.clone());
    worker_contract::validate_transcode_result(&selected, &selection, &result)?;
    worker_contract::require_transcode_output_file_matches_result(&staging.path, &result).await?;
    let staged = commit::record_staged_audio_transcode(
        cp,
        &input,
        selected.location.id,
        &staging.path,
        &result,
    )
    .await?;
    context.artifact_handle_id = Some(staged.artifact_handle_id);
    context.artifact_location_id = Some(staged.artifact_location_id);
    commit_verified_transcode_audio(
        cp,
        TranscodeCommitRequest {
            input,
            source_location_id: selected.location.id,
            source_media_snapshot_id: snapshot.id.0,
            staged,
            staging_path: staging.path,
            target_path,
            selected_streams: events::stream_payloads(&selection.selection.selected_streams),
            result,
        },
        verify,
        result_probe,
    )
    .await
}

#[derive(Debug, Default)]
struct TranscodeAttemptContext {
    source_location_id: Option<FileLocationId>,
    source_media_snapshot_id: Option<u64>,
    staging_path: Option<PathBuf>,
    selected_streams: Vec<ArtifactAudioStreamPayload>,
    artifact_handle_id: Option<ArtifactHandleId>,
    artifact_location_id: Option<ArtifactLocationId>,
    result: Option<TranscodeAudioResult>,
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
    // Probe the staged result before commit: the fallible external probe runs on
    // the content-hash-verified staged file (byte-identical to the add-only
    // committed target), so a probe failure leaves nothing committed and
    // propagates as Err (the caller records the failed event).
    let probed =
        commit::probe_staged_result(cp, &request.staging_path, &request.result, result_probe)
            .await?;
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
    // Only the local DB write remains after commit. On failure, keep the graceful
    // recovery report rather than returning Err: a committed artifact stays in
    // place and the caller's any-Err path would otherwise emit a misleading
    // transcode-failed event.
    let result_snapshot =
        match commit::record_result_snapshot_payload(cp, result_file_version_id, probed).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                return Ok(transcode_report_after_commit(
                    &request,
                    &verified,
                    committed.commit_record_id,
                    result_file_version_id,
                    result_file_location_id,
                    None,
                    Some(transcode_post_commit_recovery(
                        committed.commit_record_id,
                        result_file_version_id,
                        result_file_location_id,
                        None,
                        request.target_path.clone(),
                        &err,
                    )),
                ));
            }
        };
    if let Err(err) = events::record_transcode_succeeded(
        cp,
        events::TranscodeSucceededEventInput {
            input: &request.input,
            source_location_id: request.source_location_id,
            source_media_snapshot_id: request.source_media_snapshot_id,
            artifact_handle_id: request.staged.artifact_handle_id,
            artifact_location_id: request.staged.artifact_location_id,
            selected_streams: request.selected_streams.clone(),
            result: &request.result,
        },
    )
    .await
    {
        return Ok(transcode_report_after_commit(
            &request,
            &verified,
            committed.commit_record_id,
            result_file_version_id,
            result_file_location_id,
            Some(result_snapshot.id),
            Some(transcode_post_commit_recovery(
                committed.commit_record_id,
                result_file_version_id,
                result_file_location_id,
                Some(result_snapshot.id),
                request.target_path.clone(),
                &err,
            )),
        ));
    }
    Ok(transcode_report_after_commit(
        &request,
        &verified,
        committed.commit_record_id,
        result_file_version_id,
        result_file_location_id,
        Some(result_snapshot.id),
        None,
    ))
}

fn transcode_report_after_commit(
    request: &TranscodeCommitRequest,
    verified: &crate::artifact::verify::VerifyArtifactReport,
    commit_record_id: ArtifactCommitRecordId,
    result_file_version_id: FileVersionId,
    result_file_location_id: FileLocationId,
    result_media_snapshot_id: Option<MediaSnapshotId>,
    recovery: Option<TranscodePostCommitRecoveryReport>,
) -> ExecuteTranscodeAudioReport {
    ExecuteTranscodeAudioReport {
        job_id: request.input.job_id,
        ticket_id: request.input.ticket_id,
        lease_id: request.input.lease_id,
        source_file_version_id: request.input.source_file_version_id,
        source_file_location_id: request.source_location_id,
        staged_artifact_handle_id: request.staged.artifact_handle_id,
        staged_artifact_location_id: request.staged.artifact_location_id,
        verification_id: verified.verification_id,
        commit_record_id,
        result_file_version_id,
        result_file_location_id,
        result_media_snapshot_id: result_media_snapshot_id.unwrap_or(MediaSnapshotId(0)),
        staging_path: request.staging_path.clone(),
        target_path: request.target_path.clone(),
        commit_recovery_required: recovery,
    }
}

fn transcode_post_commit_recovery(
    commit_record_id: ArtifactCommitRecordId,
    result_file_version_id: FileVersionId,
    result_file_location_id: FileLocationId,
    result_media_snapshot_id: Option<MediaSnapshotId>,
    target_path: PathBuf,
    err: &VoomError,
) -> TranscodePostCommitRecoveryReport {
    TranscodePostCommitRecoveryReport {
        recovery_reason: "audio transcode post-commit reporting failed".to_owned(),
        commit_record_id,
        result_file_version_id,
        result_file_location_id,
        result_media_snapshot_id,
        target_path,
        error_code: err.error_code().as_str(),
        message: err.to_string(),
    }
}

struct TranscodeCommitRequest {
    input: ExecuteTranscodeAudioInput,
    source_location_id: FileLocationId,
    source_media_snapshot_id: u64,
    staged: commit::StagedAudioArtifact,
    staging_path: PathBuf,
    target_path: PathBuf,
    selected_streams: Vec<ArtifactAudioStreamPayload>,
    result: TranscodeAudioResult,
}

pub(crate) async fn execute_extract_audio_with_dispatchers(
    cp: &ControlPlane,
    input: ExecuteExtractAudioInput,
    extract: &dyn ExtractAudioDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let failure_input = input.clone();
    let mut context = ExtractAttemptContext::default();
    match execute_extract_audio_inner(cp, input, extract, verify, &mut context).await {
        Ok(report) => Ok(report),
        Err(err) => {
            events::record_extract_failed(
                cp,
                events::ExtractFailedEventInput {
                    input: &failure_input,
                    source_location_id: context.source_location_id,
                    source_media_snapshot_id: context
                        .source_media_snapshot_id
                        .or_else(|| audio_payload_snapshot_id(&failure_input.operation_payload)),
                    selection: context.selection.as_ref(),
                    staging_path: context.staging_path.as_deref(),
                    artifact_handle_id: context.artifact_handle_id,
                    artifact_location_id: context.artifact_location_id,
                    result: context.result.as_ref(),
                    error: &err,
                },
            )
            .await?;
            Err(err)
        }
    }
}

async fn execute_extract_audio_inner(
    cp: &ControlPlane,
    input: ExecuteExtractAudioInput,
    extract: &dyn ExtractAudioDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
    context: &mut ExtractAttemptContext,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let selected =
        source::select_source(cp, input.source_file_version_id, input.source_location_id).await?;
    context.source_location_id = Some(selected.location.id);
    let snapshot =
        source::read_media_snapshot(cp, input.source_file_version_id, &input.operation_payload)
            .await?;
    context.source_media_snapshot_id = Some(snapshot.id.0);
    let selection = selection::extract_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )?;
    context.selection = Some(selection.clone());
    let staging = stage::prepare_extract_staging_path(
        &input.staging_root,
        input.ticket_id,
        input.lease_id,
        std::path::Path::new(&selected.location.value),
        &selection.stream.snapshot_stream_id,
        &selection.target_codec,
    )
    .await?;
    context.staging_path = Some(staging.path.clone());
    let target_path = stage::extract_target_path(
        &input.target_dir,
        std::path::Path::new(&selected.location.value),
        &selection.stream.snapshot_stream_id,
        &selection.target_codec,
    )
    .await?;

    events::record_extract_started(
        cp,
        &input,
        selected.location.id,
        snapshot.id.0,
        &staging.path,
        &selection,
    )
    .await?;
    worker_contract::revalidate_source_file(&selected).await?;
    let request = worker_contract::extract_request_for(
        &selected,
        &selection,
        &staging.canonical_root,
        &staging.path,
    );
    let result = extract.dispatch_extract_audio(request).await?;
    context.result = Some(result.clone());
    worker_contract::validate_extract_result(&selected, &selection, &result)?;
    worker_contract::require_extract_output_file_matches_result(&staging.path, &result).await?;
    let staged = commit::record_staged_audio_extract(
        cp,
        &input,
        selected.location.id,
        &staging.path,
        &selection,
        &result,
    )
    .await?;
    context.artifact_handle_id = Some(staged.artifact_handle_id);
    context.artifact_location_id = Some(staged.artifact_location_id);
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
            source_media_snapshot_id: snapshot.id.0,
            staged,
            staging_path: staging.path,
            target_path,
            selection,
            result,
            verification_id: verified.verification_id,
        },
    )
    .await
}

#[derive(Debug, Default)]
struct ExtractAttemptContext {
    source_location_id: Option<FileLocationId>,
    source_media_snapshot_id: Option<u64>,
    staging_path: Option<PathBuf>,
    selection: Option<selection::ExtractAudioSelectionPlan>,
    artifact_handle_id: Option<ArtifactHandleId>,
    artifact_location_id: Option<ArtifactLocationId>,
    result: Option<ExtractAudioResult>,
}

struct ExtractCommitRequest {
    input: ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    source_media_snapshot_id: u64,
    staged: commit::StagedAudioArtifact,
    staging_path: PathBuf,
    target_path: PathBuf,
    selection: selection::ExtractAudioSelectionPlan,
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
            role: request.selection.role,
            staging_path: request.staging_path.clone(),
            target_path: request.target_path.clone(),
            output: request.result.output.clone(),
        },
    )
    .await?;
    ensure_extract_commit_succeeded(&committed)?;
    // `finalize_sidecar_commit` is the only producer of a Committed,
    // recovery-free report, and it always sets Some IDs. These guards are
    // defense-in-depth: a future Committed-with-None path fails loud here
    // instead of emitting a sentinel zero ID downstream.
    let result_file_version_id = committed.result_file_version_id.ok_or_else(|| {
        VoomError::Internal("committed audio extract missing result_file_version_id".to_owned())
    })?;
    let result_file_location_id = committed.result_file_location_id.ok_or_else(|| {
        VoomError::Internal("committed audio extract missing result_file_location_id".to_owned())
    })?;
    let commit_recovery_required = match events::record_extract_succeeded(
        cp,
        events::ExtractSucceededEventInput {
            input: &request.input,
            source_location_id: request.source_location_id,
            source_media_snapshot_id: request.source_media_snapshot_id,
            artifact_handle_id: request.staged.artifact_handle_id,
            artifact_location_id: request.staged.artifact_location_id,
            selection: &request.selection,
            result: &request.result,
        },
    )
    .await
    {
        Ok(()) => committed.recovery_required.clone(),
        Err(err) => Some(
            commit::extract_post_commit_recovery(
                &committed,
                request.input.source_bundle_id,
                request.selection.role,
                &request.staging_path,
                &err,
            )
            .await,
        ),
    };
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
        result_file_version_id,
        result_file_location_id,
        staging_path: request.staging_path,
        target_path: request.target_path,
        commit_recovery_required,
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

fn audio_payload_snapshot_id(payload: &serde_json::Value) -> Option<u64> {
    payload
        .get("source_media_snapshot_id")
        .and_then(serde_json::Value::as_u64)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
