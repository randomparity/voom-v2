use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, JobId, LeaseId,
    MediaSnapshotId, TicketId, VoomError,
};
use voom_events::payload::{
    ArtifactAudioDispositionPayload, ArtifactAudioExtractQuiescedPayload,
    ArtifactAudioStreamPayload, ArtifactAudioSynthesisCompanionPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::artifacts::{ArtifactCommitState, ArtifactVerificationStatus};
use voom_store::repo::identity::FileVersionRepo;
use voom_store::repo::media::audio_extract_operations::{
    AudioExtractDispatchAttemptStatus, AudioExtractOperationRecord, AudioExtractOperationState,
    AudioExtractQuiescenceAcknowledgement, NewAudioExtractClaim, NewAudioExtractDispatchAttempt,
    NewAudioExtractOperation, NewAudioExtractOutput, SqliteAudioExtractOperationRepo,
};
use voom_store::repo::media::audio_synthesis_operations::{
    AudioSynthesisDispatchAttempt, AudioSynthesisOperationRecord, AudioSynthesisOperationState,
    FinalizeAudioSynthesisOperation, NewAudioSynthesisClaim, NewAudioSynthesisCompanion,
    NewAudioSynthesisDispatchAttempt, NewAudioSynthesisOperation,
    SqliteAudioSynthesisOperationRepo, StagedAudioSynthesisCompanion,
    ValidateAudioSynthesisOperation,
};
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

#[derive(Debug, Clone)]
pub(crate) struct FirstExtractPlanInput {
    pub(crate) source_file_version_id: FileVersionId,
    pub(crate) source_location_id: Option<FileLocationId>,
    pub(crate) operation_payload: serde_json::Value,
    pub(crate) target_dir: PathBuf,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_operation_key: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub synthesized_companions: Vec<ExecuteSynthesisCompanionReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteSynthesisCompanionReport {
    pub ordinal: u32,
    pub companion_id: String,
    pub source_file_version_id: FileVersionId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub result_snapshot_stream_id: String,
    pub result_provider_stream_index: u32,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub lineage_id: u64,
    pub location: PathBuf,
    pub codec: String,
    pub channels: u32,
    pub language: Option<String>,
    pub title: Option<String>,
    pub disposition_default: bool,
    pub disposition_forced: bool,
    pub disposition_commentary: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
        dispatch_lease_id: LeaseId,
        idempotency_key: &str,
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
            abandon_failed_synthesis_generation(cp, &context, &err).await;
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
                    synthesis_operation_id: context.synthesis_operation_id.clone(),
                    synthesis_operation_key: context.synthesis_operation_key.clone(),
                    synthesized_companions: context.synthesized_companions.clone(),
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
    record_transcode_selection_context(context, &input, &selection);
    let add_track = selection.add_track;
    let resolved = ResolvedTranscodeAudio {
        input,
        selected,
        snapshot,
        selection,
    };
    let dispatchers = TranscodeAudioDispatchers {
        transcode,
        verify,
        result_probe,
    };
    if add_track {
        return execute_synthesis_audio(cp, resolved, &dispatchers, context).await;
    }
    execute_replacement_audio(cp, resolved, &dispatchers, context).await
}

fn record_transcode_selection_context(
    context: &mut TranscodeAttemptContext,
    input: &ExecuteTranscodeAudioInput,
    selection: &selection::TranscodeAudioSelectionPlan,
) {
    context.selected_streams = events::stream_payloads(&selection.selection.selected_streams);
    context
        .synthesis_operation_id
        .clone_from(&selection.operation_id);
    context.synthesis_operation_key = selection
        .operation_id
        .as_deref()
        .map(|operation_id| synthesis_operation_key(input.source_file_version_id, operation_id));
    context.synthesized_companions = events::planned_synthesis_companions(selection);
}

struct ResolvedTranscodeAudio {
    input: ExecuteTranscodeAudioInput,
    selected: source::SelectedSource,
    snapshot: voom_store::repo::identity::MediaSnapshot,
    selection: selection::TranscodeAudioSelectionPlan,
}

struct TranscodeAudioDispatchers<'a> {
    transcode: &'a dyn TranscodeAudioDispatcher,
    verify: &'a dyn VerifyArtifactDispatcher,
    result_probe: &'a dyn commit::AudioResultProbeDispatcher,
}

async fn execute_replacement_audio(
    cp: &ControlPlane,
    resolved: ResolvedTranscodeAudio,
    dispatchers: &TranscodeAudioDispatchers<'_>,
    context: &mut TranscodeAttemptContext,
) -> Result<ExecuteTranscodeAudioReport, VoomError> {
    let ResolvedTranscodeAudio {
        input,
        selected,
        snapshot,
        selection,
    } = resolved;
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
    let idempotency_key = format!("ticket-{}-lease-{}", input.ticket_id.0, input.lease_id.0);
    let result = dispatchers
        .transcode
        .dispatch_transcode_audio(input.lease_id, &idempotency_key, request)
        .await?;
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
        dispatchers.verify,
        dispatchers.result_probe,
    )
    .await
}

async fn execute_synthesis_audio(
    cp: &ControlPlane,
    resolved: ResolvedTranscodeAudio,
    dispatchers: &TranscodeAudioDispatchers<'_>,
    context: &mut TranscodeAttemptContext,
) -> Result<ExecuteTranscodeAudioReport, VoomError> {
    let ResolvedTranscodeAudio {
        input,
        selected,
        snapshot,
        selection,
    } = resolved;
    let target_path = stage::synthesis_target_path(
        &input.target_dir,
        &selected.canonical_path,
        &selection.target_codec,
    )
    .await?;
    let operation =
        resolve_synthesis_operation(cp, &input, &snapshot, &selection, &target_path).await?;
    if operation.operation.state != AudioSynthesisOperationState::Planned {
        let operation =
            validate_staged_synthesis_operation(cp, operation, &snapshot, &selection, dispatchers)
                .await?;
        return finish_synthesis_operation(cp, &input, selected.location.id, &operation).await;
    }
    let dispatch = claim_synthesis_dispatch(
        cp,
        &input,
        &operation,
        &selected.canonical_path,
        &selection.target_codec,
    )
    .await?;
    let claim = dispatch.claim.clone();
    let staging = dispatch.staging.clone();
    context.synthesis_claim = Some(claim.clone());
    context.staging_path = Some(staging.path.clone());
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
    let result =
        dispatch_synthesis_result(cp, dispatchers.transcode, &dispatch, request, context).await?;
    context.result = Some(result.clone());
    worker_contract::validate_transcode_result(&selected, &selection, &result)?;
    worker_contract::require_transcode_output_file_matches_result(&staging.path, &result).await?;
    let staged = commit::record_staged_audio_synthesis(
        cp,
        commit::StageAudioSynthesisArtifactInput {
            execution: &input,
            source_file_location_id: selected.location.id,
            staging_path: &staging.path,
            operation_id: operation.operation.id,
            claim: &claim,
            result: &result,
            companions: staged_synthesis_companions(&result)?,
        },
    )
    .await?;
    context.artifact_handle_id = Some(staged.artifact_handle_id);
    context.artifact_location_id = Some(staged.artifact_location_id);
    context.synthesis_claim = None;
    let operation = cp
        .audio_synthesis_operations
        .get_by_key(&operation.operation.operation_key)
        .await?
        .ok_or_else(|| VoomError::Internal("staged audio synthesis disappeared".to_owned()))?;
    let operation =
        validate_staged_synthesis_operation(cp, operation, &snapshot, &selection, dispatchers)
            .await?;
    finish_synthesis_operation(cp, &input, selected.location.id, &operation).await
}

fn require_synthesis_verification(
    verified: &crate::artifact::verify::VerifyArtifactReport,
) -> Result<(), VoomError> {
    if verified.status == ArtifactVerificationStatus::Succeeded {
        return Ok(());
    }
    Err(VoomError::VerificationFailure(
        "audio synthesis artifact verification failed".to_owned(),
    ))
}

async fn dispatch_synthesis_result(
    cp: &ControlPlane,
    transcode: &dyn TranscodeAudioDispatcher,
    dispatch: &ClaimedSynthesisDispatch,
    request: voom_worker_protocol::TranscodeAudioRequest,
    context: &mut TranscodeAttemptContext,
) -> Result<TranscodeAudioResult, VoomError> {
    match transcode
        .dispatch_transcode_audio(
            dispatch.attempt.dispatch_lease_id,
            &dispatch.attempt.idempotency_key,
            request,
        )
        .await
    {
        Ok(result) => {
            cp.audio_synthesis_operations
                .mark_dispatch_terminal(&dispatch.claim, dispatch.attempt.id, cp.clock().now())
                .await?;
            Ok(result)
        }
        Err(error) => {
            cp.audio_synthesis_operations
                .release_claim(&dispatch.claim)
                .await?;
            context.synthesis_claim = None;
            Err(error)
        }
    }
}

async fn resolve_synthesis_operation(
    cp: &ControlPlane,
    input: &ExecuteTranscodeAudioInput,
    snapshot: &voom_store::repo::identity::MediaSnapshot,
    selection: &selection::TranscodeAudioSelectionPlan,
    target_path: &Path,
) -> Result<AudioSynthesisOperationRecord, VoomError> {
    let planned_operation_id = selection.operation_id.as_deref().ok_or_else(|| {
        VoomError::Config("synthesize_audio selection has no operation identity".to_owned())
    })?;
    let companions = selection
        .selected_streams
        .iter()
        .map(|selected| NewAudioSynthesisCompanion {
            companion_id: selected.stream.snapshot_stream_id.clone(),
            source_snapshot_stream_id: selected.source.snapshot_stream_id.clone(),
            source_provider_stream_index: selected.source.provider_stream_index,
            result_snapshot_stream_id: selected.stream.snapshot_stream_id.clone(),
        })
        .collect::<Vec<_>>();
    cp.audio_synthesis_operations
        .create_planned(
            NewAudioSynthesisOperation {
                operation_key: synthesis_operation_key(
                    input.source_file_version_id,
                    planned_operation_id,
                ),
                planned_operation_id: planned_operation_id.to_owned(),
                source_file_version_id: input.source_file_version_id,
                source_media_snapshot_id: snapshot.id,
                target_codec: selection.target_codec.clone(),
                target_channels: u32::try_from(selection.target_channels.ok_or_else(|| {
                    VoomError::Config(
                        "synthesize_audio selection has no target channels".to_owned(),
                    )
                })?)
                .map_err(|error| {
                    VoomError::Config(format!(
                        "synthesize_audio target channels exceed u32: {error}"
                    ))
                })?,
                container: selection.container.clone(),
                target_path: target_path.display().to_string(),
            },
            &companions,
            cp.clock().now(),
        )
        .await
}

fn synthesis_operation_token(operation_key: &str) -> String {
    blake3::hash(operation_key.as_bytes()).to_hex()[..16].to_owned()
}

fn synthesis_operation_key(
    source_file_version_id: FileVersionId,
    planned_operation_id: &str,
) -> String {
    format!(
        "synthesize:{}:{planned_operation_id}",
        source_file_version_id.0
    )
}

struct ClaimedSynthesisDispatch {
    claim: NewAudioSynthesisClaim,
    attempt: AudioSynthesisDispatchAttempt,
    staging: stage::PreparedStagingPath,
}

async fn claim_synthesis_dispatch(
    cp: &ControlPlane,
    input: &ExecuteTranscodeAudioInput,
    operation: &AudioSynthesisOperationRecord,
    source_path: &Path,
    codec: &str,
) -> Result<ClaimedSynthesisDispatch, VoomError> {
    let now = cp.clock().now();
    let (worker_id, worker_epoch, expires_at) = audio_dispatch_lease(cp, input.lease_id).await?;
    let claim = NewAudioSynthesisClaim {
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
    let repo = &cp.audio_synthesis_operations;
    repo.acquire_claim(&claim, now).await?;
    let token = synthesis_operation_token(&operation.operation.operation_key);
    if let Some(attempt) = repo
        .get_dispatch_attempt(operation.operation.id, claim.expected_generation)
        .await?
    {
        return reconcile_synthesis_dispatch(
            repo,
            claim,
            attempt,
            worker_id,
            worker_epoch,
            stage::resolve_synthesis_staging_path(
                &input.staging_root,
                &token,
                operation.operation.dispatch_generation,
                source_path,
                codec,
            )
            .await?,
            now,
        )
        .await;
    }
    let staging = stage::prepare_synthesis_staging_path(
        &input.staging_root,
        &token,
        operation.operation.dispatch_generation,
        source_path,
        codec,
    )
    .await?;
    let attempt_directory = staging.path.parent().ok_or_else(|| {
        VoomError::Internal("audio synthesis staging path has no parent".to_owned())
    })?;
    let attempt = repo
        .record_dispatch_attempt(
            &claim,
            &NewAudioSynthesisDispatchAttempt {
                dispatch_lease_id: input.lease_id,
                worker_id: worker_id.0,
                worker_epoch,
                idempotency_key: format!(
                    "audio-synthesis:{}:{}",
                    operation.operation.operation_key, operation.operation.dispatch_generation
                ),
                attempt_directory: attempt_directory.display().to_string(),
                staging_path: staging.path.display().to_string(),
            },
            now,
        )
        .await?;
    Ok(ClaimedSynthesisDispatch {
        claim,
        attempt,
        staging,
    })
}

async fn reconcile_synthesis_dispatch(
    repo: &voom_store::repo::media::audio_synthesis_operations::SqliteAudioSynthesisOperationRepo,
    claim: NewAudioSynthesisClaim,
    attempt: AudioSynthesisDispatchAttempt,
    worker_id: voom_core::WorkerId,
    worker_epoch: u32,
    staging: stage::PreparedStagingPath,
    now: time::OffsetDateTime,
) -> Result<ClaimedSynthesisDispatch, VoomError> {
    if attempt.status == "active"
        && attempt.worker_id == worker_id.0
        && attempt.worker_epoch == worker_epoch
    {
        let attempt_directory = staging.path.parent().ok_or_else(|| {
            VoomError::Internal("audio synthesis staging path has no parent".to_owned())
        })?;
        if attempt.attempt_directory != attempt_directory.display().to_string()
            || attempt.staging_path != staging.path.display().to_string()
        {
            repo.quarantine_and_advance_generation(&claim, attempt.id, now)
                .await?;
            return Err(VoomError::Conflict(format!(
                "audio synthesis attempt {} does not match its deterministic staging path",
                attempt.id
            )));
        }
        return Ok(ClaimedSynthesisDispatch {
            claim,
            attempt,
            staging,
        });
    }
    if attempt.status == "active" {
        repo.quarantine_and_advance_generation(&claim, attempt.id, now)
            .await?;
        return Err(VoomError::Conflict(format!(
            "audio synthesis attempt {} belongs to worker {} epoch {}; generation advanced \
             before retry",
            attempt.id, attempt.worker_id, attempt.worker_epoch
        )));
    }
    repo.abandon_planned_generation(&claim, now).await?;
    Err(VoomError::Conflict(format!(
        "audio synthesis attempt {} is {}; generation advanced before retry",
        attempt.id, attempt.status
    )))
}

fn staged_synthesis_companions(
    result: &TranscodeAudioResult,
) -> Result<Vec<StagedAudioSynthesisCompanion>, VoomError> {
    result
        .selected_output_streams
        .iter()
        .map(|fact| {
            let disposition = fact.disposition.as_ref();
            Ok(StagedAudioSynthesisCompanion {
                companion_id: fact.snapshot_stream_id.clone(),
                result_provider_stream_index: fact.output_provider_stream_index,
                codec: fact.codec.clone(),
                channels: u32::try_from(fact.channels.ok_or_else(|| {
                    VoomError::MalformedWorkerResult(
                        "synthesis result has no channel count".to_owned(),
                    )
                })?)
                .map_err(|error| {
                    VoomError::MalformedWorkerResult(format!(
                        "synthesis result channels exceed u32: {error}"
                    ))
                })?,
                language: fact.language.clone(),
                title: fact.title.clone(),
                disposition_default: disposition
                    .and_then(|value| value.default)
                    .or(fact.default)
                    .unwrap_or(false),
                disposition_forced: disposition.and_then(|value| value.forced).unwrap_or(false),
                disposition_commentary: disposition
                    .and_then(|value| value.commentary)
                    .unwrap_or(false),
                result_facts: serde_json::to_value(fact).map_err(|error| {
                    VoomError::Internal(format!("encode staged audio synthesis companion: {error}"))
                })?,
            })
        })
        .collect()
}

async fn validate_staged_synthesis_operation(
    cp: &ControlPlane,
    operation: AudioSynthesisOperationRecord,
    snapshot: &voom_store::repo::identity::MediaSnapshot,
    selection: &selection::TranscodeAudioSelectionPlan,
    dispatchers: &TranscodeAudioDispatchers<'_>,
) -> Result<AudioSynthesisOperationRecord, VoomError> {
    if operation.operation.verification_id.is_some() {
        return Ok(operation);
    }
    let staging_path = PathBuf::from(required_synthesis_field(
        operation.operation.staging_path.clone(),
        "staging path",
    )?);
    let artifact_handle_id =
        required_synthesis_field(operation.operation.artifact_handle_id, "artifact handle")?;
    let result: TranscodeAudioResult = serde_json::from_value(required_synthesis_field(
        operation.operation.worker_result.clone(),
        "worker result",
    )?)
    .map_err(|error| {
        VoomError::database(format!(
            "decode staged audio synthesis worker result: {error}"
        ))
    })?;
    let verified = verify_artifact_with_dispatcher(
        cp,
        VerifyArtifactInput::for_staged_file(artifact_handle_id, &staging_path),
        dispatchers.verify,
        &NoVerifyArtifactHooks,
    )
    .await?;
    require_synthesis_verification(&verified)?;
    let probed = commit::probe_staged_synthesis_result(
        cp,
        &staging_path,
        snapshot,
        selection,
        &result,
        dispatchers.result_probe,
    )
    .await?;
    cp.audio_synthesis_operations
        .record_validation(&ValidateAudioSynthesisOperation {
            operation_id: operation.operation.id,
            verification_id: verified.verification_id,
            probe_worker_id: probed.worker_id,
            probe_payload: probed.payload,
        })
        .await?;
    cp.audio_synthesis_operations
        .get_by_key(&operation.operation.operation_key)
        .await?
        .ok_or_else(|| VoomError::Internal("validated audio synthesis disappeared".to_owned()))
}

async fn finish_synthesis_operation(
    cp: &ControlPlane,
    input: &ExecuteTranscodeAudioInput,
    source_location_id: FileLocationId,
    operation: &AudioSynthesisOperationRecord,
) -> Result<ExecuteTranscodeAudioReport, VoomError> {
    if operation.operation.state == AudioSynthesisOperationState::Committed {
        return synthesis_report(input, source_location_id, operation);
    }
    if operation.operation.state != AudioSynthesisOperationState::Staged {
        return Err(VoomError::Conflict(format!(
            "audio synthesis {} cannot resume from {:?}",
            operation.operation.operation_key, operation.operation.state
        )));
    }
    let artifact_handle_id = operation.operation.artifact_handle_id.ok_or_else(|| {
        VoomError::Internal("staged audio synthesis has no artifact handle".to_owned())
    })?;
    let prepared = prepare_or_recover_synthesis(cp, operation, artifact_handle_id).await?;
    if let Err(error) =
        finalize_synthesis_lineage(cp, input, source_location_id, operation, &prepared).await
    {
        if let Err(recovery_error) = crate::artifact::commit::transition_prepared_artifact_recovery(
            cp,
            &prepared,
            VoomError::CommitFailure(error.to_string()),
        )
        .await
        {
            tracing::warn!(
                primary_error = %error,
                secondary_error = %recovery_error,
                operation_key = operation.operation.operation_key,
                "audio synthesis recovery transition failed"
            );
        }
        return Err(error);
    }
    let operation = cp
        .audio_synthesis_operations
        .get_by_key(&operation.operation.operation_key)
        .await?
        .ok_or_else(|| VoomError::Internal("committed audio synthesis disappeared".to_owned()))?;
    synthesis_report(input, source_location_id, &operation)
}

async fn prepare_or_recover_synthesis(
    cp: &ControlPlane,
    operation: &AudioSynthesisOperationRecord,
    artifact_handle_id: ArtifactHandleId,
) -> Result<crate::artifact::commit::PreparedArtifactCommit, VoomError> {
    let records = cp.artifacts.list_commit_records(artifact_handle_id).await?;
    if let Some(state) = [
        ArtifactCommitState::RecoveryRequired,
        ArtifactCommitState::Pending,
    ]
    .into_iter()
    .find(|state| records.iter().any(|record| record.state == *state))
    {
        crate::artifact::commit::prepare_artifact_recovery(cp, artifact_handle_id, state).await
    } else {
        let prepared = crate::artifact::commit::prepare_and_promote_artifact(
            cp,
            CommitArtifactInput {
                artifact_handle_id,
                target_path: PathBuf::from(&operation.operation.target_path),
            },
        )
        .await
        .map_err(|error| VoomError::CommitFailure(error.to_string()))?;
        Ok(prepared)
    }
}

async fn finalize_synthesis_lineage(
    cp: &ControlPlane,
    input: &ExecuteTranscodeAudioInput,
    source_location_id: FileLocationId,
    operation: &AudioSynthesisOperationRecord,
    prepared: &crate::artifact::commit::PreparedArtifactCommit,
) -> Result<(), VoomError> {
    let probe_worker_id = operation.operation.probe_worker_id.ok_or_else(|| {
        VoomError::Internal("staged audio synthesis has no probe worker".to_owned())
    })?;
    let probe_payload = operation.operation.probe_payload.clone().ok_or_else(|| {
        VoomError::Internal("staged audio synthesis has no probe payload".to_owned())
    })?;
    let mut tx = crate::cases::begin_immediate_tx(&cp.pool).await?;
    let committed =
        crate::artifact::commit::finalize_prepared_artifact_in_tx(cp, &mut tx, prepared).await?;
    let result_file_version_id = committed.result_file_version_id.ok_or_else(|| {
        VoomError::Internal("audio synthesis commit has no result file version".to_owned())
    })?;
    let result_file_location_id = committed.result_file_location_id.ok_or_else(|| {
        VoomError::Internal("audio synthesis commit has no result file location".to_owned())
    })?;
    let result_file_asset_id =
        synthesis_result_file_asset_id(cp, &mut tx, result_file_version_id).await?;
    let snapshot = crate::media_snapshot::record_with_event_in_tx(
        cp,
        &mut tx,
        voom_store::repo::identity::NewMediaSnapshot {
            file_version_id: result_file_version_id,
            probed_by: Some(probe_worker_id),
            probed_at: cp.clock().now(),
            payload: probe_payload,
        },
    )
    .await?;
    SqliteAudioSynthesisOperationRepo::finalize_in_tx(
        &mut tx,
        &FinalizeAudioSynthesisOperation {
            operation_id: operation.operation.id,
            commit_record_id: committed.commit_record_id,
            result_file_asset_id,
            result_file_version_id,
            result_file_location_id,
            result_media_snapshot_id: snapshot.id,
            recorded_at: cp.clock().now(),
        },
    )
    .await?;
    let worker_result: TranscodeAudioResult =
        serde_json::from_value(operation.operation.worker_result.clone().ok_or_else(|| {
            VoomError::Internal("staged audio synthesis has no worker result".to_owned())
        })?)
        .map_err(|error| {
            VoomError::database(format!(
                "staged audio synthesis worker result is malformed: {error}"
            ))
        })?;
    let synthesized_companions = synthesis_event_companions(operation)?;
    crate::cases::append_event(
        &cp.events,
        &mut tx,
        SubjectType::ArtifactHandle,
        Some(
            operation
                .operation
                .artifact_handle_id
                .ok_or_else(|| {
                    VoomError::Internal("staged audio synthesis has no artifact handle".to_owned())
                })?
                .0,
        ),
        cp.clock().now(),
        events::transcode_succeeded_event(events::TranscodeSucceededEventInput {
            input,
            source_location_id,
            source_media_snapshot_id: operation.operation.source_media_snapshot_id.0,
            artifact_handle_id: operation.operation.artifact_handle_id.ok_or_else(|| {
                VoomError::Internal("staged audio synthesis has no artifact handle".to_owned())
            })?,
            artifact_location_id: operation.operation.artifact_location_id.ok_or_else(|| {
                VoomError::Internal("staged audio synthesis has no artifact location".to_owned())
            })?,
            selected_streams: synthesized_companions
                .iter()
                .map(|companion| ArtifactAudioStreamPayload {
                    snapshot_stream_id: companion.companion_id.clone(),
                    provider_stream_index: companion.source_provider_stream_index,
                })
                .collect(),
            result: &worker_result,
            synthesis_operation_id: Some(operation.operation.planned_operation_id.clone()),
            synthesis_operation_key: Some(operation.operation.operation_key.clone()),
            synthesized_companions,
        }),
    )
    .await?;
    crate::cases::commit_tx(tx).await
}

async fn synthesis_result_file_asset_id(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    result_file_version_id: FileVersionId,
) -> Result<voom_core::FileAssetId, VoomError> {
    cp.identity
        .get_file_version_in_tx(tx, result_file_version_id)
        .await?
        .map(|version| version.file_asset_id)
        .ok_or_else(|| {
            VoomError::Internal("committed synthesis result file version disappeared".to_owned())
        })
}

fn synthesis_event_companions(
    operation: &AudioSynthesisOperationRecord,
) -> Result<Vec<ArtifactAudioSynthesisCompanionPayload>, VoomError> {
    operation
        .companions
        .iter()
        .map(|companion| {
            Ok(ArtifactAudioSynthesisCompanionPayload {
                companion_id: companion.companion_id.clone(),
                source_snapshot_stream_id: companion.source_snapshot_stream_id.clone(),
                source_provider_stream_index: companion.source_provider_stream_index,
                result_snapshot_stream_id: companion.result_snapshot_stream_id.clone(),
                result_provider_stream_index: Some(required_synthesis_field(
                    companion.result_provider_stream_index,
                    "companion result provider stream",
                )?),
                codec: Some(required_synthesis_field(
                    companion.codec.clone(),
                    "companion codec",
                )?),
                channels: Some(u64::from(required_synthesis_field(
                    companion.channels,
                    "companion channel count",
                )?)),
                language: companion.language.clone(),
                title: companion.title.clone(),
                disposition: Some(ArtifactAudioDispositionPayload {
                    default: Some(required_synthesis_field(
                        companion.disposition_default,
                        "companion default disposition",
                    )?),
                    forced: Some(required_synthesis_field(
                        companion.disposition_forced,
                        "companion forced disposition",
                    )?),
                    commentary: Some(required_synthesis_field(
                        companion.disposition_commentary,
                        "companion commentary disposition",
                    )?),
                }),
            })
        })
        .collect()
}

fn synthesis_report(
    input: &ExecuteTranscodeAudioInput,
    source_location_id: FileLocationId,
    operation: &AudioSynthesisOperationRecord,
) -> Result<ExecuteTranscodeAudioReport, VoomError> {
    let artifact_handle_id =
        required_synthesis_field(operation.operation.artifact_handle_id, "artifact handle")?;
    let artifact_location_id = required_synthesis_field(
        operation.operation.artifact_location_id,
        "artifact location",
    )?;
    let result_file_version_id = required_synthesis_field(
        operation.operation.result_file_version_id,
        "result file version",
    )?;
    let result_file_location_id = required_synthesis_field(
        operation.operation.result_file_location_id,
        "result file location",
    )?;
    let result_media_snapshot_id = required_synthesis_field(
        operation.operation.result_media_snapshot_id,
        "result media snapshot",
    )?;
    let synthesized_companions = operation
        .companions
        .iter()
        .map(|companion| {
            Ok(ExecuteSynthesisCompanionReport {
                ordinal: companion.ordinal,
                companion_id: companion.companion_id.clone(),
                source_file_version_id: operation.operation.source_file_version_id,
                source_media_snapshot_id: operation.operation.source_media_snapshot_id,
                source_snapshot_stream_id: companion.source_snapshot_stream_id.clone(),
                source_provider_stream_index: companion.source_provider_stream_index,
                result_file_version_id,
                result_file_location_id,
                result_media_snapshot_id,
                result_snapshot_stream_id: companion.result_snapshot_stream_id.clone(),
                result_provider_stream_index: required_synthesis_field(
                    companion.result_provider_stream_index,
                    "companion result provider stream",
                )?,
                artifact_handle_id,
                artifact_location_id,
                lineage_id: required_synthesis_field(companion.lineage_id, "companion lineage")?,
                location: PathBuf::from(&operation.operation.target_path),
                codec: required_synthesis_field(companion.codec.clone(), "companion codec")?,
                channels: required_synthesis_field(companion.channels, "companion channel count")?,
                language: companion.language.clone(),
                title: companion.title.clone(),
                disposition_default: required_synthesis_field(
                    companion.disposition_default,
                    "companion default disposition",
                )?,
                disposition_forced: required_synthesis_field(
                    companion.disposition_forced,
                    "companion forced disposition",
                )?,
                disposition_commentary: required_synthesis_field(
                    companion.disposition_commentary,
                    "companion commentary disposition",
                )?,
            })
        })
        .collect::<Result<Vec<_>, VoomError>>()?;
    Ok(ExecuteTranscodeAudioReport {
        job_id: input.job_id,
        ticket_id: input.ticket_id,
        lease_id: input.lease_id,
        source_file_version_id: input.source_file_version_id,
        source_file_location_id: source_location_id,
        staged_artifact_handle_id: artifact_handle_id,
        staged_artifact_location_id: artifact_location_id,
        verification_id: required_synthesis_field(
            operation.operation.verification_id,
            "verification",
        )?,
        commit_record_id: required_synthesis_field(
            operation.operation.commit_record_id,
            "commit record",
        )?,
        result_file_version_id,
        result_file_location_id,
        result_media_snapshot_id,
        staging_path: PathBuf::from(required_synthesis_field(
            operation.operation.staging_path.clone(),
            "staging path",
        )?),
        target_path: PathBuf::from(&operation.operation.target_path),
        commit_recovery_required: None,
        synthesis_operation_id: Some(operation.operation.planned_operation_id.clone()),
        synthesis_operation_key: Some(operation.operation.operation_key.clone()),
        synthesized_companions,
    })
}

fn required_synthesis_field<T>(value: Option<T>, name: &str) -> Result<T, VoomError> {
    value.ok_or_else(|| {
        VoomError::Internal(format!("committed audio synthesis has no {name} identity"))
    })
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
    synthesis_claim: Option<NewAudioSynthesisClaim>,
    synthesis_operation_id: Option<String>,
    synthesis_operation_key: Option<String>,
    synthesized_companions: Vec<voom_events::payload::ArtifactAudioSynthesisCompanionPayload>,
}

async fn abandon_failed_synthesis_generation(
    cp: &ControlPlane,
    context: &TranscodeAttemptContext,
    primary: &VoomError,
) {
    let Some(claim) = &context.synthesis_claim else {
        return;
    };
    if let Err(error) = cp
        .audio_synthesis_operations
        .abandon_planned_generation(claim, cp.clock().now())
        .await
    {
        tracing::warn!(
            primary_error_code = primary.error_code().as_str(),
            secondary_error = %error,
            operation_key = claim.operation_key,
            "audio synthesis generation abandonment failed"
        );
    }
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
            synthesis_operation_id: None,
            synthesis_operation_key: None,
            synthesized_companions: Vec::new(),
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
        synthesis_operation_id: None,
        synthesis_operation_key: None,
        synthesized_companions: Vec::new(),
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
    execute_extract_audio_with_services(
        cp,
        input,
        ExtractAudioExecutionServices {
            extract,
            verify,
            result_probe,
            claim_fence_hooks: &commit::NoExtractClaimFenceHooks,
        },
    )
    .await
}

#[derive(Clone, Copy)]
struct ExtractAudioExecutionServices<'a> {
    extract: &'a dyn ExtractAudioDispatcher,
    verify: &'a dyn VerifyArtifactDispatcher,
    result_probe: &'a dyn commit::AudioResultProbeDispatcher,
    claim_fence_hooks: &'a dyn commit::ExtractClaimFenceHooks,
}

async fn execute_extract_audio_with_services(
    cp: &ControlPlane,
    input: ExecuteExtractAudioInput,
    services: ExtractAudioExecutionServices<'_>,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let ExtractAudioExecutionServices {
        extract,
        verify,
        result_probe,
        claim_fence_hooks,
    } = services;
    let failure_input = input.clone();
    let mut context = ExtractAttemptContext::default();
    let verify = verify.start_session();
    let result_probe = result_probe.start_session();
    let outcome = execute_extract_audio_inner(
        ExtractExecutionDependencies {
            cp,
            extract,
            verify: verify.as_ref(),
            result_probe: result_probe.as_ref(),
            claim_fence_hooks,
        },
        input,
        &mut context,
    )
    .await;
    verify.shutdown().await;
    result_probe.shutdown().await;
    match outcome {
        Ok(report) => Ok(report),
        Err(error) => Err(finalize_extract_failure(cp, &failure_input, &context, error).await),
    }
}

async fn finalize_extract_failure(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    context: &ExtractAttemptContext,
    error: VoomError,
) -> VoomError {
    if let Some(claim) = &context.claim
        && let Err(release_error) = cp.audio_extract_operations.release_claim(claim).await
    {
        tracing::warn!(
            primary_error_code = error.error_code().as_str(),
            secondary_error = %release_error,
            operation_key = claim.operation_key,
            "audio extraction claim release failed while preserving the primary error"
        );
    }
    if let Err(event_error) = events::record_extract_failed(
        cp,
        events::ExtractFailedEventInput {
            input,
            source_location_id: context.source_location_id,
            source_media_snapshot_id: context
                .source_media_snapshot_id
                .or_else(|| audio_payload_snapshot_id(&input.operation_payload)),
            selection: context.selection.as_ref(),
            staging_path: context.staging_path.as_deref(),
            artifact_handle_id: context.artifact_handle_id,
            artifact_location_id: context.artifact_location_id,
            result: context.result.as_ref(),
            outputs: &context.outputs,
            error: &error,
        },
    )
    .await
    {
        tracing::warn!(
            primary_error_code = error.error_code().as_str(),
            secondary_error = %event_error,
            ticket_id = input.ticket_id.0,
            "audio extraction failure-event persistence failed while preserving the primary error"
        );
    }
    error
}

async fn execute_extract_audio_inner(
    dependencies: ExtractExecutionDependencies<'_>,
    input: ExecuteExtractAudioInput,
    context: &mut ExtractAttemptContext,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let ExtractExecutionDependencies {
        cp,
        extract,
        verify,
        result_probe,
        claim_fence_hooks,
    } = dependencies;
    let prepared = prepare_extract_execution(cp, &input, result_probe, context).await?;
    let resume = ExtractResumeContext {
        cp,
        input: &input,
        source_location_id: prepared.selected.location.id,
        selection: &prepared.selection,
        operation: &prepared.paths.operation,
    };
    if let Some(report) =
        maybe_resume_extract_operation(&resume, verify, result_probe, context, claim_fence_hooks)
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
            claim_fence_hooks,
        },
        input,
        prepared,
        context,
    ))
    .await
}

struct ExtractExecutionDependencies<'a> {
    cp: &'a ControlPlane,
    extract: &'a dyn ExtractAudioDispatcher,
    verify: &'a dyn VerifyArtifactDispatcher,
    result_probe: &'a dyn commit::AudioResultProbeDispatcher,
    claim_fence_hooks: &'a dyn commit::ExtractClaimFenceHooks,
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
        claim_fence_hooks,
    } = dependencies;
    let PreparedExtractExecution {
        selected,
        snapshot,
        selection,
        paths,
    } = prepared;
    back_up_extract_source(cp, &input, &selected).await?;
    let dispatch = claim_extract_dispatch(cp, &input, &paths.operation, &paths.targets).await?;
    context.claim = Some(dispatch.claim.clone());
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
    let repo = &cp.audio_extract_operations;
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
    let staged_members = prepare_new_extract_members(NewExtractMemberInputs {
        operation: &paths.operation,
        staged: &staged,
        selection: &selection,
        staging_paths: &staging.paths,
        target_paths: &paths.targets,
        result: &result,
    })?;
    let commit_outputs =
        verify_and_probe_extract_members(cp, staged_members, verify, result_probe).await?;
    commit_verified_extract_audio_with_hooks(
        cp,
        ExtractCommitRequest {
            input,
            source_location_id: selected.location.id,
            source_media_snapshot_id: snapshot.id.0,
            operation_row_id: paths.operation.operation.id,
            selection,
            result,
            outputs: commit_outputs,
            claim: dispatch.claim.clone(),
        },
        claim_fence_hooks,
    )
    .await
}

async fn back_up_extract_source(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    selected: &source::SelectedSource,
) -> Result<(), VoomError> {
    crate::backup::maybe_back_up_source(
        cp,
        input.backup_root.as_deref(),
        &selected.canonical_path,
        input.source_file_version_id,
        input.job_id,
        input.ticket_id,
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
    let repo = &cp.audio_extract_operations;
    let attempt = if let Some(attempt) = &dispatch.replay_attempt {
        attempt.clone()
    } else {
        let attempt_directory = staging
            .paths
            .first()
            .and_then(|path| path.parent())
            .ok_or_else(|| {
                VoomError::Internal("audio extraction staging path has no parent".to_owned())
            })?
            .display()
            .to_string();
        repo.record_dispatch_attempt(
            &dispatch.claim,
            NewAudioExtractDispatchAttempt {
                worker_id: dispatch.worker_id,
                worker_epoch: dispatch.worker_epoch,
                idempotency_key: dispatch.idempotency_key.clone(),
                attempt_directory,
                paths: staging
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            },
            cp.clock().now(),
        )
        .await?
    };
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
    result_probe: &dyn commit::AudioResultProbeDispatcher,
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
        result_probe,
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

#[derive(Clone, Copy)]
struct ExtractResumeContext<'a> {
    cp: &'a ControlPlane,
    input: &'a ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    selection: &'a selection::ExtractAudioSelectionPlan,
    operation: &'a AudioExtractOperationRecord,
}

async fn maybe_resume_extract_operation(
    resume: &ExtractResumeContext<'_>,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::AudioResultProbeDispatcher,
    context: &mut ExtractAttemptContext,
    claim_fence_hooks: &dyn commit::ExtractClaimFenceHooks,
) -> Result<Option<ExecuteExtractAudioReport>, VoomError> {
    let ExtractResumeContext {
        cp,
        input,
        source_location_id,
        selection,
        operation,
    } = *resume;
    match operation.operation.state {
        AudioExtractOperationState::Committed => {
            committed_extract_report(input, source_location_id, selection, operation).map(Some)
        }
        AudioExtractOperationState::Prepared => {
            let (claim, _, _) = acquire_extract_claim(cp, input, operation).await?;
            context.claim = Some(claim.clone());
            commit::record_prepared_successor_evidence(cp, operation, &claim).await?;
            recover_extract_report(resume, &claim, claim_fence_hooks)
                .await
                .map(Some)
        }
        AudioExtractOperationState::RecoveryRequired => {
            let (claim, _, _) = acquire_extract_claim(cp, input, operation).await?;
            context.claim = Some(claim.clone());
            recover_extract_report(resume, &claim, claim_fence_hooks)
                .await
                .map(Some)
        }
        AudioExtractOperationState::Planned => Ok(None),
        AudioExtractOperationState::Staged => {
            let (claim, _, _) = acquire_extract_claim(cp, input, operation).await?;
            context.claim = Some(claim.clone());
            resume_staged_extract_report(resume, &claim, verify, result_probe, claim_fence_hooks)
                .await
                .map(Some)
        }
    }
}

async fn resume_staged_extract_report(
    resume: &ExtractResumeContext<'_>,
    claim: &NewAudioExtractClaim,
    verify: &dyn VerifyArtifactDispatcher,
    result_probe: &dyn commit::AudioResultProbeDispatcher,
    claim_fence_hooks: &dyn commit::ExtractClaimFenceHooks,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let ExtractResumeContext {
        cp,
        input,
        source_location_id,
        selection,
        operation,
    } = *resume;
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
    let staged_members = prepare_resumed_extract_members(operation, selection, &result, missing)?;
    let commit_outputs =
        verify_and_probe_extract_members(cp, staged_members, verify, result_probe).await?;
    commit_verified_extract_audio_with_hooks(
        cp,
        ExtractCommitRequest {
            input: input.clone(),
            source_location_id,
            source_media_snapshot_id: operation.operation.source_media_snapshot_id.0,
            operation_row_id: operation.operation.id,
            selection: selection.clone(),
            result,
            outputs: commit_outputs,
            claim: claim.clone(),
        },
        claim_fence_hooks,
    )
    .await
}

struct ExtractExecutionPaths {
    targets: Vec<PathBuf>,
    operation: AudioExtractOperationRecord,
}

struct ClaimedExtractDispatch {
    claim: NewAudioExtractClaim,
    worker_id: voom_core::WorkerId,
    worker_epoch: u32,
    idempotency_key: String,
    staging: stage::PreparedStagingPaths,
    replay_attempt: Option<voom_store::repo::audio_extract_operations::AudioExtractDispatchAttempt>,
}

fn extract_attempt_members(
    selection: &selection::ExtractAudioSelectionPlan,
    paths: &ExtractExecutionPaths,
) -> Vec<voom_events::payload::ArtifactAudioExtractMemberPayload> {
    let staging_paths: Vec<PathBuf> = paths
        .operation
        .outputs
        .iter()
        .filter_map(|output| output.staging_path.as_ref().map(PathBuf::from))
        .collect();
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
    result_probe: &dyn commit::AudioResultProbeDispatcher,
) -> Result<ExtractExecutionPaths, VoomError> {
    let targets = stage::extract_target_paths(&input.target_dir, source_path, selection).await?;
    let (new_operation, outputs) = new_extract_plan(
        input.source_file_version_id,
        input.source_bundle_id,
        source_media_snapshot_id,
        selection,
        &targets,
    );
    let repo = &cp.audio_extract_operations;
    if let Some(operation) = repo.get_exact_by_key(&new_operation, &outputs).await? {
        return Ok(ExtractExecutionPaths { targets, operation });
    }
    if outputs.len() == 1
        && let Some(operation) = commit::try_adopt_legacy_extract(
            cp,
            commit::LegacyExtractAdoptionInput {
                operation: new_operation.clone(),
                output: outputs[0].clone(),
                selection: selection.outputs[0].clone(),
            },
            result_probe,
        )
        .await?
    {
        return Ok(ExtractExecutionPaths { targets, operation });
    }
    let operation = repo
        .create_planned(new_operation, &outputs, cp.clock().now())
        .await?;
    Ok(ExtractExecutionPaths { targets, operation })
}

pub(crate) async fn plan_first_extract_with_bundle(
    cp: &ControlPlane,
    input: FirstExtractPlanInput,
) -> Result<voom_core::BundleId, VoomError> {
    let selected =
        source::select_source(cp, input.source_file_version_id, input.source_location_id).await?;
    let snapshot =
        source::read_media_snapshot(cp, input.source_file_version_id, &input.operation_payload)
            .await?;
    let selection = selection::extract_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )?;
    let source_path = Path::new(&selected.location.value);
    let targets = stage::extract_target_paths(&input.target_dir, source_path, &selection).await?;
    let mut tx = crate::cases::begin_immediate_tx(&cp.pool).await?;
    let resolution = cp
        .resolve_or_create_primary_bundle_in_tx(
            &mut tx,
            input.source_file_version_id,
            source_path,
            cp.clock().now(),
        )
        .await?;
    let (operation, outputs) = new_extract_plan(
        input.source_file_version_id,
        resolution.bundle_id,
        snapshot.id.0,
        &selection,
        &targets,
    );
    SqliteAudioExtractOperationRepo::create_planned_in_tx(
        &mut tx,
        &operation,
        &outputs,
        cp.clock().now(),
    )
    .await?;
    crate::cases::commit_tx(tx).await?;
    Ok(resolution.bundle_id)
}

fn new_extract_plan(
    source_file_version_id: FileVersionId,
    source_bundle_id: voom_core::BundleId,
    source_media_snapshot_id: u64,
    selection: &selection::ExtractAudioSelectionPlan,
    targets: &[PathBuf],
) -> (NewAudioExtractOperation, Vec<NewAudioExtractOutput>) {
    let operation_key = extract_operation_token(
        source_file_version_id,
        source_media_snapshot_id,
        selection.operation_id.as_deref(),
        targets,
    );
    let outputs = selection
        .outputs
        .iter()
        .zip(targets)
        .map(|(output, target)| NewAudioExtractOutput {
            output_id: output.output_id.clone(),
            source_snapshot_stream_id: output.stream.snapshot_stream_id.clone(),
            source_provider_stream_index: output.stream.provider_stream_index,
            bundle_role: commit::bundle_role(output.role).as_str().to_owned(),
            target_path: target.display().to_string(),
        })
        .collect();
    (
        NewAudioExtractOperation {
            operation_key,
            operation_id: selection.operation_id.clone(),
            source_file_version_id,
            source_bundle_id,
            source_media_snapshot_id: MediaSnapshotId(source_media_snapshot_id),
        },
        outputs,
    )
}

async fn claim_extract_dispatch(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    operation: &AudioExtractOperationRecord,
    targets: &[PathBuf],
) -> Result<ClaimedExtractDispatch, VoomError> {
    let now = cp.clock().now();
    let (claim, worker_id, worker_epoch) = acquire_extract_claim(cp, input, operation).await?;
    let repo = &cp.audio_extract_operations;
    if let Some(attempt) = repo
        .get_dispatch_attempt(operation.operation.id, claim.expected_generation)
        .await?
    {
        let expected = stage::resolve_extract_staging_paths(
            &input.staging_root,
            &operation.operation.operation_key,
            operation.operation.dispatch_generation,
            targets,
        )
        .await?;
        return reconcile_prior_dispatch_attempt(
            repo,
            &claim,
            attempt,
            worker_id,
            worker_epoch,
            &expected,
            now,
        )
        .await;
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
        replay_attempt: None,
    })
}

async fn acquire_extract_claim(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    operation: &AudioExtractOperationRecord,
) -> Result<(NewAudioExtractClaim, voom_core::WorkerId, u32), VoomError> {
    let (worker_id, worker_epoch, expires_at) = audio_dispatch_lease(cp, input.lease_id).await?;
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
    cp.audio_extract_operations
        .acquire_claim(&claim, cp.clock().now())
        .await?;
    Ok((claim, worker_id, worker_epoch))
}

async fn audio_dispatch_lease(
    cp: &ControlPlane,
    lease_id: LeaseId,
) -> Result<(voom_core::WorkerId, u32, time::OffsetDateTime), VoomError> {
    let row = sqlx::query(
        "SELECT leases.expires_at, workers.id AS worker_id, workers.epoch AS worker_epoch \
         FROM leases JOIN workers ON workers.id = leases.worker_id \
         WHERE leases.id = ? AND leases.state = 'held'",
    )
    .bind(i64::try_from(lease_id.0).map_err(|error| {
        VoomError::Config(format!("audio dispatch lease id is invalid: {error}"))
    })?)
    .fetch_optional(&cp.pool)
    .await
    .map_err(|error| VoomError::database_context("audio dispatch lease", error))?
    .ok_or_else(|| {
        VoomError::Conflict(format!("audio dispatch lease {} is not held", lease_id.0))
    })?;
    let expires_at: String = row.try_get("expires_at").map_err(|error| {
        VoomError::database_context("audio dispatch lease expiry decode", error)
    })?;
    let expires_at =
        time::OffsetDateTime::parse(&expires_at, &time::format_description::well_known::Rfc3339)
            .map_err(|error| {
                VoomError::database(format!("audio dispatch lease expiry: {error}"))
            })?;
    let worker_epoch: i64 = row
        .try_get("worker_epoch")
        .map_err(|error| VoomError::database_context("audio worker epoch decode", error))?;
    Ok((
        voom_core::WorkerId(
            u64::try_from(
                row.try_get::<i64, _>("worker_id").map_err(|error| {
                    VoomError::database_context("audio worker id decode", error)
                })?,
            )
            .map_err(|error| VoomError::database(format!("audio worker id: {error}")))?,
        ),
        u32::try_from(worker_epoch)
            .map_err(|error| VoomError::database(format!("audio worker epoch: {error}")))?,
        expires_at,
    ))
}

async fn reconcile_prior_dispatch_attempt(
    repo: &SqliteAudioExtractOperationRepo,
    claim: &NewAudioExtractClaim,
    mut attempt: voom_store::repo::media::audio_extract_operations::AudioExtractDispatchAttempt,
    worker_id: voom_core::WorkerId,
    worker_epoch: u32,
    expected: &stage::PreparedStagingPaths,
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
            if attempt.worker_id == worker_id && attempt.worker_epoch == worker_epoch =>
        {
            let staging = match replay_staging_paths(&attempt, expected) {
                Ok(staging) => staging,
                Err(error) => {
                    repo.quarantine_dispatch(claim, attempt.id, now).await?;
                    repo.release_claim(claim).await?;
                    return Err(error);
                }
            };
            Ok(ClaimedExtractDispatch {
                claim: claim.clone(),
                worker_id,
                worker_epoch,
                idempotency_key: attempt.idempotency_key.clone(),
                staging,
                replay_attempt: Some(attempt),
            })
        }
        AudioExtractDispatchAttemptStatus::Active => {
            repo.quarantine_dispatch(claim, attempt.id, now).await?;
            repo.release_claim(claim).await?;
            attempt.status = AudioExtractDispatchAttemptStatus::Quarantined;
            Err(dispatch_quiescence_required(&attempt))
        }
        AudioExtractDispatchAttemptStatus::Quarantined => {
            repo.release_claim(claim).await?;
            Err(dispatch_quiescence_required(&attempt))
        }
    }
}

fn replay_staging_paths(
    attempt: &voom_store::repo::media::audio_extract_operations::AudioExtractDispatchAttempt,
    expected: &stage::PreparedStagingPaths,
) -> Result<stage::PreparedStagingPaths, VoomError> {
    let attempt_directory = PathBuf::from(&attempt.attempt_directory);
    let paths = attempt.paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    let expected_directory = expected.paths.first().and_then(|path| path.parent());
    if expected_directory != Some(attempt_directory.as_path())
        || paths != expected.paths
        || paths
            .iter()
            .any(|path| path.parent() != Some(attempt_directory.as_path()))
    {
        return Err(VoomError::Conflict(format!(
            "audio extraction attempt {} does not match its deterministic ordered staging paths",
            attempt.id
        )));
    }
    Ok(expected.clone())
}

fn dispatch_quiescence_required(
    attempt: &voom_store::repo::media::audio_extract_operations::AudioExtractDispatchAttempt,
) -> VoomError {
    VoomError::Conflict(format!(
        "audio extraction attempt {} is {:?}; worker {} epoch {} must prove terminal \
         completion or be explicitly quiesced before retry (key {})",
        attempt.id,
        attempt.status,
        attempt.worker_id.0,
        attempt.worker_epoch,
        attempt.idempotency_key
    ))
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
    execution_report_from_outputs(input, source_location_id, outputs, None)
}

async fn recover_extract_report(
    resume: &ExtractResumeContext<'_>,
    claim: &NewAudioExtractClaim,
    claim_fence_hooks: &dyn commit::ExtractClaimFenceHooks,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let ExtractResumeContext {
        cp,
        input,
        source_location_id,
        selection,
        operation,
    } = *resume;
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
        .collect::<Result<Vec<_>, _>>();
    let outputs = match outputs {
        Ok(outputs) => outputs,
        Err(error) => {
            return Err(
                commit::record_recovery_projection_failure(cp, operation, claim, error).await,
            );
        }
    };
    let committed = commit::recover_audio_extract_set_with_hooks(
        cp,
        &commit::CommitAudioExtractSetInput {
            operation_row_id: operation.operation.id,
            source_file_version_id: input.source_file_version_id,
            source_media_snapshot_id: operation.operation.source_media_snapshot_id,
            source_bundle_id: input.source_bundle_id,
            outputs,
            claim: claim.clone(),
        },
        claim_fence_hooks,
    )
    .await?;
    let outputs = extract_output_reports(
        selection,
        &committed,
        input.source_file_version_id,
        operation.operation.source_media_snapshot_id,
    )?;
    execution_report_from_outputs(input, source_location_id, outputs, None)
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
    commit_recovery_required: Option<commit::AudioExtractRecoveryReport>,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let first = outputs
        .first()
        .ok_or_else(|| VoomError::Internal("audio extraction returned no outputs".to_owned()))?;
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
        commit_recovery_required,
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

struct StagedExtractMember {
    operation_output_id: u64,
    artifact: commit::StagedAudioArtifact,
    role: voom_plan::audio::AudioBundleRole,
    source_snapshot_stream_id: String,
    source_provider_stream_index: u32,
    staging_path: PathBuf,
    target_path: PathBuf,
    output: voom_worker_protocol::AudioObservedFacts,
}

struct VerifiedExtractMember {
    staged: StagedExtractMember,
    verification_id: ArtifactVerificationId,
}

#[derive(Clone, Copy)]
struct NewExtractMemberInputs<'a> {
    operation: &'a AudioExtractOperationRecord,
    staged: &'a [commit::StagedAudioArtifact],
    selection: &'a selection::ExtractAudioSelectionPlan,
    staging_paths: &'a [PathBuf],
    target_paths: &'a [PathBuf],
    result: &'a ExtractAudioResult,
}

fn prepare_new_extract_members(
    input: NewExtractMemberInputs<'_>,
) -> Result<Vec<StagedExtractMember>, VoomError> {
    let output_facts = worker_contract::extract_result_output_facts(input.result);
    let expected_count = input.selection.outputs.len();
    if input.operation.outputs.len() != expected_count
        || input.staged.len() != expected_count
        || input.staging_paths.len() != expected_count
        || input.target_paths.len() != expected_count
        || output_facts.len() != expected_count
    {
        return Err(VoomError::Internal(
            "audio extraction staged member inputs have inconsistent output counts".to_owned(),
        ));
    }
    let mut members = Vec::with_capacity(expected_count);
    for (index, selected) in input.selection.outputs.iter().enumerate() {
        members.push(staged_extract_member(StagedExtractMemberInput {
            operation: &input.operation.outputs[index],
            artifact: &input.staged[index],
            selected,
            staging_path: &input.staging_paths[index],
            target_path: &input.target_paths[index],
            output: output_facts[index],
        }));
    }
    Ok(members)
}

fn prepare_resumed_extract_members(
    operation: &AudioExtractOperationRecord,
    selection: &selection::ExtractAudioSelectionPlan,
    result: &ExtractAudioResult,
    missing: impl Fn(u64, &str) -> VoomError,
) -> Result<Vec<StagedExtractMember>, VoomError> {
    let output_facts = worker_contract::extract_result_output_facts(result);
    if output_facts.len() != operation.outputs.len() {
        return Err(VoomError::Internal(format!(
            "staged audio extraction {} has inconsistent worker outputs",
            operation.operation.operation_key
        )));
    }
    let mut members = Vec::with_capacity(operation.outputs.len());
    for (index, output) in operation.outputs.iter().enumerate() {
        let artifact = commit::StagedAudioArtifact {
            artifact_handle_id: output
                .artifact_handle_id
                .ok_or_else(|| missing(output.id, "artifact_handle_id"))?,
            artifact_location_id: output
                .artifact_location_id
                .ok_or_else(|| missing(output.id, "artifact_location_id"))?,
        };
        let staging_path = PathBuf::from(
            output
                .staging_path
                .as_ref()
                .ok_or_else(|| missing(output.id, "staging_path"))?,
        );
        members.push(staged_extract_member(StagedExtractMemberInput {
            operation: output,
            artifact: &artifact,
            selected: &selection.outputs[index],
            staging_path: &staging_path,
            target_path: Path::new(&output.target_path),
            output: output_facts[index],
        }));
    }
    Ok(members)
}

#[derive(Clone, Copy)]
struct StagedExtractMemberInput<'a> {
    operation: &'a voom_store::repo::audio_extract_operations::AudioExtractOperationOutput,
    artifact: &'a commit::StagedAudioArtifact,
    selected: &'a selection::ExtractAudioSelectionOutput,
    staging_path: &'a Path,
    target_path: &'a Path,
    output: &'a voom_worker_protocol::AudioObservedFacts,
}

fn staged_extract_member(input: StagedExtractMemberInput<'_>) -> StagedExtractMember {
    StagedExtractMember {
        operation_output_id: input.operation.id,
        artifact: input.artifact.clone(),
        role: input.selected.role,
        source_snapshot_stream_id: input.selected.stream.snapshot_stream_id.clone(),
        source_provider_stream_index: input.selected.stream.provider_stream_index,
        staging_path: input.staging_path.to_owned(),
        target_path: input.target_path.to_owned(),
        output: input.output.clone(),
    }
}

async fn verify_and_probe_extract_members(
    cp: &ControlPlane,
    members: Vec<StagedExtractMember>,
    verify: &dyn VerifyArtifactDispatcher,
    probe: &dyn commit::AudioResultProbeDispatcher,
) -> Result<Vec<commit::CommitAudioExtractOutputInput>, VoomError> {
    let mut verified = Vec::with_capacity(members.len());
    for member in members {
        let verification_id =
            verify_staged_extract(cp, &member.artifact, &member.staging_path, verify).await?;
        verified.push(VerifiedExtractMember {
            staged: member,
            verification_id,
        });
    }
    let mut outputs = Vec::with_capacity(verified.len());
    for member in verified {
        let StagedExtractMember {
            operation_output_id,
            artifact,
            role,
            source_snapshot_stream_id,
            source_provider_stream_index,
            staging_path,
            target_path,
            output,
        } = member.staged;
        let probed = commit::probe_staged_extract_result(cp, &staging_path, &output, probe).await?;
        outputs.push(commit::CommitAudioExtractOutputInput {
            operation_output_id,
            artifact_handle_id: artifact.artifact_handle_id,
            artifact_location_id: artifact.artifact_location_id,
            verification_id: member.verification_id,
            role,
            source_snapshot_stream_id,
            source_provider_stream_index,
            staging_path,
            target_path,
            prepared_temp_path: None,
            prepared_commit_record_id: None,
            output,
            probed,
        });
    }
    Ok(outputs)
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
    claim: Option<NewAudioExtractClaim>,
}

struct ExtractCommitRequest {
    input: ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    source_media_snapshot_id: u64,
    operation_row_id: u64,
    selection: selection::ExtractAudioSelectionPlan,
    result: ExtractAudioResult,
    outputs: Vec<commit::CommitAudioExtractOutputInput>,
    claim: NewAudioExtractClaim,
}

async fn commit_verified_extract_audio_with_hooks(
    cp: &ControlPlane,
    mut request: ExtractCommitRequest,
    claim_fence_hooks: &dyn commit::ExtractClaimFenceHooks,
) -> Result<ExecuteExtractAudioReport, VoomError> {
    let outputs = std::mem::take(&mut request.outputs);
    let committed = commit::commit_audio_extract_set_with_hooks(
        cp,
        &commit::CommitAudioExtractSetInput {
            operation_row_id: request.operation_row_id,
            source_file_version_id: request.input.source_file_version_id,
            source_media_snapshot_id: MediaSnapshotId(request.source_media_snapshot_id),
            source_bundle_id: request.input.source_bundle_id,
            outputs,
            claim: request.claim.clone(),
        },
        claim_fence_hooks,
    )
    .await?;
    complete_extract_report(cp, request, committed).await
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
    execution_report_from_outputs(
        &request.input,
        request.source_location_id,
        report_outputs,
        commit_recovery_required,
    )
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
        role: commit::bundle_role(role).as_str(),
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
            role: commit::bundle_role(selection.role).as_str().to_owned(),
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

fn audio_payload_snapshot_id(payload: &serde_json::Value) -> Option<u64> {
    payload
        .get("source_media_snapshot_id")
        .and_then(serde_json::Value::as_u64)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
