use std::path::PathBuf;

use async_trait::async_trait;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, JobId, LeaseId,
    MediaSnapshotId, TicketId, VoomError,
};
use voom_events::payload::{ArtifactAudioExtractQuiescedPayload, ArtifactAudioStreamPayload};
use voom_events::{Event, SubjectType};
#[cfg(test)]
use voom_store::repo::artifacts::ArtifactCommitState;
use voom_store::repo::artifacts::ArtifactVerificationStatus;
use voom_store::repo::media::audio_extract_operations::{
    AudioExtractDispatchAttemptStatus, AudioExtractOperationRecord, AudioExtractOperationState,
    AudioExtractQuiescenceAcknowledgement, NewAudioExtractClaim, NewAudioExtractDispatchAttempt,
    NewAudioExtractOperation, NewAudioExtractOutput, SqliteAudioExtractOperationRepo,
};
use voom_worker_protocol::{AudioObservedFacts, ExtractAudioResult, TranscodeAudioResult};

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
    /// Opt-in backup-before-mutation destination root; `Some` backs up the
    /// source before dispatch (ADR 0025).
    pub backup_root: Option<PathBuf>,
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
    /// Opt-in backup-before-mutation destination root; `Some` backs up the
    /// source before dispatch (ADR 0025).
    pub backup_root: Option<PathBuf>,
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
    pub outputs: Vec<ExecuteExtractAudioOutputReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecuteExtractAudioOutputReport {
    pub operation_output_id: u64,
    pub output_id: Option<String>,
    pub source_file_version_id: FileVersionId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub role: String,
    pub staged_artifact_handle_id: ArtifactHandleId,
    pub staged_artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_file_asset_id: u64,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub bundle_member_id: u64,
    pub lineage_id: u64,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcknowledgeExtractDispatchQuiescenceInput {
    pub operation_key: String,
    pub generation: u32,
    pub attempt_id: u64,
    pub worker_id: voom_core::WorkerId,
    pub worker_epoch: u32,
    pub idempotency_key: String,
    pub acknowledged_by: String,
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
        idempotency_key: &str,
        request: voom_worker_protocol::ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError>;
}

impl ControlPlane {
    /// Records operator proof that one quarantined extraction worker generation
    /// can no longer write its persisted attempt paths.
    ///
    /// # Errors
    /// Returns `Conflict` unless the exact planned generation is quarantined
    /// and its writer claim has expired.
    pub async fn acknowledge_extract_dispatch_quiescence(
        &self,
        input: AcknowledgeExtractDispatchQuiescenceInput,
    ) -> Result<(), VoomError> {
        let now = self.clock().now();
        let acknowledgement = AudioExtractQuiescenceAcknowledgement {
            operation_key: input.operation_key,
            generation: input.generation,
            attempt_id: input.attempt_id,
            worker_id: input.worker_id,
            worker_epoch: input.worker_epoch,
            idempotency_key: input.idempotency_key,
            acknowledged_by: input.acknowledged_by,
        };
        let mut tx = crate::cases::begin_immediate_tx(&self.pool).await?;
        SqliteAudioExtractOperationRepo::acknowledge_quiescence_in_tx(
            &mut tx,
            &acknowledgement,
            now,
        )
        .await?;
        crate::cases::append_event(
            &self.events,
            &mut tx,
            SubjectType::System,
            None,
            now,
            Event::ArtifactAudioExtractQuiesced(ArtifactAudioExtractQuiescedPayload {
                operation_key: acknowledgement.operation_key,
                generation: acknowledgement.generation,
                attempt_id: acknowledgement.attempt_id,
                worker_id: acknowledgement.worker_id.0,
                worker_epoch: acknowledgement.worker_epoch,
                idempotency_key: acknowledgement.idempotency_key,
                acknowledged_by: acknowledgement.acknowledged_by,
                acknowledged_at: now,
            }),
        )
        .await?;
        crate::cases::commit_tx(tx).await
    }

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
            &commit::BundledAudioResultProbeDispatcher,
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
    crate::backup::maybe_back_up_source(
        cp,
        input.backup_root.as_deref(),
        &selected.canonical_path,
        input.source_file_version_id,
        input.job_id,
        input.ticket_id,
    )
    .await?;
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
    let request = worker_contract::transcode_audio_request_for(
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
        VerifyArtifactInput::for_staged_file(
            request.staged.artifact_handle_id,
            &request.staging_path,
        ),
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
    result_probe: &dyn commit::AudioResultProbeDispatcher,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let failure_input = input.clone();
    let mut context = ExtractAttemptContext::default();
    match execute_extract_audio_inner(cp, input, extract, verify, result_probe, &mut context).await
    {
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
                    outputs: &context.outputs,
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
    result_probe: &dyn commit::AudioResultProbeDispatcher,
    context: &mut ExtractAttemptContext,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let prepared = prepare_extract_execution(cp, &input, context).await?;
    let selected = prepared.selected;
    let snapshot = prepared.snapshot;
    let selection = prepared.selection;
    let paths = prepared.paths;
    if let Some(report) = maybe_resume_extract_operation(
        cp,
        &input,
        selected.location.id,
        &selection,
        &paths.operation,
        verify,
        result_probe,
    )
    .await?
    {
        return Ok(report);
    }
    Box::pin(execute_new_extract_attempt(
        ExtractExecutionDependencies {
            cp,
            extract,
            verify,
            result_probe,
        },
        input,
        prepared_from_parts(selected, snapshot, selection, paths),
        context,
    ))
    .await
}

fn prepared_from_parts(
    selected: source::SelectedSource,
    snapshot: voom_store::repo::identity::MediaSnapshot,
    selection: selection::ExtractAudioSelectionPlan,
    paths: ExtractExecutionPaths,
) -> PreparedExtractExecution {
    PreparedExtractExecution {
        selected,
        snapshot,
        selection,
        paths,
    }
}

struct ExtractExecutionDependencies<'a> {
    cp: &'a ControlPlane,
    extract: &'a dyn ExtractAudioDispatcher,
    verify: &'a dyn VerifyArtifactDispatcher,
    result_probe: &'a dyn commit::AudioResultProbeDispatcher,
}

async fn execute_new_extract_attempt(
    dependencies: ExtractExecutionDependencies<'_>,
    input: ExecuteExtractAudioInput,
    prepared: PreparedExtractExecution,
    context: &mut ExtractAttemptContext,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let ExtractExecutionDependencies {
        cp,
        extract,
        verify,
        result_probe,
    } = dependencies;
    let PreparedExtractExecution {
        selected,
        snapshot,
        selection,
        paths,
    } = prepared;
    crate::backup::maybe_back_up_source(
        cp,
        input.backup_root.as_deref(),
        &selected.canonical_path,
        input.source_file_version_id,
        input.job_id,
        input.ticket_id,
    )
    .await?;
    let dispatch = claim_extract_dispatch(cp, &input, &paths.operation, &paths.targets).await?;
    let staging = &dispatch.staging;
    context.outputs = events::extract_member_payloads(&selection, &staging.paths, &paths.targets);
    let staging_path = staging.paths.first().ok_or_else(|| {
        VoomError::Internal("audio extraction produced an empty staging path set".to_owned())
    })?;
    context.staging_path = Some(staging_path.clone());

    events::record_extract_started(
        cp,
        &input,
        selected.location.id,
        snapshot.id.0,
        staging_path,
        &selection,
        &context.outputs,
    )
    .await?;
    worker_contract::revalidate_source_file(&selected).await?;
    let request = worker_contract::extract_audio_request_for(
        &selected,
        &selection,
        &staging.canonical_root,
        &staging.paths,
    )?;
    let (result, attempt) =
        dispatch_extract_worker(cp, extract, &dispatch, staging, request.clone()).await?;
    let repo = SqliteAudioExtractOperationRepo::new(cp.pool.clone());
    context.result = Some(result.clone());
    if let Err(error) = validate_and_cleanup_extract_result(
        &selected,
        &selection,
        &request,
        &staging.paths,
        &result,
    )
    .await
    {
        repo.advance_terminal_generation(&dispatch.claim, attempt.id, cp.clock().now())
            .await?;
        return Err(error);
    }
    let staged = commit::record_staged_audio_extract_set(
        cp,
        commit::StageAudioExtractSetInput {
            execution: &input,
            source_file_location_id: selected.location.id,
            staging_paths: &staging.paths,
            operation: &paths.operation,
            selection: &selection,
            result: &result,
            claim: &dispatch.claim,
        },
    )
    .await?;
    hydrate_extract_artifact_context(context, &staged);
    let verification_ids = verify_staged_extract_set(cp, &staged, &staging.paths, verify).await?;
    let probed = probe_staged_extract_set(cp, &staging.paths, &result, result_probe).await?;
    commit_verified_extract_audio(
        cp,
        ExtractCommitRequest {
            input,
            source_location_id: selected.location.id,
            source_media_snapshot_id: snapshot.id.0,
            staged,
            staging_paths: staging.paths.clone(),
            target_paths: paths.targets,
            operation: paths.operation,
            selection,
            result,
            verification_ids,
            probed,
        },
    )
    .await
}

fn hydrate_extract_artifact_context(
    context: &mut ExtractAttemptContext,
    staged: &[commit::StagedAudioArtifact],
) {
    context.artifact_handle_id = staged.first().map(|item| item.artifact_handle_id);
    context.artifact_location_id = staged.first().map(|item| item.artifact_location_id);
    for (member, artifact) in context.outputs.iter_mut().zip(staged) {
        member.artifact_handle_id = Some(artifact.artifact_handle_id.0);
        member.artifact_location_id = Some(artifact.artifact_location_id.0);
    }
}

async fn dispatch_extract_worker(
    cp: &ControlPlane,
    extract: &dyn ExtractAudioDispatcher,
    dispatch: &ClaimedExtractDispatch,
    staging: &stage::PreparedStagingPaths,
    request: voom_worker_protocol::ExtractAudioRequest,
) -> Result<
    (
        ExtractAudioResult,
        voom_store::repo::audio_extract_operations::AudioExtractDispatchAttempt,
    ),
    VoomError,
> {
    let repo = SqliteAudioExtractOperationRepo::new(cp.pool.clone());
    let attempt = repo
        .record_dispatch_attempt(
            &dispatch.claim,
            NewAudioExtractDispatchAttempt {
                worker_id: dispatch.worker_id,
                worker_epoch: dispatch.worker_epoch,
                idempotency_key: dispatch.idempotency_key.clone(),
                attempt_directory: staging.canonical_root.display().to_string(),
                paths: staging
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            },
            cp.clock().now(),
        )
        .await?;
    match extract
        .dispatch_extract_audio(&dispatch.idempotency_key, request)
        .await
    {
        Ok(result) => {
            repo.mark_dispatch_terminal(&dispatch.claim, attempt.id, cp.clock().now())
                .await?;
            Ok((result, attempt))
        }
        Err(error) => {
            repo.quarantine_dispatch(&dispatch.claim, attempt.id, cp.clock().now())
                .await?;
            Err(VoomError::WorkerCrash(format!(
                "{error}; audio extraction attempt {} is quarantined because worker \
                 quiescence is not proven (worker {} epoch {}, key {})",
                attempt.id, attempt.worker_id.0, attempt.worker_epoch, attempt.idempotency_key
            )))
        }
    }
}

async fn validate_and_cleanup_extract_result(
    selected: &source::SelectedSource,
    selection: &selection::ExtractAudioSelectionPlan,
    request: &voom_worker_protocol::ExtractAudioRequest,
    staging_paths: &[PathBuf],
    result: &ExtractAudioResult,
) -> Result<(), VoomError> {
    let Err(error) =
        validate_extract_worker_result(selected, selection, request, staging_paths, result).await
    else {
        return Ok(());
    };
    cleanup_terminal_extract_outputs(staging_paths)
        .await
        .map_err(|cleanup| {
            VoomError::CommitFailure(format!(
                "audio extraction result was invalid ({error}); \
                 cleaning its terminal staging files failed: {cleanup}"
            ))
        })?;
    Err(error)
}

struct PreparedExtractExecution {
    selected: source::SelectedSource,
    snapshot: voom_store::repo::identity::MediaSnapshot,
    selection: selection::ExtractAudioSelectionPlan,
    paths: ExtractExecutionPaths,
}

async fn prepare_extract_execution(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    context: &mut ExtractAttemptContext,
) -> Result<PreparedExtractExecution, VoomError> {
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
    let paths = prepare_extract_paths(
        cp,
        input,
        std::path::Path::new(&selected.location.value),
        snapshot.id.0,
        &selection,
    )
    .await?;
    context.outputs = extract_attempt_members(&selection, &paths);
    Ok(PreparedExtractExecution {
        selected,
        snapshot,
        selection,
        paths,
    })
}

async fn maybe_resume_extract_operation(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    selection: &selection::ExtractAudioSelectionPlan,
    operation: &AudioExtractOperationRecord,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::AudioResultProbeDispatcher,
) -> Result<Option<ExecuteExtractAudioReport>, VoomError> {
    match operation.operation.state {
        AudioExtractOperationState::Committed => {
            committed_extract_report(input, source_location_id, selection, operation).map(Some)
        }
        AudioExtractOperationState::Prepared | AudioExtractOperationState::RecoveryRequired => {
            recover_extract_report(cp, input, source_location_id, selection, operation)
                .await
                .map(Some)
        }
        AudioExtractOperationState::Planned => Ok(None),
        AudioExtractOperationState::Staged => resume_staged_extract_report(
            cp,
            input,
            source_location_id,
            selection,
            operation,
            verify,
            result_probe,
        )
        .await
        .map(Some),
    }
}

async fn resume_staged_extract_report(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    selection: &selection::ExtractAudioSelectionPlan,
    operation: &AudioExtractOperationRecord,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::AudioResultProbeDispatcher,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    if operation.outputs.len() != selection.outputs.len() {
        return Err(VoomError::Internal(format!(
            "staged audio extraction {} has an incomplete output set",
            operation.operation.operation_key
        )));
    }
    let missing = |output_id: u64, field: &str| {
        VoomError::Internal(format!(
            "staged audio extraction output {output_id} is missing {field}"
        ))
    };
    let mut staged = Vec::with_capacity(operation.outputs.len());
    let mut staging_paths = Vec::with_capacity(operation.outputs.len());
    for output in &operation.outputs {
        staged.push(commit::StagedAudioArtifact {
            artifact_handle_id: output
                .artifact_handle_id
                .ok_or_else(|| missing(output.id, "artifact_handle_id"))?,
            artifact_location_id: output
                .artifact_location_id
                .ok_or_else(|| missing(output.id, "artifact_location_id"))?,
        });
        staging_paths.push(PathBuf::from(
            output
                .staging_path
                .as_ref()
                .ok_or_else(|| missing(output.id, "staging_path"))?,
        ));
    }
    let result: ExtractAudioResult =
        serde_json::from_value(operation.operation.worker_result.clone().ok_or_else(|| {
            VoomError::Internal(format!(
                "staged audio extraction {} is missing worker_result",
                operation.operation.operation_key
            ))
        })?)
        .map_err(|error| {
            VoomError::database(format!(
                "staged audio extraction worker_result is malformed: {error}"
            ))
        })?;
    let verification_ids = verify_staged_extract_set(cp, &staged, &staging_paths, verify).await?;
    let probed = probe_staged_extract_set(cp, &staging_paths, &result, result_probe).await?;
    commit_verified_extract_audio(
        cp,
        ExtractCommitRequest {
            input: input.clone(),
            source_location_id,
            source_media_snapshot_id: operation.operation.source_media_snapshot_id.0,
            staged,
            staging_paths,
            target_paths: operation
                .outputs
                .iter()
                .map(|output| PathBuf::from(&output.target_path))
                .collect(),
            operation: operation.clone(),
            selection: selection.clone(),
            result,
            verification_ids,
            probed,
        },
    )
    .await
}

struct ExtractExecutionPaths {
    staging: Option<stage::PreparedStagingPaths>,
    targets: Vec<PathBuf>,
    operation: AudioExtractOperationRecord,
}

struct ClaimedExtractDispatch {
    claim: NewAudioExtractClaim,
    worker_id: voom_core::WorkerId,
    worker_epoch: u32,
    idempotency_key: String,
    staging: stage::PreparedStagingPaths,
}

fn extract_attempt_members(
    selection: &selection::ExtractAudioSelectionPlan,
    paths: &ExtractExecutionPaths,
) -> Vec<voom_events::payload::ArtifactAudioExtractMemberPayload> {
    let staging_paths = if let Some(staging) = &paths.staging {
        staging.paths.clone()
    } else {
        paths
            .operation
            .outputs
            .iter()
            .filter_map(|output| output.staging_path.as_ref().map(PathBuf::from))
            .collect()
    };
    let mut members = events::extract_member_payloads(selection, &staging_paths, &paths.targets);
    for (member, output) in members.iter_mut().zip(&paths.operation.outputs) {
        member.artifact_handle_id = output.artifact_handle_id.map(|id| id.0);
        member.artifact_location_id = output.artifact_location_id.map(|id| id.0);
    }
    members
}

async fn prepare_extract_paths(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    source_path: &std::path::Path,
    source_media_snapshot_id: u64,
    selection: &selection::ExtractAudioSelectionPlan,
) -> Result<ExtractExecutionPaths, VoomError> {
    let targets = stage::extract_target_paths(&input.target_dir, source_path, selection).await?;
    let operation_token = extract_operation_token(
        input.source_file_version_id,
        source_media_snapshot_id,
        selection.operation_id.as_deref(),
        &targets,
    );
    let outputs = selection
        .outputs
        .iter()
        .zip(&targets)
        .map(|(output, target)| NewAudioExtractOutput {
            output_id: output.output_id.clone(),
            source_snapshot_stream_id: output.stream.snapshot_stream_id.clone(),
            source_provider_stream_index: output.stream.provider_stream_index,
            bundle_role: extract_role_name(output.role).to_owned(),
            target_path: target.display().to_string(),
        })
        .collect::<Vec<_>>();
    let operation = SqliteAudioExtractOperationRepo::new(cp.pool.clone())
        .create_planned(
            NewAudioExtractOperation {
                operation_key: operation_token.clone(),
                operation_id: selection.operation_id.clone(),
                target_set_hash: operation_token.clone(),
                source_file_version_id: input.source_file_version_id,
                source_bundle_id: input.source_bundle_id,
                source_media_snapshot_id: MediaSnapshotId(source_media_snapshot_id),
            },
            &outputs,
            cp.clock().now(),
        )
        .await?;
    Ok(ExtractExecutionPaths {
        staging: None,
        targets,
        operation,
    })
}

async fn claim_extract_dispatch(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    operation: &AudioExtractOperationRecord,
    targets: &[PathBuf],
) -> Result<ClaimedExtractDispatch, VoomError> {
    let now = cp.clock().now();
    let (worker_id, worker_epoch, expires_at) = extract_dispatch_lease(cp, input.lease_id).await?;
    let claim = NewAudioExtractClaim {
        operation_key: operation.operation.operation_key.clone(),
        expected_generation: operation.operation.dispatch_generation,
        lease_id: input.lease_id,
        claim_token: format!(
            "lease-{}-{}",
            input.lease_id.0,
            crate::worker_process::random_hex_128()
        ),
        expires_at,
    };
    let repo = SqliteAudioExtractOperationRepo::new(cp.pool.clone());
    repo.acquire_claim(&claim, now).await?;
    if let Some(attempt) = repo
        .get_dispatch_attempt(operation.operation.id, claim.expected_generation)
        .await?
    {
        return reconcile_prior_dispatch_attempt(&repo, &claim, attempt, now).await;
    }
    let staging = stage::prepare_extract_staging_paths(
        &input.staging_root,
        &operation.operation.operation_key,
        operation.operation.dispatch_generation,
        targets,
    )
    .await?;
    Ok(ClaimedExtractDispatch {
        idempotency_key: format!(
            "audio-extract:{}:{}",
            operation.operation.operation_key, operation.operation.dispatch_generation
        ),
        claim,
        worker_id,
        worker_epoch,
        staging,
    })
}

async fn extract_dispatch_lease(
    cp: &ControlPlane,
    lease_id: LeaseId,
) -> Result<(voom_core::WorkerId, u32, time::OffsetDateTime), VoomError> {
    let row = sqlx::query(
        "SELECT leases.expires_at, workers.id AS worker_id, workers.epoch AS worker_epoch \
         FROM leases JOIN workers ON workers.id = leases.worker_id \
         WHERE leases.id = ? AND leases.state = 'held'",
    )
    .bind(i64::try_from(lease_id.0).map_err(|error| {
        VoomError::Config(format!("audio extraction lease id is invalid: {error}"))
    })?)
    .fetch_optional(&cp.pool)
    .await
    .map_err(|error| VoomError::database_context("audio extraction dispatch lease", error))?
    .ok_or_else(|| {
        VoomError::Conflict(format!("audio extraction lease {} is not held", lease_id.0))
    })?;
    let expires_at: String = row.try_get("expires_at").map_err(|error| {
        VoomError::database_context("audio extraction lease expiry decode", error)
    })?;
    let expires_at =
        time::OffsetDateTime::parse(&expires_at, &time::format_description::well_known::Rfc3339)
            .map_err(|error| {
                VoomError::database(format!("audio extraction lease expiry: {error}"))
            })?;
    let worker_epoch: i64 = row.try_get("worker_epoch").map_err(|error| {
        VoomError::database_context("audio extraction worker epoch decode", error)
    })?;
    Ok((
        voom_core::WorkerId(
            u64::try_from(row.try_get::<i64, _>("worker_id").map_err(|error| {
                VoomError::database_context("audio extraction worker id decode", error)
            })?)
            .map_err(|error| VoomError::database(format!("audio extraction worker id: {error}")))?,
        ),
        u32::try_from(worker_epoch)
            .map_err(|error| VoomError::database(format!("audio worker epoch: {error}")))?,
        expires_at,
    ))
}

async fn reconcile_prior_dispatch_attempt(
    repo: &SqliteAudioExtractOperationRepo,
    claim: &NewAudioExtractClaim,
    attempt: voom_store::repo::media::audio_extract_operations::AudioExtractDispatchAttempt,
    now: time::OffsetDateTime,
) -> Result<ClaimedExtractDispatch, VoomError> {
    match attempt.status {
        AudioExtractDispatchAttemptStatus::Terminal
        | AudioExtractDispatchAttemptStatus::Quiesced => {
            cleanup_recorded_dispatch_paths(&attempt).await?;
            repo.advance_terminal_generation(claim, attempt.id, now)
                .await?;
            Err(VoomError::Conflict(format!(
                "audio extraction attempt {} was cleaned and advanced; retry the operation",
                attempt.id
            )))
        }
        AudioExtractDispatchAttemptStatus::Active
        | AudioExtractDispatchAttemptStatus::Quarantined => Err(VoomError::Conflict(format!(
            "audio extraction attempt {} is {:?}; worker {} epoch {} must prove terminal \
             completion or be explicitly quiesced before retry (key {})",
            attempt.id,
            attempt.status,
            attempt.worker_id.0,
            attempt.worker_epoch,
            attempt.idempotency_key
        ))),
    }
}

async fn cleanup_recorded_dispatch_paths(
    attempt: &voom_store::repo::media::audio_extract_operations::AudioExtractDispatchAttempt,
) -> Result<(), VoomError> {
    let attempt_directory = std::path::Path::new(&attempt.attempt_directory);
    for value in &attempt.paths {
        let path = std::path::Path::new(value);
        if path.parent() != Some(attempt_directory) {
            return Err(VoomError::Conflict(format!(
                "audio extraction attempt {} path {} is not an immediate child of {}",
                attempt.id,
                path.display(),
                attempt_directory.display()
            )));
        }
        match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) if metadata.file_type().is_file() => {
                tokio::fs::remove_file(path).await.map_err(|error| {
                    VoomError::CommitFailure(format!(
                        "remove quiesced audio extraction path {}: {error}",
                        path.display()
                    ))
                })?;
            }
            Ok(_) => {
                return Err(VoomError::Conflict(format!(
                    "audio extraction attempt {} path {} is not a regular file",
                    attempt.id,
                    path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(VoomError::CommitFailure(format!(
                    "inspect quiesced audio extraction path {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn extract_role_name(role: voom_plan::audio::AudioBundleRole) -> &'static str {
    match role {
        voom_plan::audio::AudioBundleRole::CommentaryAudio => "commentary_audio",
        voom_plan::audio::AudioBundleRole::ExternalAudio => "external_audio",
    }
}

fn committed_extract_report(
    input: &ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    selection: &selection::ExtractAudioSelectionPlan,
    operation: &AudioExtractOperationRecord,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let outputs = operation
        .outputs
        .iter()
        .zip(&selection.outputs)
        .map(|(stored, selected)| {
            committed_output_report(
                stored,
                selected,
                operation.operation.source_file_version_id,
                operation.operation.source_media_snapshot_id,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    if outputs.len() != selection.outputs.len() {
        return Err(VoomError::Internal(format!(
            "committed audio extraction {} has an incomplete output set",
            operation.operation.operation_key
        )));
    }
    let first = outputs.first().ok_or_else(|| {
        VoomError::Internal(format!(
            "committed audio extraction {} has no outputs",
            operation.operation.operation_key
        ))
    })?;
    Ok(ExecuteExtractAudioReport {
        job_id: input.job_id,
        ticket_id: input.ticket_id,
        lease_id: input.lease_id,
        source_file_version_id: input.source_file_version_id,
        source_file_location_id: source_location_id,
        staged_artifact_handle_id: first.staged_artifact_handle_id,
        staged_artifact_location_id: first.staged_artifact_location_id,
        verification_id: first.verification_id,
        commit_record_id: first.commit_record_id,
        result_file_version_id: first.result_file_version_id,
        result_file_location_id: first.result_file_location_id,
        staging_path: first.staging_path.clone(),
        target_path: first.target_path.clone(),
        commit_recovery_required: None,
        outputs,
    })
}

async fn recover_extract_report(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    selection: &selection::ExtractAudioSelectionPlan,
    operation: &AudioExtractOperationRecord,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    if operation.outputs.len() != selection.outputs.len() {
        return Err(VoomError::Internal(format!(
            "recoverable audio extraction {} has an incomplete output set",
            operation.operation.operation_key
        )));
    }
    let outputs = operation
        .outputs
        .iter()
        .zip(&selection.outputs)
        .map(|(stored, selected)| recovery_output_input(stored, selected))
        .collect::<Result<Vec<_>, _>>()?;
    let committed = commit::recover_audio_extract_set(
        cp,
        &commit::CommitAudioExtractSetInput {
            operation_row_id: operation.operation.id,
            source_file_version_id: input.source_file_version_id,
            source_media_snapshot_id: operation.operation.source_media_snapshot_id,
            source_bundle_id: input.source_bundle_id,
            outputs,
        },
    )
    .await?;
    let outputs = extract_output_reports(
        selection,
        &committed,
        input.source_file_version_id,
        operation.operation.source_media_snapshot_id,
    )?;
    execution_report_from_outputs(input, source_location_id, outputs)
}

fn recovery_output_input(
    stored: &voom_store::repo::audio_extract_operations::AudioExtractOperationOutput,
    selected: &selection::ExtractAudioSelectionOutput,
) -> Result<commit::CommitAudioExtractOutputInput, VoomError> {
    let missing = |field: &str| {
        VoomError::Internal(format!(
            "recoverable audio extraction output {} is missing {field}",
            stored.id
        ))
    };
    let output = serde_json::from_value(
        stored
            .result_facts
            .clone()
            .ok_or_else(|| missing("result_facts"))?,
    )
    .map_err(|error| {
        VoomError::Internal(format!(
            "recoverable audio extraction output {} has malformed result_facts: {error}",
            stored.id
        ))
    })?;
    Ok(commit::CommitAudioExtractOutputInput {
        operation_output_id: stored.id,
        artifact_handle_id: stored
            .artifact_handle_id
            .ok_or_else(|| missing("artifact_handle_id"))?,
        artifact_location_id: stored
            .artifact_location_id
            .ok_or_else(|| missing("artifact_location_id"))?,
        verification_id: stored
            .verification_id
            .ok_or_else(|| missing("verification_id"))?,
        role: selected.role,
        source_snapshot_stream_id: stored.source_snapshot_stream_id.clone(),
        source_provider_stream_index: stored.source_provider_stream_index,
        staging_path: PathBuf::from(
            stored
                .staging_path
                .as_ref()
                .ok_or_else(|| missing("staging_path"))?,
        ),
        target_path: PathBuf::from(&stored.target_path),
        prepared_temp_path: Some(PathBuf::from(
            stored
                .temp_path
                .as_ref()
                .ok_or_else(|| missing("temp_path"))?,
        )),
        prepared_commit_record_id: Some(
            stored
                .commit_record_id
                .ok_or_else(|| missing("commit_record_id"))?,
        ),
        output,
        probed: commit::ProbedResultPayload {
            worker_id: stored
                .probe_worker_id
                .ok_or_else(|| missing("probe_worker_id"))?,
            payload: stored
                .probe_payload
                .clone()
                .ok_or_else(|| missing("probe_payload"))?,
        },
    })
}

fn execution_report_from_outputs(
    input: &ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    outputs: Vec<ExecuteExtractAudioOutputReport>,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let first = outputs.first().ok_or_else(|| {
        VoomError::Internal("audio extraction recovery returned no outputs".to_owned())
    })?;
    Ok(ExecuteExtractAudioReport {
        job_id: input.job_id,
        ticket_id: input.ticket_id,
        lease_id: input.lease_id,
        source_file_version_id: input.source_file_version_id,
        source_file_location_id: source_location_id,
        staged_artifact_handle_id: first.staged_artifact_handle_id,
        staged_artifact_location_id: first.staged_artifact_location_id,
        verification_id: first.verification_id,
        commit_record_id: first.commit_record_id,
        result_file_version_id: first.result_file_version_id,
        result_file_location_id: first.result_file_location_id,
        staging_path: first.staging_path.clone(),
        target_path: first.target_path.clone(),
        commit_recovery_required: None,
        outputs,
    })
}

fn committed_output_report(
    stored: &voom_store::repo::audio_extract_operations::AudioExtractOperationOutput,
    selected: &selection::ExtractAudioSelectionOutput,
    source_file_version_id: FileVersionId,
    source_media_snapshot_id: MediaSnapshotId,
) -> Result<ExecuteExtractAudioOutputReport, VoomError> {
    let missing = |field: &str| {
        VoomError::Internal(format!(
            "committed audio extraction output {} is missing {field}",
            stored.id
        ))
    };
    Ok(ExecuteExtractAudioOutputReport {
        operation_output_id: stored.id,
        output_id: selected.output_id.clone(),
        source_file_version_id,
        source_media_snapshot_id,
        source_snapshot_stream_id: stored.source_snapshot_stream_id.clone(),
        source_provider_stream_index: stored.source_provider_stream_index,
        role: stored.bundle_role.clone(),
        staged_artifact_handle_id: stored
            .artifact_handle_id
            .ok_or_else(|| missing("artifact_handle_id"))?,
        staged_artifact_location_id: stored
            .artifact_location_id
            .ok_or_else(|| missing("artifact_location_id"))?,
        verification_id: stored
            .verification_id
            .ok_or_else(|| missing("verification_id"))?,
        commit_record_id: stored
            .commit_record_id
            .ok_or_else(|| missing("commit_record_id"))?,
        result_file_version_id: stored
            .result_file_version_id
            .ok_or_else(|| missing("result_file_version_id"))?,
        result_file_location_id: stored
            .result_file_location_id
            .ok_or_else(|| missing("result_file_location_id"))?,
        result_file_asset_id: stored
            .result_file_asset_id
            .ok_or_else(|| missing("result_file_asset_id"))?,
        result_media_snapshot_id: stored
            .result_media_snapshot_id
            .ok_or_else(|| missing("result_media_snapshot_id"))?,
        bundle_member_id: stored
            .bundle_member_id
            .ok_or_else(|| missing("bundle_member_id"))?,
        lineage_id: stored.lineage_id.ok_or_else(|| missing("lineage_id"))?,
        staging_path: PathBuf::from(
            stored
                .staging_path
                .as_ref()
                .ok_or_else(|| missing("staging_path"))?,
        ),
        target_path: PathBuf::from(&stored.target_path),
    })
}

fn extract_operation_token(
    source_file_version_id: FileVersionId,
    source_media_snapshot_id: u64,
    operation_id: Option<&str>,
    target_paths: &[PathBuf],
) -> String {
    let mut hasher = Sha256::new();
    update_operation_hash(&mut hasher, &source_file_version_id.0.to_string());
    update_operation_hash(&mut hasher, &source_media_snapshot_id.to_string());
    update_operation_hash(&mut hasher, operation_id.unwrap_or("legacy"));
    for path in target_paths {
        update_operation_hash(&mut hasher, &path.to_string_lossy());
    }
    format!("{:x}", hasher.finalize())
}

fn update_operation_hash(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_string());
    hasher.update(b":");
    hasher.update(value.as_bytes());
}

async fn verify_staged_extract(
    cp: &ControlPlane,
    staged: &commit::StagedAudioArtifact,
    staging_path: &std::path::Path,
    verify: &dyn VerifyArtifactDispatcher,
) -> Result<ArtifactVerificationId, VoomError> {
    let verified = verify_artifact_with_dispatcher(
        cp,
        VerifyArtifactInput::for_staged_file(staged.artifact_handle_id, staging_path),
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
    Ok(verified.verification_id)
}

async fn verify_staged_extract_set(
    cp: &ControlPlane,
    staged: &[commit::StagedAudioArtifact],
    staging_paths: &[PathBuf],
    verify: &dyn VerifyArtifactDispatcher,
) -> Result<Vec<ArtifactVerificationId>, VoomError> {
    if staged.len() != staging_paths.len() {
        return Err(VoomError::Internal(
            "audio extraction staged artifact/path count mismatch".to_owned(),
        ));
    }
    let mut verification_ids = Vec::with_capacity(staged.len());
    for (artifact, path) in staged.iter().zip(staging_paths) {
        verification_ids.push(verify_staged_extract(cp, artifact, path, verify).await?);
    }
    Ok(verification_ids)
}

async fn probe_staged_extract_set(
    cp: &ControlPlane,
    staging_paths: &[PathBuf],
    result: &ExtractAudioResult,
    probe: &dyn commit::AudioResultProbeDispatcher,
) -> Result<Vec<commit::ProbedResultPayload>, VoomError> {
    let output_facts = worker_contract::extract_result_output_facts(result);
    if staging_paths.len() != output_facts.len() {
        return Err(VoomError::Internal(
            "audio extraction probe inputs have inconsistent output counts".to_owned(),
        ));
    }
    let mut probed = Vec::with_capacity(staging_paths.len());
    for (path, output) in staging_paths.iter().zip(output_facts) {
        probed.push(commit::probe_staged_extract_result(cp, path, output, probe).await?);
    }
    Ok(probed)
}

async fn validate_extract_worker_result(
    selected: &source::SelectedSource,
    selection: &selection::ExtractAudioSelectionPlan,
    request: &voom_worker_protocol::ExtractAudioRequest,
    staging_paths: &[PathBuf],
    result: &ExtractAudioResult,
) -> Result<(), VoomError> {
    worker_contract::validate_extract_result(selected, selection, request, result)?;
    worker_contract::require_extract_output_files_match_result(staging_paths, result).await
}

async fn cleanup_terminal_extract_outputs(staging_paths: &[PathBuf]) -> Result<(), VoomError> {
    for path in staging_paths {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(VoomError::CommitFailure(format!(
                    "remove terminal audio extraction staging path {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
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
    outputs: Vec<voom_events::payload::ArtifactAudioExtractMemberPayload>,
}

struct ExtractCommitRequest {
    input: ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    source_media_snapshot_id: u64,
    staged: Vec<commit::StagedAudioArtifact>,
    staging_paths: Vec<PathBuf>,
    target_paths: Vec<PathBuf>,
    operation: AudioExtractOperationRecord,
    selection: selection::ExtractAudioSelectionPlan,
    result: ExtractAudioResult,
    verification_ids: Vec<ArtifactVerificationId>,
    probed: Vec<commit::ProbedResultPayload>,
}

async fn commit_verified_extract_audio(
    cp: &ControlPlane,
    request: ExtractCommitRequest,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let output_facts = worker_contract::extract_result_output_facts(&request.result);
    require_extract_commit_input_counts(&request, output_facts.len())?;
    let commit_inputs = extract_commit_inputs(&request, &output_facts);
    let committed = commit::commit_audio_extract_set(
        cp,
        &commit::CommitAudioExtractSetInput {
            operation_row_id: request.operation.operation.id,
            source_file_version_id: request.input.source_file_version_id,
            source_media_snapshot_id: MediaSnapshotId(request.source_media_snapshot_id),
            source_bundle_id: request.input.source_bundle_id,
            outputs: commit_inputs,
        },
    )
    .await?;
    complete_extract_report(cp, request, committed).await
}

fn extract_commit_inputs(
    request: &ExtractCommitRequest,
    output_facts: &[&AudioObservedFacts],
) -> Vec<commit::CommitAudioExtractOutputInput> {
    request
        .operation
        .outputs
        .iter()
        .zip(&request.staged)
        .zip(&request.verification_ids)
        .zip(&request.selection.outputs)
        .zip(&request.staging_paths)
        .zip(&request.target_paths)
        .zip(&request.probed)
        .zip(output_facts)
        .map(
            |(
                (
                    (
                        ((((operation, staged), verification_id), selection), staging_path),
                        target_path,
                    ),
                    probed,
                ),
                output,
            )| {
                commit::CommitAudioExtractOutputInput {
                    operation_output_id: operation.id,
                    artifact_handle_id: staged.artifact_handle_id,
                    artifact_location_id: staged.artifact_location_id,
                    verification_id: *verification_id,
                    role: selection.role,
                    source_snapshot_stream_id: selection.stream.snapshot_stream_id.clone(),
                    source_provider_stream_index: selection.stream.provider_stream_index,
                    staging_path: staging_path.clone(),
                    target_path: target_path.clone(),
                    prepared_temp_path: None,
                    prepared_commit_record_id: None,
                    output: (*output).clone(),
                    probed: probed.clone(),
                }
            },
        )
        .collect()
}

async fn complete_extract_report(
    cp: &ControlPlane,
    request: ExtractCommitRequest,
    committed: Vec<commit::CommittedAudioExtractOutput>,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let report_outputs = extract_output_reports(
        &request.selection,
        &committed,
        request.input.source_file_version_id,
        MediaSnapshotId(request.source_media_snapshot_id),
    )?;
    let first = report_outputs.first().ok_or_else(|| {
        VoomError::Internal("committed audio extraction returned no outputs".to_owned())
    })?;
    let committed_first = committed.first().ok_or_else(|| {
        VoomError::Internal("committed audio extraction returned no commit members".to_owned())
    })?;
    let first_selection = request.selection.outputs.first().ok_or_else(|| {
        VoomError::Internal("committed audio extraction lost its planned output".to_owned())
    })?;
    let commit_recovery_required = match events::record_extract_succeeded(
        cp,
        events::ExtractSucceededEventInput {
            input: &request.input,
            source_location_id: request.source_location_id,
            source_media_snapshot_id: request.source_media_snapshot_id,
            artifact_handle_id: first.staged_artifact_handle_id,
            artifact_location_id: first.staged_artifact_location_id,
            selection: &request.selection,
            result: &request.result,
            outputs: &report_outputs,
        },
    )
    .await
    {
        Ok(()) => None,
        Err(error) => Some(
            extract_set_post_commit_recovery(
                &request,
                committed_first,
                first_selection.role,
                &error,
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
        staged_artifact_handle_id: first.staged_artifact_handle_id,
        staged_artifact_location_id: first.staged_artifact_location_id,
        verification_id: first.verification_id,
        commit_record_id: first.commit_record_id,
        result_file_version_id: first.result_file_version_id,
        result_file_location_id: first.result_file_location_id,
        staging_path: first.staging_path.clone(),
        target_path: first.target_path.clone(),
        commit_recovery_required,
        outputs: report_outputs,
    })
}

async fn extract_set_post_commit_recovery(
    request: &ExtractCommitRequest,
    committed: &commit::CommittedAudioExtractOutput,
    role: voom_plan::audio::AudioBundleRole,
    error: &VoomError,
) -> commit::AudioExtractRecoveryReport {
    commit::AudioExtractRecoveryReport {
        recovery_reason: "audio extract post-commit reporting failed".to_owned(),
        commit_record_id: committed.commit_record_id,
        source_bundle_id: request.input.source_bundle_id,
        role: extract_role_name(role),
        target_path: committed.target_path.clone(),
        target_exists: tokio::fs::symlink_metadata(&committed.target_path)
            .await
            .is_ok(),
        temp_path: committed.temp_path.clone(),
        temp_exists: tokio::fs::symlink_metadata(&committed.temp_path)
            .await
            .is_ok(),
        staging_path: committed.staging_path.clone(),
        staging_exists: tokio::fs::symlink_metadata(&committed.staging_path)
            .await
            .is_ok(),
        result_file_version_id: Some(committed.result_file_version_id),
        result_file_location_id: Some(committed.result_file_location_id),
        error_code: error.error_code().as_str(),
        message: error.to_string(),
    }
}

fn require_extract_commit_input_counts(
    request: &ExtractCommitRequest,
    result_count: usize,
) -> Result<(), VoomError> {
    let expected = request.selection.outputs.len();
    let counts = [
        request.operation.outputs.len(),
        request.staged.len(),
        request.verification_ids.len(),
        request.staging_paths.len(),
        request.target_paths.len(),
        request.probed.len(),
        result_count,
    ];
    if counts.into_iter().all(|count| count == expected) {
        return Ok(());
    }
    Err(VoomError::Internal(format!(
        "audio extraction commit inputs disagree with {expected} planned outputs: {counts:?}"
    )))
}

fn extract_output_reports(
    selection: &selection::ExtractAudioSelectionPlan,
    committed: &[commit::CommittedAudioExtractOutput],
    source_file_version_id: FileVersionId,
    source_media_snapshot_id: MediaSnapshotId,
) -> Result<Vec<ExecuteExtractAudioOutputReport>, VoomError> {
    if committed.len() != selection.outputs.len() {
        return Err(VoomError::Internal(
            "committed audio extraction output count mismatch".to_owned(),
        ));
    }
    Ok(committed
        .iter()
        .zip(&selection.outputs)
        .map(|(output, selection)| ExecuteExtractAudioOutputReport {
            operation_output_id: output.operation_output_id,
            output_id: selection.output_id.clone(),
            source_file_version_id,
            source_media_snapshot_id,
            source_snapshot_stream_id: selection.stream.snapshot_stream_id.clone(),
            source_provider_stream_index: selection.stream.provider_stream_index,
            role: extract_role_name(selection.role).to_owned(),
            staged_artifact_handle_id: output.artifact_handle_id,
            staged_artifact_location_id: output.artifact_location_id,
            verification_id: output.verification_id,
            commit_record_id: output.commit_record_id,
            result_file_version_id: output.result_file_version_id,
            result_file_location_id: output.result_file_location_id,
            result_file_asset_id: output.result_file_asset_id,
            result_media_snapshot_id: output.result_media_snapshot_id,
            bundle_member_id: output.bundle_member_id,
            lineage_id: output.lineage_id,
            staging_path: output.staging_path.clone(),
            target_path: output.target_path.clone(),
        })
        .collect())
}

#[cfg(test)]
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
