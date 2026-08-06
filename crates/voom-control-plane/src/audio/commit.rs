use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::json;
use voom_artifact::commit_pipeline::{
    PendingCommitRecordError, RecoveryRequiredCommit, append_commit_event_in_tx,
    create_pending_commit_with_started_event_in_tx, mark_recovery_required_with_event_in_tx,
};
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId, BundleId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FailureClass, FileLocationId, FileVersionId,
    MediaSnapshotId, ProviderRelativeLocator, StorageRootId, UseLeaseId, VoomError, WorkerId,
};
use voom_events::payload::{
    ArtifactCommitCompletedPayload, ArtifactCommitRecoveryRequiredPayload,
    ArtifactCommitStartedPayload, ArtifactStagedPayload,
};
use voom_events::{Event, SubjectType};
use voom_plan::planner::audio::AudioBundleRole;
use voom_store::repo::media::artifacts::{
    ArtifactCommitFailure, ArtifactCommitRecord, ArtifactCommitState, ArtifactHandleAccessMode,
    ArtifactLocationKind, NewArtifactCommitRecord, NewArtifactHandle, NewArtifactLocation,
    NewSidecarArtifactCommit, SidecarArtifactCommit,
};
use voom_store::repo::media::audio_extract_operations::{
    AudioExtractOperationRecord, AudioExtractRecoveryFailure, LegacyAudioExtractOwner,
    NewAudioExtractClaim, NewAudioExtractOperation, NewAudioExtractOutput,
    NewFinalizedAudioExtractOutput, NewLegacyAudioExtractAdoption, NewPreparedAudioExtractOutput,
    NewStagedAudioExtractOutput, SqliteAudioExtractOperationRepo, StageAudioExtractOperation,
};
use voom_store::repo::media::audio_synthesis_operations::{
    BindAudioSynthesisOperation, NewAudioSynthesisClaim, SqliteAudioSynthesisOperationRepo,
    StagedAudioSynthesisCompanion,
};
use voom_store::repo::media::bundles::{BundleMemberRole, NewBundleMember};
use voom_store::repo::media::identity::{FileVersionRepo, MediaSnapshot, NewMediaSnapshot};
use voom_worker_protocol::{
    AudioObservedFacts, AudioOutputStreamFact, ExpectedFileFacts, ExtractAudioResult,
    ProbeFileRequest, ProbeFileResult, TranscodeAudioResult,
};

use super::selection::{
    ExtractAudioSelectionOutput, ExtractAudioSelectionPlan, TranscodeAudioSelectionPlan,
};
use super::worker_contract::extract_result_output_facts;
use super::{ExecuteExtractAudioInput, ExecuteTranscodeAudioInput};
use crate::ControlPlane;
use crate::artifact::fs::{
    ArtifactFileFacts, canonical_new_leaf_no_symlink, promote_staged_add_only_with_temp,
    recover_staged_add_only_with_temp, require_expected_staging_facts, unique_temp_sibling_path,
};
use crate::cases::{append_event, begin_immediate_tx, begin_tx, commit_tx};
use crate::scan::persist::{ObservedCandidateFacts, snapshot_with_stream_ids, verify_probe_facts};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedAudioArtifact {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitAudioExtractOutputInput {
    pub operation_output_id: u64,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub role: AudioBundleRole,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
    pub prepared_temp_path: Option<PathBuf>,
    pub prepared_commit_record_id: Option<ArtifactCommitRecordId>,
    pub output: AudioObservedFacts,
    pub(crate) probed: ProbedResultPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitAudioExtractSetInput {
    pub operation_row_id: u64,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_bundle_id: BundleId,
    pub outputs: Vec<CommitAudioExtractOutputInput>,
    pub claim: NewAudioExtractClaim,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExtractClaimFenceContext<'a> {
    pub boundary_index: usize,
    pub member_count: usize,
    pub claim: &'a NewAudioExtractClaim,
}

#[async_trait]
pub(crate) trait ExtractClaimFenceHooks: Send + Sync {
    async fn after_prepare_gate(&self) -> Result<(), VoomError> {
        Ok(())
    }

    async fn before_assert(
        &self,
        _cp: &ControlPlane,
        context: ExtractClaimFenceContext<'_>,
    ) -> Result<(), VoomError> {
        let _ = (context.boundary_index, context.member_count, context.claim);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NoExtractClaimFenceHooks;

#[async_trait]
impl ExtractClaimFenceHooks for NoExtractClaimFenceHooks {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedAudioExtractOutput {
    pub operation_output_id: u64,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_file_asset_id: u64,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub lineage_id: u64,
    pub bundle_member_id: u64,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
    pub temp_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AudioExtractRecoveryReport {
    pub recovery_reason: String,
    pub commit_record_id: ArtifactCommitRecordId,
    pub source_bundle_id: BundleId,
    pub role: &'static str,
    pub target_path: PathBuf,
    pub target_exists: bool,
    pub temp_path: PathBuf,
    pub temp_exists: bool,
    pub staging_path: PathBuf,
    pub staging_exists: bool,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
    pub error_code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProbedAudioResult {
    pub worker_id: WorkerId,
    pub result: ProbeFileResult,
}

#[async_trait]
pub(crate) trait AudioResultProbeDispatcher: Send + Sync {
    async fn dispatch_result_probe(
        &self,
        cp: &ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<ProbedAudioResult, VoomError>;

    fn start_session(&self) -> Box<dyn AudioResultProbeSession + '_> {
        Box::new(PerDispatchAudioResultProbeSession { dispatcher: self })
    }
}

#[async_trait]
pub(crate) trait AudioResultProbeSession: AudioResultProbeDispatcher {
    async fn shutdown(self: Box<Self>);
}

struct PerDispatchAudioResultProbeSession<
    'a,
    D: AudioResultProbeDispatcher + ?Sized = dyn AudioResultProbeDispatcher + 'a,
> {
    dispatcher: &'a D,
}

#[async_trait]
impl<D> AudioResultProbeDispatcher for PerDispatchAudioResultProbeSession<'_, D>
where
    D: AudioResultProbeDispatcher + ?Sized,
{
    async fn dispatch_result_probe(
        &self,
        cp: &ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<ProbedAudioResult, VoomError> {
        self.dispatcher.dispatch_result_probe(cp, request).await
    }

    fn start_session(&self) -> Box<dyn AudioResultProbeSession + '_> {
        Box::new(Self {
            dispatcher: self.dispatcher,
        })
    }
}

#[async_trait]
impl<D> AudioResultProbeSession for PerDispatchAudioResultProbeSession<'_, D>
where
    D: AudioResultProbeDispatcher + ?Sized,
{
    async fn shutdown(self: Box<Self>) {}
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BundledAudioResultProbeDispatcher;

#[async_trait]
impl AudioResultProbeDispatcher for BundledAudioResultProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        cp: &ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<ProbedAudioResult, VoomError> {
        let worker_id = ensure_result_probe_worker(cp).await?;
        let mut worker =
            crate::scan::worker::BundledWorkerProcess::launch_bundled_ffprobe(worker_id)
                .await
                .map_err(|err| result_probe_worker_error(&err))?;
        let result = worker
            .dispatch_probe_file(request)
            .await
            .map_err(|err| result_probe_worker_error(&err))?;
        let _shutdown = worker.shutdown(Duration::from_secs(5)).await;
        Ok(ProbedAudioResult { worker_id, result })
    }

    fn start_session(&self) -> Box<dyn AudioResultProbeSession + '_> {
        Box::new(BundledAudioResultProbeSession::default())
    }
}

#[derive(Debug, Default)]
struct BundledAudioResultProbeSession {
    worker: tokio::sync::Mutex<Option<(WorkerId, crate::scan::worker::BundledWorkerProcess)>>,
}

#[async_trait]
impl AudioResultProbeDispatcher for BundledAudioResultProbeSession {
    async fn dispatch_result_probe(
        &self,
        cp: &ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<ProbedAudioResult, VoomError> {
        let mut session_worker = self.worker.lock().await;
        let (worker_id, result) = {
            let (worker_id, worker) = if let Some((worker_id, worker)) = session_worker.as_mut() {
                (*worker_id, worker)
            } else {
                let worker_id = ensure_result_probe_worker(cp).await?;
                let launched =
                    crate::scan::worker::BundledWorkerProcess::launch_bundled_ffprobe(worker_id)
                        .await
                        .map_err(|error| result_probe_worker_error(&error))?;
                let (worker_id, worker) = session_worker.insert((worker_id, launched));
                (*worker_id, worker)
            };
            (worker_id, worker.dispatch_probe_file(request).await)
        };
        if result
            .as_ref()
            .is_err_and(crate::scan::worker::ScanWorkerError::should_shutdown_worker)
        {
            *session_worker = None;
        }
        let result = result.map_err(|error| result_probe_worker_error(&error))?;
        Ok(ProbedAudioResult { worker_id, result })
    }
}

#[async_trait]
impl AudioResultProbeSession for BundledAudioResultProbeSession {
    async fn shutdown(self: Box<Self>) {
        let Some((_worker_id, worker)) = self.worker.into_inner() else {
            return;
        };
        if let Err(error) = worker.shutdown(Duration::from_secs(5)).await {
            tracing::warn!(%error, "audio result probe worker session shutdown failed");
        }
    }
}

pub async fn record_staged_audio_transcode(
    cp: &ControlPlane,
    input: &ExecuteTranscodeAudioInput,
    source_file_location_id: FileLocationId,
    staging_path: &Path,
    result: &TranscodeAudioResult,
) -> Result<StagedAudioArtifact, VoomError> {
    record_staged_audio(
        cp,
        input.source_file_version_id,
        source_file_location_id,
        staging_path,
        result.output.size_bytes,
        &result.output.content_hash,
        json!({
            "operation": "transcode_audio",
            "source_file_version_id": input.source_file_version_id.0,
            "source_file_location_id": source_file_location_id.0,
            "selected_snapshot_stream_ids": result.selected_snapshot_stream_ids,
        }),
    )
    .await
}

pub struct StageAudioSynthesisArtifactInput<'a> {
    pub execution: &'a ExecuteTranscodeAudioInput,
    pub source_file_location_id: FileLocationId,
    pub staging_path: &'a Path,
    pub operation_id: u64,
    pub claim: &'a NewAudioSynthesisClaim,
    pub result: &'a TranscodeAudioResult,
    pub companions: Vec<StagedAudioSynthesisCompanion>,
}

pub async fn record_staged_audio_synthesis(
    cp: &ControlPlane,
    input: StageAudioSynthesisArtifactInput<'_>,
) -> Result<StagedAudioArtifact, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let artifact = record_staged_audio_in_tx(
        cp,
        &mut tx,
        NewStagedAudioArtifact {
            source_file_version_id: input.execution.source_file_version_id,
            source_file_location_id: input.source_file_location_id,
            staging_path: input.staging_path,
            size_bytes: input.result.output.size_bytes,
            checksum: &input.result.output.content_hash,
            lineage: json!({
                "operation": "synthesize_audio",
                "source_file_version_id": input.execution.source_file_version_id.0,
                "source_file_location_id": input.source_file_location_id.0,
                "selected_snapshot_stream_ids": input.result.selected_snapshot_stream_ids,
            }),
        },
        now,
    )
    .await?;
    SqliteAudioSynthesisOperationRepo::bind_staged_in_tx(
        &mut tx,
        &BindAudioSynthesisOperation {
            operation_id: input.operation_id,
            claim: input.claim.clone(),
            staging_path: input.staging_path.display().to_string(),
            expected_size_bytes: input.result.output.size_bytes,
            expected_checksum: input.result.output.content_hash.clone(),
            worker_result: serde_json::to_value(input.result).map_err(|error| {
                VoomError::Internal(format!("encode audio synthesis result: {error}"))
            })?,
            artifact_handle_id: artifact.artifact_handle_id,
            artifact_location_id: artifact.artifact_location_id,
            companions: input.companions,
        },
        now,
    )
    .await?;
    commit_tx(tx).await?;
    Ok(artifact)
}

pub struct StageAudioExtractSetInput<'a> {
    pub execution: &'a ExecuteExtractAudioInput,
    pub source_file_location_id: FileLocationId,
    pub staging_paths: &'a [PathBuf],
    pub operation: &'a AudioExtractOperationRecord,
    pub selection: &'a ExtractAudioSelectionPlan,
    pub result: &'a ExtractAudioResult,
    pub claim: &'a NewAudioExtractClaim,
}

pub async fn record_staged_audio_extract_set(
    cp: &ControlPlane,
    input: StageAudioExtractSetInput<'_>,
) -> Result<Vec<StagedAudioArtifact>, VoomError> {
    let StageAudioExtractSetInput {
        execution,
        source_file_location_id,
        staging_paths,
        operation,
        selection,
        result,
        claim,
    } = input;
    let result_outputs = extract_result_output_facts(result);
    if staging_paths.len() != selection.outputs.len()
        || staging_paths.len() != result_outputs.len()
        || staging_paths.len() != operation.outputs.len()
    {
        return Err(VoomError::MalformedWorkerResult(
            "audio extract staging inputs have inconsistent output counts".to_owned(),
        ));
    }
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let mut staged = Vec::with_capacity(staging_paths.len());
    let mut bindings = Vec::with_capacity(staging_paths.len());
    for (((path, selected), output), operation_output) in staging_paths
        .iter()
        .zip(&selection.outputs)
        .zip(result_outputs)
        .zip(&operation.outputs)
    {
        let artifact = record_staged_audio_in_tx(
            cp,
            &mut tx,
            NewStagedAudioArtifact {
                source_file_version_id: execution.source_file_version_id,
                source_file_location_id,
                staging_path: path,
                size_bytes: output.size_bytes,
                checksum: &output.content_hash,
                lineage: json!({
                    "operation": "extract_audio",
                    "operation_id": selection.operation_id,
                    "output_id": selected.output_id,
                    "source_file_version_id": execution.source_file_version_id.0,
                    "source_file_location_id": source_file_location_id.0,
                    "source_snapshot_stream_id": selected.stream.snapshot_stream_id,
                    "source_provider_stream_index": selected.stream.provider_stream_index,
                    "intended_role": bundle_role(selected.role).as_str(),
                }),
            },
            now,
        )
        .await?;
        bindings.push(NewStagedAudioExtractOutput {
            operation_output_id: operation_output.id,
            staging_path: path.display().to_string(),
            expected_size_bytes: output.size_bytes,
            expected_checksum: output.content_hash.clone(),
            artifact_handle_id: artifact.artifact_handle_id,
            artifact_location_id: artifact.artifact_location_id,
            result_facts: serde_json::to_value(output).map_err(|error| {
                VoomError::Internal(format!("serialize staged audio extraction output: {error}"))
            })?,
        });
        staged.push(artifact);
    }
    let worker_result = serde_json::to_value(result).map_err(|error| {
        VoomError::Internal(format!("serialize staged audio extraction result: {error}"))
    })?;
    SqliteAudioExtractOperationRepo::stage_operation_in_tx(
        &mut tx,
        StageAudioExtractOperation {
            operation_id: operation.operation.id,
            claim,
            worker_result: &worker_result,
            outputs: &bindings,
            observed_at: cp.clock().now(),
        },
    )
    .await?;
    commit_tx(tx).await?;
    Ok(staged)
}

/// The normalized media-snapshot payload probed from the staged artifact (with
/// audio output facts merged in), paired with the probe worker so the
/// post-commit record step can attribute it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbedResultPayload {
    pub worker_id: WorkerId,
    pub payload: serde_json::Value,
}

pub(crate) struct LegacyExtractAdoptionInput {
    pub operation: NewAudioExtractOperation,
    pub output: NewAudioExtractOutput,
    pub selection: ExtractAudioSelectionOutput,
}

pub(crate) async fn try_adopt_legacy_extract(
    cp: &ControlPlane,
    input: LegacyExtractAdoptionInput,
    dispatcher: &dyn AudioResultProbeDispatcher,
) -> Result<Option<AudioExtractOperationRecord>, VoomError> {
    let target = PathBuf::from(&input.output.target_path);
    let target_exists = tokio::fs::symlink_metadata(&target).await.is_ok();
    let repo = &cp.audio_extract_operations;
    let owner = repo
        .legacy_committed_owner(
            &input.output.target_path,
            input.operation.source_bundle_id,
            &input.output.bundle_role,
        )
        .await?;
    let Some(owner) = owner else {
        if target_exists {
            return Err(VoomError::Conflict(format!(
                "audio extraction target {} exists without a committed artifact owner",
                target.display()
            )));
        }
        return Ok(None);
    };
    if !target_exists {
        return Err(VoomError::Conflict(format!(
            "committed legacy audio extraction {} owns missing target {}",
            owner.commit_record_id,
            target.display()
        )));
    }
    let observed = validate_legacy_extract_owner(cp, &input, &owner, &target).await?;
    let probed = probe_staged_extract_result(
        cp,
        &target,
        &AudioObservedFacts {
            size_bytes: observed.size_bytes,
            content_hash: observed.content_hash.clone(),
            modified_at: None,
            local_file_key: observed.local_file_key.clone(),
        },
        dispatcher,
    )
    .await?;
    insert_legacy_extract_adoption(cp, &input, &owner, &observed, probed)
        .await
        .map(Some)
}

async fn validate_legacy_extract_owner(
    cp: &ControlPlane,
    input: &LegacyExtractAdoptionInput,
    owner: &LegacyAudioExtractOwner,
    target: &Path,
) -> Result<ArtifactFileFacts, VoomError> {
    require_legacy_selection(input, owner)?;
    require_legacy_lineage(input, owner)?;
    require_legacy_artifact_owner(input, owner)?;
    require_legacy_result(cp, owner, target).await?;
    require_legacy_bundle(input, owner)?;
    let observed = crate::artifact::fs::observe_regular_file(target).await?;
    if observed.size_bytes != owner.result_size_bytes
        || observed.content_hash != owner.result_checksum
    {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "legacy audio extraction target {} differs from committed owner {}",
            target.display(),
            owner.commit_record_id
        )));
    }
    Ok(observed)
}

fn require_legacy_selection(
    input: &LegacyExtractAdoptionInput,
    owner: &LegacyAudioExtractOwner,
) -> Result<(), VoomError> {
    require_legacy_evidence(
        input.selection.output_id == input.output.output_id,
        owner,
        "selection output id differs from the requested output",
    )?;
    require_legacy_evidence(
        input.selection.stream.snapshot_stream_id == input.output.source_snapshot_stream_id,
        owner,
        "selection stream id differs from the requested output",
    )?;
    require_legacy_evidence(
        input.selection.stream.provider_stream_index == input.output.source_provider_stream_index,
        owner,
        "selection stream index differs from the requested output",
    )?;
    require_legacy_evidence(
        bundle_role(input.selection.role).as_str() == input.output.bundle_role,
        owner,
        "selection role differs from the requested bundle role",
    )
}

fn require_legacy_lineage(
    input: &LegacyExtractAdoptionInput,
    owner: &LegacyAudioExtractOwner,
) -> Result<(), VoomError> {
    let lineage = &owner.source_lineage;
    require_legacy_evidence(
        optional_lineage_identity(
            lineage,
            "operation_id",
            input.operation.operation_id.as_deref(),
        ),
        owner,
        "lineage operation id differs from the requested operation",
    )?;
    require_legacy_evidence(
        optional_lineage_identity(lineage, "output_id", input.output.output_id.as_deref()),
        owner,
        "lineage output id differs from the requested output",
    )?;
    require_legacy_evidence(
        lineage.get("operation").and_then(serde_json::Value::as_str) == Some("extract_audio"),
        owner,
        "lineage operation is not extract_audio",
    )?;
    require_legacy_evidence(
        lineage
            .get("source_file_version_id")
            .and_then(serde_json::Value::as_u64)
            == Some(input.operation.source_file_version_id.0),
        owner,
        "lineage source file version differs from the requested source",
    )?;
    let stream_id = lineage
        .get("source_snapshot_stream_id")
        .or_else(|| lineage.get("selected_snapshot_stream_id"))
        .and_then(serde_json::Value::as_str);
    require_legacy_evidence(
        stream_id == Some(&input.output.source_snapshot_stream_id),
        owner,
        "lineage stream id differs from the requested output",
    )?;
    let stream_index = lineage
        .get("source_provider_stream_index")
        .and_then(serde_json::Value::as_u64);
    require_legacy_evidence(
        stream_index
            .is_none_or(|index| index == u64::from(input.output.source_provider_stream_index)),
        owner,
        "lineage stream index differs from the requested output",
    )?;
    require_legacy_evidence(
        lineage
            .get("intended_role")
            .and_then(serde_json::Value::as_str)
            == Some(&input.output.bundle_role),
        owner,
        "lineage role differs from the requested bundle role",
    )
}

fn require_legacy_artifact_owner(
    input: &LegacyExtractAdoptionInput,
    owner: &LegacyAudioExtractOwner,
) -> Result<(), VoomError> {
    require_legacy_evidence(
        owner.source_file_version_id == input.operation.source_file_version_id.0,
        owner,
        "artifact source file version differs from the requested source",
    )?;
    require_legacy_evidence(
        owner.source_media_snapshot_count == 1,
        owner,
        "artifact source snapshot evidence is not unique",
    )?;
    require_legacy_evidence(
        owner.sole_source_media_snapshot_id == Some(input.operation.source_media_snapshot_id.0),
        owner,
        "artifact source snapshot differs from the requested snapshot",
    )?;
    require_legacy_evidence(
        owner.verification_artifact_handle_id == owner.artifact_handle_id,
        owner,
        "verification belongs to another artifact",
    )?;
    require_legacy_evidence(
        owner.artifact_location_handle_id == owner.artifact_handle_id,
        owner,
        "staging location belongs to another artifact",
    )?;
    require_legacy_evidence(
        owner.verification_status == "succeeded",
        owner,
        "artifact verification did not succeed",
    )?;
    require_legacy_evidence(
        owner.artifact_location_value == owner.staging_path,
        owner,
        "verified staging location differs from the commit record",
    )?;
    require_legacy_evidence(
        owner.artifact_location_retired_at.is_none(),
        owner,
        "verified staging location is retired",
    )?;
    require_legacy_evidence(
        owner.expected_size_bytes == owner.observed_size_bytes,
        owner,
        "artifact size evidence is inconsistent",
    )?;
    require_legacy_evidence(
        owner.expected_checksum == owner.observed_checksum,
        owner,
        "artifact checksum evidence is inconsistent",
    )
}

async fn require_legacy_result(
    cp: &ControlPlane,
    owner: &LegacyAudioExtractOwner,
    target: &Path,
) -> Result<(), VoomError> {
    require_legacy_evidence(
        owner.result_location_file_version_id == owner.result_file_version_id,
        owner,
        "result location belongs to another file version",
    )?;
    let result_location = crate::operation_source::resolve_root_relative_existing_path(
        cp,
        "legacy audio extraction result",
        owner.result_storage_root_id,
        &owner.result_provider_relative_locator,
    )
    .await?;
    let requested_target = crate::artifact::fs::canonical_existing_file_no_symlink(target).await?;
    require_legacy_evidence(
        result_location == requested_target,
        owner,
        "result location differs from the requested target",
    )?;
    require_legacy_evidence(
        owner.result_location_retired_at.is_none(),
        owner,
        "result location is retired",
    )?;
    require_legacy_evidence(
        owner.result_size_bytes == owner.observed_size_bytes,
        owner,
        "result size differs from the verified artifact",
    )?;
    require_legacy_evidence(
        owner.result_checksum == owner.observed_checksum,
        owner,
        "result checksum differs from the verified artifact",
    )
}

fn require_legacy_bundle(
    input: &LegacyExtractAdoptionInput,
    owner: &LegacyAudioExtractOwner,
) -> Result<(), VoomError> {
    require_legacy_evidence(
        owner.bundle_id == input.operation.source_bundle_id.0,
        owner,
        "bundle id differs from the requested source bundle",
    )?;
    require_legacy_evidence(
        owner.bundle_role == input.output.bundle_role,
        owner,
        "bundle role differs from the requested output role",
    )
}

fn require_legacy_evidence(
    matches: bool,
    owner: &LegacyAudioExtractOwner,
    context: &str,
) -> Result<(), VoomError> {
    if matches {
        return Ok(());
    }
    Err(VoomError::Conflict(format!(
        "committed legacy audio extraction {} {context}",
        owner.commit_record_id
    )))
}

fn optional_lineage_identity(
    lineage: &serde_json::Value,
    field: &str,
    expected: Option<&str>,
) -> bool {
    let actual = lineage.get(field).and_then(serde_json::Value::as_str);
    actual == expected
}

async fn insert_legacy_extract_adoption(
    cp: &ControlPlane,
    input: &LegacyExtractAdoptionInput,
    owner: &LegacyAudioExtractOwner,
    observed: &ArtifactFileFacts,
    probed: ProbedResultPayload,
) -> Result<AudioExtractOperationRecord, VoomError> {
    let now = cp.clock().now();
    let mut tx = begin_immediate_tx(&cp.pool).await?;
    if let Some(existing) = SqliteAudioExtractOperationRepo::get_exact_by_key_in_tx(
        &mut tx,
        &input.operation,
        std::slice::from_ref(&input.output),
    )
    .await?
    {
        commit_tx(tx).await?;
        return Ok(existing);
    }
    let snapshot = crate::media_snapshot::record_with_event_in_tx(
        cp,
        &mut tx,
        NewMediaSnapshot {
            file_version_id: FileVersionId(owner.result_file_version_id),
            probed_by: Some(probed.worker_id),
            probed_at: now,
            payload: probed.payload.clone(),
        },
    )
    .await?;
    let result_facts = serde_json::to_value(AudioObservedFacts {
        size_bytes: observed.size_bytes,
        content_hash: observed.content_hash.clone(),
        modified_at: None,
        local_file_key: observed.local_file_key.clone(),
    })
    .map_err(|error| VoomError::Internal(format!("encode adopted result facts: {error}")))?;
    let record = SqliteAudioExtractOperationRepo::insert_legacy_adoption_in_tx(
        &mut tx,
        &NewLegacyAudioExtractAdoption {
            operation: input.operation.clone(),
            output: input.output.clone(),
            owner: owner.clone(),
            probe_worker_id: probed.worker_id,
            probe_payload: probed.payload,
            result_media_snapshot_id: snapshot.id,
            result_facts,
            recorded_at: now,
        },
    )
    .await?;
    commit_tx(tx).await?;
    Ok(record)
}

/// Probes the STAGED artifact (the content-hash-verified file at the staging
/// path) and returns its normalized media-snapshot payload WITHOUT recording.
///
/// The staged file is byte-identical to the committed target (commit is an
/// add-only promotion), so probing it yields the same stream/codec facts.
/// Running this fallible external probe before commit lets a transient probe
/// failure retry cleanly from staging without orphaning a committed artifact.
///
/// # Errors
/// Returns the probe dispatch error, or `ArtifactChecksumMismatch` when the
/// probed facts drift from the worker-reported output facts.
pub(crate) async fn probe_staged_result(
    cp: &ControlPlane,
    staging_path: &Path,
    result: &TranscodeAudioResult,
    dispatcher: &dyn AudioResultProbeDispatcher,
) -> Result<ProbedResultPayload, VoomError> {
    let expected = ObservedCandidateFacts {
        size_bytes: result.output.size_bytes,
        content_hash: result.output.content_hash.clone(),
        modified_at: None,
        // Inode facts are a scan-time hardlink signal; a produced-artifact
        // verification has no source file to stat, so they do not apply.
        dev: None,
        ino: None,
        nlink: None,
    };
    let probed = dispatch_verified_result_probe(cp, staging_path, &expected, dispatcher).await?;
    let mut payload = snapshot_with_stream_ids(&probed.result.snapshot)?;
    merge_audio_output_facts(&mut payload, &result.selected_output_streams);
    Ok(ProbedResultPayload {
        worker_id: probed.worker_id,
        payload,
    })
}

pub(crate) async fn probe_staged_synthesis_result(
    cp: &ControlPlane,
    staging_path: &Path,
    source_snapshot: &MediaSnapshot,
    selection: &TranscodeAudioSelectionPlan,
    result: &TranscodeAudioResult,
    dispatcher: &dyn AudioResultProbeDispatcher,
) -> Result<ProbedResultPayload, VoomError> {
    let expected = ObservedCandidateFacts {
        size_bytes: result.output.size_bytes,
        content_hash: result.output.content_hash.clone(),
        modified_at: None,
        dev: None,
        ino: None,
        nlink: None,
    };
    let probed = dispatch_verified_result_probe(cp, staging_path, &expected, dispatcher).await?;
    let mut payload = snapshot_with_stream_ids(&probed.result.snapshot)?;
    bind_synthesis_companions(&mut payload, source_snapshot, selection, result)?;
    Ok(ProbedResultPayload {
        worker_id: probed.worker_id,
        payload,
    })
}

pub(crate) async fn probe_staged_extract_result(
    cp: &ControlPlane,
    staging_path: &Path,
    output: &AudioObservedFacts,
    dispatcher: &dyn AudioResultProbeDispatcher,
) -> Result<ProbedResultPayload, VoomError> {
    let expected = ObservedCandidateFacts {
        size_bytes: output.size_bytes,
        content_hash: output.content_hash.clone(),
        modified_at: None,
        dev: None,
        ino: None,
        nlink: None,
    };
    let probed = dispatch_verified_result_probe(cp, staging_path, &expected, dispatcher).await?;
    Ok(ProbedResultPayload {
        worker_id: probed.worker_id,
        payload: snapshot_with_stream_ids(&probed.result.snapshot)?,
    })
}

async fn dispatch_verified_result_probe(
    cp: &ControlPlane,
    staging_path: &Path,
    expected: &ObservedCandidateFacts,
    dispatcher: &dyn AudioResultProbeDispatcher,
) -> Result<ProbedAudioResult, VoomError> {
    let request = result_probe_request(staging_path, expected)?;
    let probed = dispatcher.dispatch_result_probe(cp, request).await?;
    verify_probe_facts(expected, &probed.result)
        .map_err(|error| VoomError::ArtifactChecksumMismatch(error.message().to_owned()))?;
    Ok(probed)
}

/// Records the already-probed media-snapshot payload against the committed
/// result file version. Only a local DB write remains here, so this runs
/// AFTER commit.
///
/// # Errors
/// Returns the underlying store error if the snapshot insert fails.
pub(crate) async fn record_result_snapshot_payload(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    probed: ProbedResultPayload,
) -> Result<MediaSnapshot, VoomError> {
    cp.record_media_snapshot(
        file_version_id,
        Some(probed.worker_id),
        probed.payload,
        cp.clock().now(),
    )
    .await
}

fn merge_audio_output_facts(payload: &mut serde_json::Value, facts: &[AudioOutputStreamFact]) {
    let Some(streams) = payload
        .get_mut("streams")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for fact in facts {
        let Some(stream) = streams.iter_mut().find(|stream| {
            stream.get("id").and_then(serde_json::Value::as_str)
                == Some(fact.snapshot_stream_id.as_str())
        }) else {
            continue;
        };
        apply_audio_output_fact(stream, fact);
    }
}

fn bind_synthesis_companions(
    payload: &mut serde_json::Value,
    source_snapshot: &MediaSnapshot,
    selection: &TranscodeAudioSelectionPlan,
    result: &TranscodeAudioResult,
) -> Result<(), VoomError> {
    if !selection.add_track {
        return Err(malformed_synthesis(
            "synthesis result validation requires add-track mode",
        ));
    }
    let source_streams = snapshot_streams(&source_snapshot.payload, "source")?;
    let streams = payload
        .get_mut("streams")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| malformed_synthesis("probed result has no stream array"))?;
    if streams.len() != source_streams.len() + result.selected_output_streams.len() {
        return Err(malformed_synthesis(
            "probed result does not contain every source stream and companion",
        ));
    }
    validate_unique_provider_indexes(streams)?;
    let companion_indexes = companion_provider_indexes(&result.selected_output_streams)?;
    bind_preserved_source_streams(source_streams, streams, &companion_indexes)?;
    bind_companion_streams(streams, selection, &result.selected_output_streams)
}

fn snapshot_streams<'a>(
    payload: &'a serde_json::Value,
    label: &str,
) -> Result<&'a [serde_json::Value], VoomError> {
    payload
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| malformed_synthesis(&format!("{label} snapshot has no stream array")))
}

fn validate_unique_provider_indexes(streams: &[serde_json::Value]) -> Result<(), VoomError> {
    let mut indexes = std::collections::BTreeSet::new();
    for stream in streams {
        let index = stream_index(stream)?;
        if !indexes.insert(index) {
            return Err(malformed_synthesis(
                "probed result contains duplicate provider stream indexes",
            ));
        }
    }
    Ok(())
}

fn companion_provider_indexes(
    facts: &[AudioOutputStreamFact],
) -> Result<std::collections::BTreeSet<u32>, VoomError> {
    let mut indexes = std::collections::BTreeSet::new();
    for fact in facts {
        if !indexes.insert(fact.output_provider_stream_index) {
            return Err(malformed_synthesis(
                "synthesis result contains duplicate companion provider stream indexes",
            ));
        }
    }
    Ok(indexes)
}

fn bind_preserved_source_streams(
    source_streams: &[serde_json::Value],
    result_streams: &mut [serde_json::Value],
    companion_indexes: &std::collections::BTreeSet<u32>,
) -> Result<(), VoomError> {
    const PRESERVED_FIELDS: [&str; 11] = [
        "kind",
        "codec_name",
        "channels",
        "language",
        "title",
        "disposition",
        "width",
        "height",
        "pixel_format",
        "filename",
        "mime_type",
    ];
    let mut preserved = result_streams.iter_mut().filter(|stream| {
        stream_index(stream).is_ok_and(|index| !companion_indexes.contains(&index))
    });
    for (ordinal, source) in source_streams.iter().enumerate() {
        let result = preserved.next().ok_or_else(|| {
            malformed_synthesis("probed result omitted a preserved source stream")
        })?;
        for field in PRESERVED_FIELDS {
            if source.get(field) != result.get(field) {
                return Err(malformed_synthesis(&format!(
                    "probed result changed source stream ordinal {ordinal} field {field}"
                )));
            }
        }
        result["id"] = source["id"].clone();
    }
    if preserved.next().is_some() {
        return Err(malformed_synthesis(
            "probed result contains an unexpected non-companion stream",
        ));
    }
    Ok(())
}

fn bind_companion_streams(
    streams: &mut [serde_json::Value],
    selection: &TranscodeAudioSelectionPlan,
    facts: &[AudioOutputStreamFact],
) -> Result<(), VoomError> {
    if selection.selected_streams.len() != facts.len() {
        return Err(malformed_synthesis(
            "synthesis selection and output fact counts differ",
        ));
    }
    for (selected, fact) in selection.selected_streams.iter().zip(facts) {
        if fact.snapshot_stream_id != selected.stream.snapshot_stream_id {
            return Err(malformed_synthesis(
                "synthesis output identity differs from the planned companion",
            ));
        }
        let planned_id = &selected.stream.snapshot_stream_id;
        if streams.iter().any(|candidate| {
            candidate.get("id").and_then(serde_json::Value::as_str) == Some(planned_id)
        }) {
            return Err(malformed_synthesis(&format!(
                "planned companion identity {planned_id} is already occupied"
            )));
        }
        let stream =
            stream_at_index_mut(streams, fact.output_provider_stream_index).ok_or_else(|| {
                malformed_synthesis(&format!(
                    "probed result omitted companion provider stream index {}",
                    fact.output_provider_stream_index
                ))
            })?;
        validate_companion_facts(stream, fact)?;
        stream["id"] = serde_json::Value::String(planned_id.clone());
        apply_audio_output_fact(stream, fact);
    }
    Ok(())
}

fn validate_companion_facts(
    stream: &serde_json::Value,
    fact: &AudioOutputStreamFact,
) -> Result<(), VoomError> {
    let disposition = fact.disposition.as_ref();
    let matches = stream.get("kind").and_then(serde_json::Value::as_str) == Some("audio")
        && stream.get("codec_name").and_then(serde_json::Value::as_str) == Some(&fact.codec)
        && stream.get("channels").and_then(serde_json::Value::as_u64) == fact.channels
        && optional_string(stream, "language") == fact.language.as_deref()
        && optional_string(stream, "title") == fact.title.as_deref()
        && optional_bool(stream, "disposition", "default")
            == disposition.and_then(|value| value.default).or(fact.default)
        && optional_bool(stream, "disposition", "forced")
            == disposition.and_then(|value| value.forced)
        && optional_bool(stream, "disposition", "commentary")
            == disposition.and_then(|value| value.commentary);
    if matches {
        Ok(())
    } else {
        Err(malformed_synthesis(&format!(
            "probed companion stream {} does not match worker facts",
            fact.output_provider_stream_index
        )))
    }
}

fn apply_audio_output_fact(stream: &mut serde_json::Value, fact: &AudioOutputStreamFact) {
    if let Some(language) = &fact.language {
        stream["language"] = serde_json::Value::String(language.clone());
    }
    if let Some(title) = &fact.title {
        stream["title"] = serde_json::Value::String(title.clone());
    }
    if let Some(channels) = fact.channels {
        stream["channels"] = serde_json::Value::from(channels);
    }
    if let Some(disposition) = &fact.disposition {
        stream["disposition"]["default"] =
            serde_json::Value::Bool(disposition.default.unwrap_or(false));
        stream["disposition"]["forced"] =
            serde_json::Value::Bool(disposition.forced.unwrap_or(false));
        stream["disposition"]["commentary"] =
            serde_json::Value::Bool(disposition.commentary.unwrap_or(false));
    }
}

fn stream_at_index_mut(
    streams: &mut [serde_json::Value],
    index: u32,
) -> Option<&mut serde_json::Value> {
    streams.iter_mut().find(|stream| {
        stream.get("index").and_then(serde_json::Value::as_u64) == Some(u64::from(index))
    })
}

fn stream_index(stream: &serde_json::Value) -> Result<u32, VoomError> {
    let index = stream
        .get("index")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| malformed_synthesis("snapshot stream has no numeric provider index"))?;
    u32::try_from(index)
        .map_err(|_| malformed_synthesis("snapshot provider stream index exceeds u32"))
}

fn optional_string<'a>(stream: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    stream.get(field).and_then(serde_json::Value::as_str)
}

fn optional_bool(stream: &serde_json::Value, object: &str, field: &str) -> Option<bool> {
    stream
        .get(object)
        .and_then(|value| value.get(field))
        .and_then(serde_json::Value::as_bool)
}

fn malformed_synthesis(message: &str) -> VoomError {
    VoomError::MalformedWorkerResult(format!("synthesize_audio: {message}"))
}

pub(crate) async fn commit_audio_extract_set_with_hooks(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    hooks: &dyn ExtractClaimFenceHooks,
) -> Result<Vec<CommittedAudioExtractOutput>, VoomError> {
    if input.outputs.is_empty() {
        return Err(VoomError::Config(
            "audio extraction commit set must not be empty".to_owned(),
        ));
    }
    let prepared = prepare_extract_set(cp, input, hooks).await?;
    match commit_audio_extract_set_inner(cp, input, &prepared, hooks).await {
        Ok(outputs) => Ok(outputs),
        Err(error) => Err(record_extract_recovery_failure(cp, input, &prepared, error).await),
    }
}

async fn commit_audio_extract_set_inner(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    prepared: &[PreparedSidecarCommit],
    hooks: &dyn ExtractClaimFenceHooks,
) -> Result<Vec<CommittedAudioExtractOutput>, VoomError> {
    assert_extract_claim(cp, input, hooks, 0, prepared.len()).await?;
    for (index, member) in prepared.iter().enumerate() {
        promote_sidecar(member).await?;
        assert_extract_claim(cp, input, hooks, index + 1, prepared.len()).await?;
    }
    finalize_extract_set(cp, input, prepared).await
}

pub(crate) async fn recover_audio_extract_set_with_hooks(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    hooks: &dyn ExtractClaimFenceHooks,
) -> Result<Vec<CommittedAudioExtractOutput>, VoomError> {
    let mut prepared = match load_recovery_extract_set(cp, input).await {
        Ok(prepared) => prepared,
        Err(error) => {
            return Err(record_extract_input_recovery_failure(cp, input, error).await);
        }
    };
    match recover_audio_extract_set_inner(cp, input, &mut prepared, hooks).await {
        Ok(outputs) => Ok(outputs),
        Err(error) => Err(record_extract_recovery_failure(cp, input, &prepared, error).await),
    }
}

pub(crate) async fn record_prepared_successor_evidence(
    cp: &ControlPlane,
    operation: &AudioExtractOperationRecord,
    claim: &NewAudioExtractClaim,
) -> Result<(), VoomError> {
    let error = VoomError::Conflict(
        "audio extraction successor claimed an operation left prepared by a lost claim".to_owned(),
    );
    let members = raw_recovery_members(operation);
    mark_extract_recovery_members(
        cp,
        operation.operation.id,
        claim,
        &members,
        &error,
        "audio extraction successor recovery after prior claim loss",
    )
    .await
}

pub(crate) async fn record_recovery_projection_failure(
    cp: &ControlPlane,
    operation: &AudioExtractOperationRecord,
    claim: &NewAudioExtractClaim,
    error: VoomError,
) -> VoomError {
    let members = raw_recovery_members(operation);
    if let Err(record_error) = mark_extract_recovery_members(
        cp,
        operation.operation.id,
        claim,
        &members,
        &error,
        "audio extraction recovery input decoding failed",
    )
    .await
    {
        tracing::warn!(
            primary_error_code = error.error_code().as_str(),
            secondary_error = %record_error,
            operation_id = operation.operation.id,
            "audio extraction recovery projection evidence failed while preserving primary error"
        );
    }
    error
}

fn raw_recovery_members(operation: &AudioExtractOperationRecord) -> Vec<ExtractRecoveryMember> {
    operation
        .outputs
        .iter()
        .filter_map(|output| {
            Some(ExtractRecoveryMember {
                commit_record_id: output.commit_record_id?,
                artifact_handle_id: output.artifact_handle_id?,
                target_path: PathBuf::from(&output.target_path),
                temp_path: PathBuf::from(output.temp_path.as_ref()?),
            })
        })
        .collect()
}

async fn recover_audio_extract_set_inner(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    prepared: &mut [PreparedSidecarCommit],
    hooks: &dyn ExtractClaimFenceHooks,
) -> Result<Vec<CommittedAudioExtractOutput>, VoomError> {
    let evaluated = check_recovery_extract_gate(cp, input).await?;
    for member in &mut *prepared {
        member.gate_evaluated_lease_ids.clone_from(&evaluated);
    }
    assert_extract_claim(cp, input, hooks, 0, prepared.len()).await?;
    for (index, member) in prepared.iter().enumerate() {
        recover_promote_extract_member(member).await?;
        assert_extract_claim(cp, input, hooks, index + 1, prepared.len()).await?;
    }
    finalize_extract_set(cp, input, prepared).await
}

async fn record_extract_input_recovery_failure(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    error: VoomError,
) -> VoomError {
    if let Err(record_error) = mark_recovery_input_required(
        cp,
        input,
        &error,
        "audio extraction recovery failed after durable prepare",
    )
    .await
    {
        tracing::warn!(
            primary_error_code = error.error_code().as_str(),
            secondary_error = %record_error,
            operation_id = input.operation_row_id,
            "audio extraction input recovery evidence write failed while preserving primary error"
        );
    }
    error
}

async fn record_extract_recovery_failure(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    prepared: &[PreparedSidecarCommit],
    error: VoomError,
) -> VoomError {
    if let Err(record_error) = mark_extract_set_recovery_required(cp, input, prepared, &error).await
    {
        tracing::warn!(
            primary_error_code = error.error_code().as_str(),
            secondary_error = %record_error,
            operation_id = input.operation_row_id,
            "audio extraction recovery evidence write failed while preserving the primary error"
        );
    }
    error
}

async fn load_recovery_extract_set(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
) -> Result<Vec<PreparedSidecarCommit>, VoomError> {
    let source = crate::operation_source::select_local_source(
        cp,
        "audio extraction recovery",
        input.source_file_version_id,
        Some(input.source_file_location_id),
    )
    .await?;
    let (source_storage_root_id, _) = source.location.rooted_address()?;
    let mut prepared = Vec::with_capacity(input.outputs.len());
    for output in &input.outputs {
        let commit_record_id = output.prepared_commit_record_id.ok_or_else(|| {
            VoomError::Internal(format!(
                "audio extraction output {} is missing commit_record_id",
                output.operation_output_id
            ))
        })?;
        let record = cp
            .artifacts
            .get_commit_record(commit_record_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!(
                    "audio extraction commit record {commit_record_id} is missing"
                ))
            })?;
        if !matches!(
            record.state,
            ArtifactCommitState::Pending | ArtifactCommitState::RecoveryRequired
        ) {
            return Err(VoomError::Conflict(format!(
                "audio extraction commit record {commit_record_id} is not recoverable"
            )));
        }
        let temp_path = output.prepared_temp_path.clone().ok_or_else(|| {
            VoomError::Internal(format!(
                "audio extraction output {} is missing temp_path",
                output.operation_output_id
            ))
        })?;
        let (storage_root_id, provider_relative_locator) =
            crate::artifact::commit::rooted_target_from_commit_report(&record)?;
        let target_path = crate::operation_source::resolve_exact_artifact_recovery_target(
            cp,
            "audio extraction recovery",
            source_storage_root_id,
            storage_root_id,
            &provider_relative_locator,
            &output.target_path,
        )
        .await?;
        prepared.push(PreparedSidecarCommit {
            record,
            staging_path: output.staging_path.clone(),
            storage_root_id,
            provider_relative_locator,
            target_path,
            temp_path,
            expected_facts: ArtifactFileFacts {
                path: output.staging_path.clone(),
                size_bytes: output.output.size_bytes,
                content_hash: output.output.content_hash.clone(),
                modified_at: None,
                local_file_key: output.output.local_file_key.clone(),
            },
            gate_evaluated_lease_ids: Vec::new(),
        });
    }
    Ok(prepared)
}

async fn check_recovery_extract_gate(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
) -> Result<Vec<UseLeaseId>, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let evaluated =
        check_sidecar_commit_gate(cp, &mut tx, input.source_file_version_id, cp.clock().now())
            .await?;
    commit_tx(tx).await?;
    Ok(evaluated)
}

async fn recover_promote_extract_member(member: &PreparedSidecarCommit) -> Result<(), VoomError> {
    recover_staged_add_only_with_temp(
        &member.staging_path,
        &member.target_path,
        &member.temp_path,
        &member.expected_facts,
    )
    .await?;
    Ok(())
}

async fn prepare_extract_set(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    hooks: &dyn ExtractClaimFenceHooks,
) -> Result<Vec<PreparedSidecarCommit>, VoomError> {
    let source = crate::operation_source::select_local_source(
        cp,
        "audio extraction commit",
        input.source_file_version_id,
        Some(input.source_file_location_id),
    )
    .await?;
    let (source_storage_root_id, _) = source.location.rooted_address()?;
    let mut inspected = Vec::with_capacity(input.outputs.len());
    for output in &input.outputs {
        inspected.push(inspect_extract_output(cp, source_storage_root_id, output).await?);
    }
    let preflight_now = cp.clock().now();
    let mut preflight_tx = begin_tx(&cp.pool).await?;
    check_sidecar_commit_gate(
        cp,
        &mut preflight_tx,
        input.source_file_version_id,
        preflight_now,
    )
    .await?;
    commit_tx(preflight_tx).await?;

    let mut tx = begin_immediate_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let evaluated =
        check_sidecar_commit_gate(cp, &mut tx, input.source_file_version_id, now).await?;
    hooks.after_prepare_gate().await?;
    let mut prepared = Vec::with_capacity(input.outputs.len());
    let mut bindings = Vec::with_capacity(input.outputs.len());
    for (output, inspected) in input.outputs.iter().zip(inspected) {
        let record =
            create_extract_pending_in_tx(cp, &mut tx, input, output, &inspected, now).await?;
        bindings.push(NewPreparedAudioExtractOutput {
            operation_output_id: output.operation_output_id,
            staging_path: output.staging_path.display().to_string(),
            temp_path: inspected.temp_path.display().to_string(),
            artifact_handle_id: output.artifact_handle_id,
            artifact_location_id: output.artifact_location_id,
            verification_id: output.verification_id,
            commit_record_id: record.id,
            probe_worker_id: output.probed.worker_id,
            probe_payload: output.probed.payload.clone(),
        });
        prepared.push(PreparedSidecarCommit {
            record,
            staging_path: output.staging_path.clone(),
            storage_root_id: inspected.storage_root_id,
            provider_relative_locator: inspected.provider_relative_locator,
            target_path: inspected.target_path,
            temp_path: inspected.temp_path,
            expected_facts: inspected.expected_facts,
            gate_evaluated_lease_ids: evaluated.clone(),
        });
    }
    SqliteAudioExtractOperationRepo::prepare_operation_in_tx(
        &mut tx,
        input.operation_row_id,
        &input.claim,
        &bindings,
        now,
    )
    .await?;
    commit_tx(tx).await?;
    Ok(prepared)
}

struct InspectedExtractOutput {
    storage_root_id: StorageRootId,
    provider_relative_locator: ProviderRelativeLocator,
    target_path: PathBuf,
    temp_path: PathBuf,
    expected_facts: ArtifactFileFacts,
}

async fn inspect_extract_output(
    cp: &ControlPlane,
    source_storage_root_id: StorageRootId,
    input: &CommitAudioExtractOutputInput,
) -> Result<InspectedExtractOutput, VoomError> {
    let (storage_root_id, provider_relative_locator, target_path) =
        crate::operation_source::resolve_artifact_target(
            cp,
            "audio extraction commit",
            source_storage_root_id,
            &input.target_path,
        )
        .await?;
    let temp_path = canonical_new_leaf_no_symlink(unique_temp_sibling_path(&target_path)?).await?;
    let reported_facts = ArtifactFileFacts {
        path: input.staging_path.clone(),
        size_bytes: input.output.size_bytes,
        content_hash: input.output.content_hash.clone(),
        modified_at: None,
        local_file_key: input.output.local_file_key.clone(),
    };
    let expected_facts =
        require_expected_staging_facts(&input.staging_path, &reported_facts).await?;
    Ok(InspectedExtractOutput {
        storage_root_id,
        provider_relative_locator,
        target_path,
        temp_path,
        expected_facts,
    })
}

async fn create_extract_pending_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    set: &CommitAudioExtractSetInput,
    output: &CommitAudioExtractOutputInput,
    inspected: &InspectedExtractOutput,
    now: time::OffsetDateTime,
) -> Result<ArtifactCommitRecord, VoomError> {
    let pending_input = NewArtifactCommitRecord {
        artifact_handle_id: output.artifact_handle_id,
        source_file_version_id: set.source_file_version_id,
        verification_id: output.verification_id,
        target_path: inspected.target_path.display().to_string(),
        temp_path: Some(inspected.temp_path.display().to_string()),
        report: json!({
            "operation": "extract_audio_sidecar",
            "phase": "prepared",
            "operation_output_id": output.operation_output_id,
            "source_bundle_id": set.source_bundle_id.0,
            "role": bundle_role(output.role).as_str(),
            "source_media_snapshot_id": set.source_media_snapshot_id.0,
            "source_snapshot_stream_id": output.source_snapshot_stream_id,
            "source_provider_stream_index": output.source_provider_stream_index,
            "staging_path": output.staging_path.display().to_string(),
            "target_path": inspected.target_path.display().to_string(),
            "temp_path": inspected.temp_path.display().to_string(),
            "expected_size_bytes": inspected.expected_facts.size_bytes,
            "expected_checksum": inspected.expected_facts.content_hash,
            "staging_local_file_key": inspected.expected_facts.local_file_key,
            "rooted_target": {
                "storage_root_id": inspected.storage_root_id.0,
                "provider_relative_locator": inspected.provider_relative_locator.as_str(),
            },
        }),
        started_at: now,
    };
    create_pending_commit_with_started_event_in_tx(
        &cp.artifacts,
        &cp.events,
        tx,
        pending_input,
        |commit_record_id| {
            Event::ArtifactCommitStarted(ArtifactCommitStartedPayload {
                commit_record_id,
                artifact_handle_id: output.artifact_handle_id,
                source_file_version_id: set.source_file_version_id,
                verification_id: output.verification_id,
                target_path: inspected.target_path.display().to_string(),
                temp_path: inspected.temp_path.display().to_string(),
            })
        },
    )
    .await
    .map_err(PendingCommitRecordError::into_inner)
}

async fn assert_extract_claim(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    hooks: &dyn ExtractClaimFenceHooks,
    boundary_index: usize,
    member_count: usize,
) -> Result<(), VoomError> {
    hooks
        .before_assert(
            cp,
            ExtractClaimFenceContext {
                boundary_index,
                member_count,
                claim: &input.claim,
            },
        )
        .await?;
    cp.audio_extract_operations
        .assert_live_claim(&input.claim, cp.clock().now())
        .await
}

async fn record_staged_audio(
    cp: &ControlPlane,
    source_file_version_id: FileVersionId,
    source_file_location_id: FileLocationId,
    staging_path: &Path,
    size_bytes: u64,
    checksum: &str,
    lineage: serde_json::Value,
) -> Result<StagedAudioArtifact, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let staged = record_staged_audio_in_tx(
        cp,
        &mut tx,
        NewStagedAudioArtifact {
            source_file_version_id,
            source_file_location_id,
            staging_path,
            size_bytes,
            checksum,
            lineage,
        },
        now,
    )
    .await?;
    commit_tx(tx).await?;
    Ok(staged)
}

struct NewStagedAudioArtifact<'a> {
    source_file_version_id: FileVersionId,
    source_file_location_id: FileLocationId,
    staging_path: &'a Path,
    size_bytes: u64,
    checksum: &'a str,
    lineage: serde_json::Value,
}

async fn record_staged_audio_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: NewStagedAudioArtifact<'_>,
    now: time::OffsetDateTime,
) -> Result<StagedAudioArtifact, VoomError> {
    let handle = cp
        .artifacts
        .create_handle_in_tx(
            tx,
            NewArtifactHandle {
                size_bytes: Some(i64::try_from(input.size_bytes).map_err(|err| {
                    VoomError::Internal(format!("audio output size exceeds SQLite integer: {err}"))
                })?),
                checksum: Some(input.checksum.to_owned()),
                privacy_class: "internal".to_owned(),
                durability_class: "staging".to_owned(),
                allowed_access_modes: vec![ArtifactHandleAccessMode::LocalPath],
                mutability: "immutable".to_owned(),
                source_lineage: Some(input.lineage),
                file_version_id: Some(input.source_file_version_id),
                created_at: now,
            },
        )
        .await?;
    let location = cp
        .artifacts
        .record_location_in_tx(
            tx,
            NewArtifactLocation {
                artifact_handle_id: handle.id,
                kind: ArtifactLocationKind::Staging,
                value: input.staging_path.display().to_string(),
                observed_at: now,
            },
        )
        .await?;
    append_event(
        &cp.events,
        tx,
        SubjectType::ArtifactHandle,
        Some(handle.id.0),
        now,
        Event::ArtifactStaged(ArtifactStagedPayload {
            artifact_handle_id: handle.id,
            artifact_location_id: location.id,
            source_file_version_id: input.source_file_version_id,
            source_file_location_id: Some(input.source_file_location_id),
            staging_path: location.value.clone(),
            size_bytes: input.size_bytes,
            checksum: input.checksum.to_owned(),
        }),
    )
    .await?;
    Ok(StagedAudioArtifact {
        artifact_handle_id: handle.id,
        artifact_location_id: location.id,
    })
}

#[derive(Debug, Clone)]
struct PreparedSidecarCommit {
    record: ArtifactCommitRecord,
    staging_path: PathBuf,
    storage_root_id: StorageRootId,
    provider_relative_locator: ProviderRelativeLocator,
    target_path: PathBuf,
    temp_path: PathBuf,
    expected_facts: ArtifactFileFacts,
    gate_evaluated_lease_ids: Vec<UseLeaseId>,
}

/// Consult the commit safety gate for the audio sidecar extract commit. Returns
/// the use-lease ids the gate evaluated (recorded in the completed event) when
/// no blocking lease is live; a blocking lease fails the commit with
/// `BlockedByUseLease` before any filesystem mutation, and any gate-check error
/// is fail-closed.
async fn check_sidecar_commit_gate(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source_file_version_id: FileVersionId,
    now: time::OffsetDateTime,
) -> Result<Vec<UseLeaseId>, VoomError> {
    let Some(source) = cp
        .identity
        .get_file_version_in_tx(tx, source_file_version_id)
        .await?
    else {
        return Err(VoomError::NotFound(format!(
            "file_versions {source_file_version_id} missing"
        )));
    };
    crate::artifact::commit::evaluate_commit_safety_gate(
        cp,
        tx,
        source.file_asset_id,
        source_file_version_id,
        now,
    )
    .await
}

async fn promote_sidecar(prepared: &PreparedSidecarCommit) -> Result<(), VoomError> {
    promote_staged_add_only_with_temp(
        &prepared.staging_path,
        &prepared.target_path,
        &prepared.temp_path,
        &prepared.expected_facts,
    )
    .await?;
    Ok(())
}

async fn finalize_extract_set(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    prepared: &[PreparedSidecarCommit],
) -> Result<Vec<CommittedAudioExtractOutput>, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let mut committed = Vec::with_capacity(prepared.len());
    for (output, member) in input.outputs.iter().zip(prepared) {
        committed.push(finalize_extract_member(cp, &mut tx, input, (output, member), now).await?);
    }
    SqliteAudioExtractOperationRepo::complete_operation_in_tx(
        &mut tx,
        input.operation_row_id,
        &input.claim,
        now,
    )
    .await?;
    commit_tx(tx).await?;
    Ok(committed)
}

async fn finalize_extract_member(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &CommitAudioExtractSetInput,
    member: (&CommitAudioExtractOutputInput, &PreparedSidecarCommit),
    now: time::OffsetDateTime,
) -> Result<CommittedAudioExtractOutput, VoomError> {
    let (output, member) = member;
    let sidecar = cp
        .artifacts
        .record_verified_sidecar_commit_rows_in_tx(
            tx,
            NewSidecarArtifactCommit {
                commit_record_id: member.record.id,
                storage_root_id: member.storage_root_id,
                provider_relative_locator: member.provider_relative_locator.clone(),
                target_path: member.target_path.display().to_string(),
                content_hash: member.expected_facts.content_hash.clone(),
                size_bytes: member.expected_facts.size_bytes,
                observed_at: now,
                finished_at: now,
            },
        )
        .await?;
    let result_snapshot = crate::media_snapshot::record_with_event_in_tx(
        cp,
        tx,
        NewMediaSnapshot {
            file_version_id: sidecar.file_version_id,
            probed_by: Some(output.probed.worker_id),
            probed_at: now,
            payload: output.probed.payload.clone(),
        },
    )
    .await?;
    let bundle_member = cp
        .bundles
        .add_member_in_tx(
            tx,
            NewBundleMember {
                bundle_id: input.source_bundle_id,
                file_asset_id: sidecar.file_asset_id,
                role: bundle_role(output.role),
            },
        )
        .await?;
    let lineage_id = SqliteAudioExtractOperationRepo::record_finalized_output_in_tx(
        tx,
        &NewFinalizedAudioExtractOutput {
            operation_output_id: output.operation_output_id,
            source_file_version_id: input.source_file_version_id,
            source_media_snapshot_id: input.source_media_snapshot_id,
            source_snapshot_stream_id: output.source_snapshot_stream_id.clone(),
            source_provider_stream_index: output.source_provider_stream_index,
            result_file_asset_id: sidecar.file_asset_id.0,
            result_file_version_id: sidecar.file_version_id,
            result_file_location_id: sidecar.file_location_id,
            result_media_snapshot_id: result_snapshot.id,
            bundle_member_id: bundle_member.id,
            recorded_at: now,
        },
    )
    .await?;
    append_extract_commit_completed_event(cp, tx, (output, member), &sidecar, now).await?;
    Ok(CommittedAudioExtractOutput {
        operation_output_id: output.operation_output_id,
        artifact_handle_id: output.artifact_handle_id,
        artifact_location_id: output.artifact_location_id,
        verification_id: output.verification_id,
        commit_record_id: sidecar.commit_record.id,
        result_file_version_id: sidecar.file_version_id,
        result_file_location_id: sidecar.file_location_id,
        result_file_asset_id: sidecar.file_asset_id.0,
        result_media_snapshot_id: result_snapshot.id,
        lineage_id,
        bundle_member_id: bundle_member.id,
        staging_path: output.staging_path.clone(),
        target_path: member.target_path.clone(),
        temp_path: member.temp_path.clone(),
    })
}

async fn append_extract_commit_completed_event(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    member: (&CommitAudioExtractOutputInput, &PreparedSidecarCommit),
    sidecar: &SidecarArtifactCommit,
    now: time::OffsetDateTime,
) -> Result<(), VoomError> {
    let (output, member) = member;
    append_commit_event_in_tx(
        &cp.events,
        tx,
        output.artifact_handle_id,
        now,
        Event::ArtifactCommitCompleted(ArtifactCommitCompletedPayload {
            commit_record_id: sidecar.commit_record.id,
            artifact_handle_id: output.artifact_handle_id,
            result_file_version_id: sidecar.file_version_id,
            result_file_location_id: sidecar.file_location_id,
            target_path: member.target_path.display().to_string(),
            gate_evaluated_lease_ids: member.gate_evaluated_lease_ids.clone(),
        }),
    )
    .await
}

async fn mark_extract_set_recovery_required(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    prepared: &[PreparedSidecarCommit],
    error: &VoomError,
) -> Result<(), VoomError> {
    let members = input
        .outputs
        .iter()
        .zip(prepared)
        .map(|(output, member)| ExtractRecoveryMember {
            commit_record_id: member.record.id,
            artifact_handle_id: output.artifact_handle_id,
            target_path: member.target_path.clone(),
            temp_path: member.temp_path.clone(),
        })
        .collect::<Vec<_>>();
    mark_extract_recovery_members(
        cp,
        input.operation_row_id,
        &input.claim,
        &members,
        error,
        "audio extraction set commit failed after durable prepare",
    )
    .await
}

async fn mark_recovery_input_required(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    error: &VoomError,
    recovery_reason: &str,
) -> Result<(), VoomError> {
    let members = input
        .outputs
        .iter()
        .map(|output| {
            let commit_record_id = output.prepared_commit_record_id.ok_or_else(|| {
                VoomError::Internal(format!(
                    "recoverable audio extraction output {} is missing commit_record_id",
                    output.operation_output_id
                ))
            })?;
            let temp_path = output.prepared_temp_path.clone().ok_or_else(|| {
                VoomError::Internal(format!(
                    "recoverable audio extraction output {} is missing temp_path",
                    output.operation_output_id
                ))
            })?;
            Ok(ExtractRecoveryMember {
                commit_record_id,
                artifact_handle_id: output.artifact_handle_id,
                target_path: output.target_path.clone(),
                temp_path,
            })
        })
        .collect::<Result<Vec<_>, VoomError>>()?;
    mark_extract_recovery_members(
        cp,
        input.operation_row_id,
        &input.claim,
        &members,
        error,
        recovery_reason,
    )
    .await
}

struct ExtractRecoveryMember {
    commit_record_id: ArtifactCommitRecordId,
    artifact_handle_id: ArtifactHandleId,
    target_path: PathBuf,
    temp_path: PathBuf,
}

async fn mark_extract_recovery_members(
    cp: &ControlPlane,
    operation_row_id: u64,
    claim: &NewAudioExtractClaim,
    members: &[ExtractRecoveryMember],
    error: &VoomError,
    recovery_reason: &str,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    for member in members {
        let recovery_reason = recovery_reason.to_owned();
        mark_recovery_required_with_event_in_tx(
            &cp.artifacts,
            &cp.events,
            &mut tx,
            RecoveryRequiredCommit {
                commit_record_id: member.commit_record_id,
                artifact_handle_id: member.artifact_handle_id,
                failure: ArtifactCommitFailure {
                    failure_class: FailureClass::CommitFailure,
                    error_code: error.error_code(),
                    message: error.to_string(),
                    finished_at: now,
                },
                recovery_reason: recovery_reason.clone(),
                event: Event::ArtifactCommitRecoveryRequired(
                    ArtifactCommitRecoveryRequiredPayload {
                        commit_record_id: member.commit_record_id,
                        artifact_handle_id: member.artifact_handle_id,
                        target_path: member.target_path.display().to_string(),
                        temp_path: member.temp_path.display().to_string(),
                        recovery_reason,
                        error_code: error.error_code().as_str().to_owned(),
                        message: error.to_string(),
                    },
                ),
                occurred_at: now,
            },
        )
        .await?;
    }
    SqliteAudioExtractOperationRepo::mark_recovery_required_in_tx(
        &mut tx,
        operation_row_id,
        claim,
        &AudioExtractRecoveryFailure {
            error_code: error.error_code().as_str().to_owned(),
            message: error.to_string(),
        },
        now,
    )
    .await?;
    commit_tx(tx).await
}

pub(super) const fn bundle_role(role: AudioBundleRole) -> BundleMemberRole {
    match role {
        AudioBundleRole::CommentaryAudio => BundleMemberRole::CommentaryAudio,
        AudioBundleRole::ExternalAudio => BundleMemberRole::ExternalAudio,
    }
}

fn result_probe_request(
    target_path: &Path,
    expected: &ObservedCandidateFacts,
) -> Result<ProbeFileRequest, VoomError> {
    let path = target_path.to_str().ok_or_else(|| {
        VoomError::Config(format!(
            "audio target path is not valid UTF-8 and cannot be sent to worker: {}",
            target_path.display()
        ))
    })?;
    Ok(ProbeFileRequest {
        path: path.to_owned(),
        expected: ExpectedFileFacts {
            size_bytes: expected.size_bytes,
            content_hash: expected.content_hash.clone(),
            modified_at: None,
            local_file_key: None,
        },
    })
}

async fn ensure_result_probe_worker(cp: &ControlPlane) -> Result<WorkerId, VoomError> {
    let mut tx = begin_immediate_tx(&cp.pool).await?;
    let worker = crate::scan::bootstrap::ensure_builtin_ffprobe_worker_in_tx(cp, &mut tx).await?;
    tx.commit()
        .await
        .map_err(|err| VoomError::database_context("audio result probe worker commit", err))?;
    Ok(worker.id)
}

fn result_probe_worker_error(err: &crate::scan::worker::ScanWorkerError) -> VoomError {
    VoomError::ExternalSystemUnavailable(format!("audio result probe failed: {err}"))
}

#[cfg(test)]
#[path = "commit_test.rs"]
mod tests;
