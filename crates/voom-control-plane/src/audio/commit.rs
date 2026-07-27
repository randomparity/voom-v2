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
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, MediaSnapshotId,
    UseLeaseId, VoomError, WorkerId,
};
use voom_events::payload::{
    ArtifactCommitCompletedPayload, ArtifactCommitRecoveryRequiredPayload,
    ArtifactCommitStartedPayload, ArtifactStagedPayload, MediaSnapshotRecordedPayload,
};
use voom_events::{Event, SubjectType};
use voom_plan::audio::AudioBundleRole;
use voom_store::repo::artifacts::{
    ArtifactCommitFailure, ArtifactCommitRecord, ArtifactCommitState, NewArtifactCommitRecord,
    NewArtifactHandle, NewArtifactLocation, NewSidecarArtifactCommit, SidecarArtifactCommit,
};
use voom_store::repo::audio_extract_operations::AudioExtractOperationRecord;
use voom_store::repo::bundles::{BundleMemberRole, NewBundleMember};
use voom_store::repo::check_lineage_commit_leases_in_tx;
use voom_store::repo::identity::{IdentityRepo, MediaSnapshot, NewMediaSnapshot};
use voom_worker_protocol::{
    AudioObservedFacts, AudioOutputStreamFact, ExpectedFileFacts, ExtractAudioResult,
    ProbeFileRequest, ProbeFileResult, TranscodeAudioResult,
};

use super::selection::ExtractAudioSelectionPlan;
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
#[cfg(test)]
pub struct CommitAudioExtractSidecarInput {
    pub artifact_handle_id: ArtifactHandleId,
    pub verification_id: ArtifactVerificationId,
    pub source_file_version_id: FileVersionId,
    pub source_bundle_id: BundleId,
    pub role: AudioBundleRole,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
    pub output: AudioObservedFacts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct CommitAudioExtractSidecarReport {
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
    pub state: ArtifactCommitState,
    pub target_path: PathBuf,
    pub temp_path: PathBuf,
    pub recovery_required: Option<AudioExtractRecoveryReport>,
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
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_bundle_id: BundleId,
    pub outputs: Vec<CommitAudioExtractOutputInput>,
}

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

#[cfg(test)]
pub async fn record_staged_audio_extract(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    source_file_location_id: FileLocationId,
    staging_path: &Path,
    selection: &ExtractAudioSelectionPlan,
    result: &ExtractAudioResult,
) -> Result<StagedAudioArtifact, VoomError> {
    record_staged_audio(
        cp,
        input.source_file_version_id,
        source_file_location_id,
        staging_path,
        result.output.size_bytes,
        &result.output.content_hash,
        json!({
            "operation": "extract_audio",
            "source_file_version_id": input.source_file_version_id.0,
            "source_file_location_id": source_file_location_id.0,
            "selected_snapshot_stream_id": result.selected_snapshot_stream_id,
            "intended_role": bundle_role(selection.outputs[0].role).as_str(),
        }),
    )
    .await
}

pub async fn record_staged_audio_extract_set(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    source_file_location_id: FileLocationId,
    staging_paths: &[PathBuf],
    operation: &AudioExtractOperationRecord,
    selection: &ExtractAudioSelectionPlan,
    result: &ExtractAudioResult,
) -> Result<Vec<StagedAudioArtifact>, VoomError> {
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
                source_file_version_id: input.source_file_version_id,
                source_file_location_id,
                staging_path: path,
                size_bytes: output.size_bytes,
                checksum: &output.content_hash,
                lineage: json!({
                    "operation": "extract_audio",
                    "operation_id": selection.operation_id,
                    "output_id": selected.output_id,
                    "source_file_version_id": input.source_file_version_id.0,
                    "source_file_location_id": source_file_location_id.0,
                    "source_snapshot_stream_id": selected.stream.snapshot_stream_id,
                    "source_provider_stream_index": selected.stream.provider_stream_index,
                    "intended_role": bundle_role(selected.role).as_str(),
                }),
            },
            now,
        )
        .await?;
        bind_staged_extract_output(&mut tx, operation_output.id, path, output, &artifact).await?;
        staged.push(artifact);
    }
    let worker_result = serde_json::to_string(result).map_err(|error| {
        VoomError::Internal(format!("serialize staged audio extraction result: {error}"))
    })?;
    let update = sqlx::query(
        "UPDATE audio_extract_operations SET state = 'staged', worker_result = ? \
         WHERE id = ? AND state = 'planned' AND worker_result IS NULL",
    )
    .bind(worker_result)
    .bind(sqlite_id(
        operation.operation.id,
        "audio extraction operation",
    )?)
    .execute(&mut *tx)
    .await
    .map_err(|error| VoomError::database_context("stage audio extraction operation", error))?;
    if update.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "audio extraction operation {} was not planned",
            operation.operation.id
        )));
    }
    commit_tx(tx).await?;
    Ok(staged)
}

async fn bind_staged_extract_output(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation_output_id: u64,
    staging_path: &Path,
    output: &AudioObservedFacts,
    artifact: &StagedAudioArtifact,
) -> Result<(), VoomError> {
    let result_facts = serde_json::to_string(output).map_err(|error| {
        VoomError::Internal(format!("serialize staged audio extraction output: {error}"))
    })?;
    let result = sqlx::query(
        "UPDATE audio_extract_operation_outputs SET staging_path = ?, expected_size_bytes = ?, \
         expected_checksum = ?, staging_local_file_key = ?, artifact_handle_id = ?, \
         artifact_location_id = ?, result_facts = ? \
         WHERE id = ? AND staging_path IS NULL AND artifact_handle_id IS NULL",
    )
    .bind(staging_path.display().to_string())
    .bind(sqlite_id(output.size_bytes, "audio extraction size")?)
    .bind(&output.content_hash)
    .bind(&output.local_file_key)
    .bind(sqlite_id(
        artifact.artifact_handle_id.0,
        "audio extraction artifact handle",
    )?)
    .bind(sqlite_id(
        artifact.artifact_location_id.0,
        "audio extraction artifact location",
    )?)
    .bind(result_facts)
    .bind(sqlite_id(
        operation_output_id,
        "audio extraction operation output",
    )?)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("bind staged audio extract output", error))?;
    if result.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "audio extraction output {operation_output_id} was already staged or is missing"
        )));
    }
    Ok(())
}

/// The normalized media-snapshot payload probed from the staged artifact (with
/// audio output facts merged in), paired with the probe worker so the
/// post-commit record step can attribute it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbedResultPayload {
    pub worker_id: WorkerId,
    pub payload: serde_json::Value,
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
    let request = result_probe_request(staging_path, &expected)?;
    let probed = dispatcher.dispatch_result_probe(cp, request).await?;
    verify_probe_facts(&expected, &probed.result)
        .map_err(|err| VoomError::ArtifactChecksumMismatch(err.message().to_owned()))?;
    let mut payload = snapshot_with_stream_ids(&probed.result.snapshot)?;
    merge_audio_output_facts(&mut payload, &result.selected_output_streams);
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
    let request = result_probe_request(staging_path, &expected)?;
    let probed = dispatcher.dispatch_result_probe(cp, request).await?;
    verify_probe_facts(&expected, &probed.result)
        .map_err(|error| VoomError::ArtifactChecksumMismatch(error.message().to_owned()))?;
    Ok(ProbedResultPayload {
        worker_id: probed.worker_id,
        payload: snapshot_with_stream_ids(&probed.result.snapshot)?,
    })
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
}

#[cfg(test)]
pub async fn commit_audio_extract_sidecar(
    cp: &ControlPlane,
    input: CommitAudioExtractSidecarInput,
) -> Result<CommitAudioExtractSidecarReport, VoomError> {
    let prepared = prepare_sidecar_commit(cp, &input).await?;
    match promote_sidecar(&prepared).await {
        Ok(()) => {}
        Err(err) => {
            let report = mark_sidecar_recovery_required(cp, &prepared, &input, err).await?;
            return Ok(report);
        }
    }
    match finalize_sidecar_commit(cp, &prepared, &input).await {
        Ok(report) => Ok(report),
        Err(err) => {
            let report = mark_sidecar_recovery_required(cp, &prepared, &input, err).await?;
            Ok(report)
        }
    }
}

pub async fn commit_audio_extract_set(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
) -> Result<Vec<CommittedAudioExtractOutput>, VoomError> {
    if input.outputs.is_empty() {
        return Err(VoomError::Config(
            "audio extraction commit set must not be empty".to_owned(),
        ));
    }
    let prepared = prepare_extract_set(cp, input).await?;
    for member in &prepared {
        if let Err(error) = promote_sidecar(&member.prepared).await {
            mark_extract_set_recovery_required(cp, input, &prepared, &error).await?;
            return Err(error);
        }
    }
    match finalize_extract_set(cp, input, &prepared).await {
        Ok(outputs) => Ok(outputs),
        Err(error) => {
            mark_extract_set_recovery_required(cp, input, &prepared, &error).await?;
            Err(error)
        }
    }
}

pub async fn recover_audio_extract_set(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
) -> Result<Vec<CommittedAudioExtractOutput>, VoomError> {
    let prepared = load_recovery_extract_set(cp, input).await?;
    for member in &prepared {
        recover_promote_extract_member(member).await?;
    }
    finalize_extract_set(cp, input, &prepared).await
}

async fn load_recovery_extract_set(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
) -> Result<Vec<PreparedExtractSetMember>, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let evaluated =
        check_sidecar_commit_gate(cp, &mut tx, input.source_file_version_id, cp.clock().now())
            .await?;
    commit_tx(tx).await?;
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
        prepared.push(PreparedExtractSetMember {
            prepared: PreparedSidecarCommit {
                record,
                staging_path: output.staging_path.clone(),
                target_path: output.target_path.clone(),
                temp_path,
                expected_facts: ArtifactFileFacts {
                    path: output.staging_path.clone(),
                    size_bytes: output.output.size_bytes,
                    content_hash: output.output.content_hash.clone(),
                    modified_at: None,
                    local_file_key: output.output.local_file_key.clone(),
                },
                gate_evaluated_lease_ids: evaluated.clone(),
            },
        });
    }
    Ok(prepared)
}

async fn recover_promote_extract_member(
    member: &PreparedExtractSetMember,
) -> Result<(), VoomError> {
    recover_staged_add_only_with_temp(
        &member.prepared.staging_path,
        &member.prepared.target_path,
        &member.prepared.temp_path,
        &member.prepared.expected_facts,
    )
    .await?;
    Ok(())
}

struct PreparedExtractSetMember {
    prepared: PreparedSidecarCommit,
}

async fn prepare_extract_set(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
) -> Result<Vec<PreparedExtractSetMember>, VoomError> {
    let mut inspected = Vec::with_capacity(input.outputs.len());
    for output in &input.outputs {
        inspected.push(inspect_extract_output(output).await?);
    }
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let evaluated =
        check_sidecar_commit_gate(cp, &mut tx, input.source_file_version_id, now).await?;
    let mut prepared = Vec::with_capacity(input.outputs.len());
    for (output, inspected) in input.outputs.iter().zip(inspected) {
        let record =
            create_extract_pending_in_tx(cp, &mut tx, input, output, &inspected, now).await?;
        bind_prepared_extract_output(&mut tx, output, &inspected, record.id).await?;
        prepared.push(PreparedExtractSetMember {
            prepared: PreparedSidecarCommit {
                record,
                staging_path: output.staging_path.clone(),
                target_path: inspected.target_path,
                temp_path: inspected.temp_path,
                expected_facts: inspected.expected_facts,
                gate_evaluated_lease_ids: evaluated.clone(),
            },
        });
    }
    update_extract_operation_state(&mut tx, input.operation_row_id, "staged", "prepared").await?;
    commit_tx(tx).await?;
    Ok(prepared)
}

struct InspectedExtractOutput {
    target_path: PathBuf,
    temp_path: PathBuf,
    expected_facts: ArtifactFileFacts,
}

async fn inspect_extract_output(
    input: &CommitAudioExtractOutputInput,
) -> Result<InspectedExtractOutput, VoomError> {
    let target_path = canonical_new_leaf_no_symlink(&input.target_path).await?;
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
                commit_record_id: commit_record_id.0,
                artifact_handle_id: output.artifact_handle_id.0,
                source_file_version_id: set.source_file_version_id.0,
                verification_id: output.verification_id.0,
                target_path: inspected.target_path.display().to_string(),
                temp_path: inspected.temp_path.display().to_string(),
            })
        },
    )
    .await
    .map_err(PendingCommitRecordError::into_inner)
}

async fn bind_prepared_extract_output(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    output: &CommitAudioExtractOutputInput,
    inspected: &InspectedExtractOutput,
    commit_record_id: ArtifactCommitRecordId,
) -> Result<(), VoomError> {
    let probe_payload = serde_json::to_string(&output.probed.payload).map_err(|error| {
        VoomError::Internal(format!("serialize audio extraction probe payload: {error}"))
    })?;
    let result = sqlx::query(
        "UPDATE audio_extract_operation_outputs SET temp_path = ?, verification_id = ?, \
         commit_record_id = ?, probe_worker_id = ?, probe_payload = ? \
         WHERE id = ? AND staging_path = ? AND artifact_handle_id = ? \
           AND artifact_location_id = ? AND verification_id IS NULL AND commit_record_id IS NULL",
    )
    .bind(inspected.temp_path.display().to_string())
    .bind(sqlite_id(
        output.verification_id.0,
        "audio extraction verification",
    )?)
    .bind(sqlite_id(
        commit_record_id.0,
        "audio extraction commit record",
    )?)
    .bind(sqlite_id(
        output.probed.worker_id.0,
        "audio extraction probe worker",
    )?)
    .bind(probe_payload)
    .bind(sqlite_id(
        output.operation_output_id,
        "audio extraction operation output",
    )?)
    .bind(output.staging_path.display().to_string())
    .bind(sqlite_id(
        output.artifact_handle_id.0,
        "audio extraction artifact handle",
    )?)
    .bind(sqlite_id(
        output.artifact_location_id.0,
        "audio extraction artifact location",
    )?)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("bind prepared audio extract output", error))?;
    if result.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "audio extraction output {} was already bound or is missing",
            output.operation_output_id
        )));
    }
    Ok(())
}

async fn update_extract_operation_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation_row_id: u64,
    expected: &str,
    next: &str,
) -> Result<(), VoomError> {
    let result =
        sqlx::query("UPDATE audio_extract_operations SET state = ? WHERE id = ? AND state = ?")
            .bind(next)
            .bind(sqlite_id(operation_row_id, "audio extraction operation")?)
            .bind(expected)
            .execute(&mut **tx)
            .await
            .map_err(|error| {
                VoomError::database_context("transition audio extract operation", error)
            })?;
    if result.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "audio extraction operation {operation_row_id} is not {expected}"
        )));
    }
    Ok(())
}

fn sqlite_id(value: u64, label: &str) -> Result<i64, VoomError> {
    i64::try_from(value)
        .map_err(|error| VoomError::Internal(format!("{label} exceeds SQLite integer: {error}")))
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
                allowed_access_modes: vec!["local_path".to_owned()],
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
                kind: "staging".to_owned(),
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
            artifact_handle_id: handle.id.0,
            artifact_location_id: location.id.0,
            source_file_version_id: input.source_file_version_id.0,
            source_file_location_id: Some(input.source_file_location_id.0),
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
    target_path: PathBuf,
    temp_path: PathBuf,
    expected_facts: ArtifactFileFacts,
    gate_evaluated_lease_ids: Vec<UseLeaseId>,
}

#[cfg(test)]
async fn prepare_sidecar_commit(
    cp: &ControlPlane,
    input: &CommitAudioExtractSidecarInput,
) -> Result<PreparedSidecarCommit, VoomError> {
    let target_path = canonical_new_leaf_no_symlink(&input.target_path).await?;
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
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    // Commit safety gate: a blocking use lease live at commit time on the
    // source lineage fails the sidecar commit here, before the pending commit
    // record and before any filesystem mutation. The evaluated lease ids are
    // recorded in the completed event. Any gate-check error is fail-closed.
    let gate_evaluated_lease_ids =
        check_sidecar_commit_gate(cp, &mut tx, input.source_file_version_id, now).await?;
    let pending_input = NewArtifactCommitRecord {
        artifact_handle_id: input.artifact_handle_id,
        source_file_version_id: input.source_file_version_id,
        verification_id: input.verification_id,
        target_path: target_path.display().to_string(),
        temp_path: Some(temp_path.display().to_string()),
        report: json!({
            "operation": "extract_audio_sidecar",
            "phase": "prepared",
            "source_bundle_id": input.source_bundle_id.0,
            "role": bundle_role(input.role).as_str(),
            "staging_path": input.staging_path.display().to_string(),
            "target_path": target_path.display().to_string(),
            "temp_path": temp_path.display().to_string(),
            "expected_size_bytes": expected_facts.size_bytes,
            "expected_checksum": expected_facts.content_hash,
            "staging_local_file_key": expected_facts.local_file_key,
        }),
        started_at: now,
    };
    let record = create_pending_commit_with_started_event_in_tx(
        &cp.artifacts,
        &cp.events,
        &mut tx,
        pending_input,
        |commit_record_id| {
            Event::ArtifactCommitStarted(ArtifactCommitStartedPayload {
                commit_record_id: commit_record_id.0,
                artifact_handle_id: input.artifact_handle_id.0,
                source_file_version_id: input.source_file_version_id.0,
                verification_id: input.verification_id.0,
                target_path: target_path.display().to_string(),
                temp_path: temp_path.display().to_string(),
            })
        },
    )
    .await
    .map_err(PendingCommitRecordError::into_inner)?;
    commit_tx(tx).await?;
    Ok(PreparedSidecarCommit {
        record,
        staging_path: input.staging_path.clone(),
        target_path,
        temp_path,
        expected_facts,
        gate_evaluated_lease_ids,
    })
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
    let check = check_lineage_commit_leases_in_tx(
        tx,
        &cp.identity,
        source.file_asset_id,
        source_file_version_id,
        now,
    )
    .await?;
    if let Some((lease_id, scope)) = check.blocking {
        return Err(VoomError::BlockedByUseLease(format!(
            "audio sidecar commit blocked by active use lease {lease_id} on {} {}",
            scope.type_str(),
            scope.id_u64()
        )));
    }
    Ok(check.evaluated_lease_ids)
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

#[cfg(test)]
async fn finalize_sidecar_commit(
    cp: &ControlPlane,
    prepared: &PreparedSidecarCommit,
    input: &CommitAudioExtractSidecarInput,
) -> Result<CommitAudioExtractSidecarReport, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let sidecar = cp
        .artifacts
        .record_verified_sidecar_commit_rows_in_tx(
            &mut tx,
            NewSidecarArtifactCommit {
                commit_record_id: prepared.record.id,
                target_path: prepared.target_path.display().to_string(),
                content_hash: prepared.expected_facts.content_hash.clone(),
                size_bytes: prepared.expected_facts.size_bytes,
                observed_at: now,
                finished_at: now,
            },
        )
        .await?;
    cp.bundles
        .add_member_in_tx(
            &mut tx,
            NewBundleMember {
                bundle_id: input.source_bundle_id,
                file_asset_id: sidecar.file_asset_id,
                role: bundle_role(input.role),
            },
        )
        .await?;
    append_commit_event_in_tx(
        &cp.events,
        &mut tx,
        input.artifact_handle_id,
        now,
        Event::ArtifactCommitCompleted(ArtifactCommitCompletedPayload {
            commit_record_id: sidecar.commit_record.id.0,
            artifact_handle_id: input.artifact_handle_id.0,
            result_file_version_id: sidecar.file_version_id.0,
            result_file_location_id: sidecar.file_location_id.0,
            target_path: prepared.target_path.display().to_string(),
            gate_evaluated_lease_ids: prepared
                .gate_evaluated_lease_ids
                .iter()
                .map(|id| id.0)
                .collect(),
        }),
    )
    .await?;
    commit_tx(tx).await?;
    Ok(CommitAudioExtractSidecarReport {
        commit_record_id: sidecar.commit_record.id,
        result_file_version_id: Some(sidecar.file_version_id),
        result_file_location_id: Some(sidecar.file_location_id),
        state: sidecar.commit_record.state,
        target_path: prepared.target_path.clone(),
        temp_path: prepared.temp_path.clone(),
        recovery_required: None,
    })
}

async fn finalize_extract_set(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    prepared: &[PreparedExtractSetMember],
) -> Result<Vec<CommittedAudioExtractOutput>, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let mut committed = Vec::with_capacity(prepared.len());
    for (output, member) in input.outputs.iter().zip(prepared) {
        committed.push(finalize_extract_member(cp, &mut tx, input, (output, member), now).await?);
    }
    complete_extract_operation_in_tx(&mut tx, input.operation_row_id, now).await?;
    commit_tx(tx).await?;
    Ok(committed)
}

async fn finalize_extract_member(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &CommitAudioExtractSetInput,
    member: (&CommitAudioExtractOutputInput, &PreparedExtractSetMember),
    now: time::OffsetDateTime,
) -> Result<CommittedAudioExtractOutput, VoomError> {
    let (output, member) = member;
    let sidecar = cp
        .artifacts
        .record_verified_sidecar_commit_rows_in_tx(
            tx,
            NewSidecarArtifactCommit {
                commit_record_id: member.prepared.record.id,
                target_path: member.prepared.target_path.display().to_string(),
                content_hash: member.prepared.expected_facts.content_hash.clone(),
                size_bytes: member.prepared.expected_facts.size_bytes,
                observed_at: now,
                finished_at: now,
            },
        )
        .await?;
    let result_snapshot = cp
        .identity
        .record_media_snapshot_in_tx(
            tx,
            NewMediaSnapshot {
                file_version_id: sidecar.file_version_id,
                probed_by: Some(output.probed.worker_id),
                probed_at: now,
                payload: output.probed.payload.clone(),
            },
        )
        .await?;
    append_result_snapshot_event(cp, tx, output, &result_snapshot, now).await?;
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
    let lineage_id =
        record_extract_lineage_in_tx(tx, input, output, sidecar.file_version_id, now).await?;
    bind_finalized_extract_output(
        tx,
        output.operation_output_id,
        sidecar.file_asset_id.0,
        sidecar.file_version_id,
        sidecar.file_location_id,
        result_snapshot.id,
        bundle_member.id,
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
        target_path: member.prepared.target_path.clone(),
        temp_path: member.prepared.temp_path.clone(),
    })
}

async fn append_result_snapshot_event(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    output: &CommitAudioExtractOutputInput,
    snapshot: &voom_store::repo::identity::MediaSnapshot,
    now: time::OffsetDateTime,
) -> Result<(), VoomError> {
    append_event(
        &cp.events,
        tx,
        SubjectType::MediaSnapshot,
        Some(snapshot.id.0),
        now,
        Event::MediaSnapshotRecorded(MediaSnapshotRecordedPayload {
            media_snapshot_id: snapshot.id.0,
            file_version_id: snapshot.file_version_id.0,
            probed_by_worker_id: Some(output.probed.worker_id.0),
            probed_at: now,
        }),
    )
    .await
}

async fn append_extract_commit_completed_event(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    member: (&CommitAudioExtractOutputInput, &PreparedExtractSetMember),
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
            commit_record_id: sidecar.commit_record.id.0,
            artifact_handle_id: output.artifact_handle_id.0,
            result_file_version_id: sidecar.file_version_id.0,
            result_file_location_id: sidecar.file_location_id.0,
            target_path: member.prepared.target_path.display().to_string(),
            gate_evaluated_lease_ids: member
                .prepared
                .gate_evaluated_lease_ids
                .iter()
                .map(|id| id.0)
                .collect(),
        }),
    )
    .await
}

async fn complete_extract_operation_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation_row_id: u64,
    now: time::OffsetDateTime,
) -> Result<(), VoomError> {
    let finished_at = now
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| {
            VoomError::Internal(format!("format audio extract completion time: {error}"))
        })?;
    let result = sqlx::query(
        "UPDATE audio_extract_operations SET state = 'committed', finished_at = ?, \
         recovery_failure_class = NULL, recovery_error_code = NULL, recovery_message = NULL \
         WHERE id = ? AND state IN ('prepared', 'recovery_required')",
    )
    .bind(finished_at)
    .bind(sqlite_id(operation_row_id, "audio extraction operation")?)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("finalize audio extract operation", error))?;
    if result.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "audio extraction operation {operation_row_id} is not prepared"
        )));
    }
    Ok(())
}

async fn record_extract_lineage_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &CommitAudioExtractSetInput,
    output: &CommitAudioExtractOutputInput,
    result_file_version_id: FileVersionId,
    now: time::OffsetDateTime,
) -> Result<u64, VoomError> {
    let result = sqlx::query(
        "INSERT INTO audio_extract_output_lineage \
         (operation_output_id, source_file_version_id, source_media_snapshot_id, \
          source_snapshot_stream_id, source_provider_stream_index, \
          result_file_version_id, recorded_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(sqlite_id(
        output.operation_output_id,
        "audio extraction operation output",
    )?)
    .bind(sqlite_id(
        input.source_file_version_id.0,
        "audio extraction source version",
    )?)
    .bind(sqlite_id(
        input.source_media_snapshot_id.0,
        "audio extraction source snapshot",
    )?)
    .bind(&output.source_snapshot_stream_id)
    .bind(i64::from(output.source_provider_stream_index))
    .bind(sqlite_id(
        result_file_version_id.0,
        "audio extraction result version",
    )?)
    .bind(
        now.format(&time::format_description::well_known::Rfc3339)
            .map_err(|error| {
                VoomError::Internal(format!("format audio extraction lineage time: {error}"))
            })?,
    )
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("insert audio extraction lineage", error))?;
    u64::try_from(result.last_insert_rowid()).map_err(|error| {
        VoomError::Internal(format!("audio extraction lineage id is invalid: {error}"))
    })
}

async fn bind_finalized_extract_output(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation_output_id: u64,
    file_asset_id: u64,
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
    media_snapshot_id: MediaSnapshotId,
    bundle_member_id: u64,
) -> Result<(), VoomError> {
    let result = sqlx::query(
        "UPDATE audio_extract_operation_outputs SET result_file_asset_id = ?, \
         result_file_version_id = ?, result_file_location_id = ?, result_media_snapshot_id = ?, \
         bundle_member_id = ? \
         WHERE id = ? AND result_file_version_id IS NULL",
    )
    .bind(sqlite_id(file_asset_id, "audio extraction result asset")?)
    .bind(sqlite_id(
        file_version_id.0,
        "audio extraction result version",
    )?)
    .bind(sqlite_id(
        file_location_id.0,
        "audio extraction result location",
    )?)
    .bind(sqlite_id(
        media_snapshot_id.0,
        "audio extraction result snapshot",
    )?)
    .bind(sqlite_id(
        bundle_member_id,
        "audio extraction bundle member",
    )?)
    .bind(sqlite_id(
        operation_output_id,
        "audio extraction operation output",
    )?)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("bind finalized audio extract output", error))?;
    if result.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "audio extraction output {operation_output_id} was already finalized or is missing"
        )));
    }
    Ok(())
}

async fn mark_extract_set_recovery_required(
    cp: &ControlPlane,
    input: &CommitAudioExtractSetInput,
    prepared: &[PreparedExtractSetMember],
    error: &VoomError,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    for (output, member) in input.outputs.iter().zip(prepared) {
        let recovery_reason = "audio extraction set commit failed after durable prepare".to_owned();
        mark_recovery_required_with_event_in_tx(
            &cp.artifacts,
            &cp.events,
            &mut tx,
            RecoveryRequiredCommit {
                commit_record_id: member.prepared.record.id,
                artifact_handle_id: output.artifact_handle_id,
                failure: ArtifactCommitFailure {
                    failure_class: "commit_failure".to_owned(),
                    error_code: error.error_code().as_str().to_owned(),
                    message: error.to_string(),
                    finished_at: now,
                },
                recovery_reason: recovery_reason.clone(),
                event: Event::ArtifactCommitRecoveryRequired(
                    ArtifactCommitRecoveryRequiredPayload {
                        commit_record_id: member.prepared.record.id.0,
                        artifact_handle_id: output.artifact_handle_id.0,
                        target_path: member.prepared.target_path.display().to_string(),
                        temp_path: member.prepared.temp_path.display().to_string(),
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
    let result = sqlx::query(
        "UPDATE audio_extract_operations SET state = 'recovery_required', \
         recovery_failure_class = 'commit_failure', recovery_error_code = ?, \
         recovery_message = ? WHERE id = ? AND state = 'prepared'",
    )
    .bind(error.error_code().as_str())
    .bind(error.to_string())
    .bind(sqlite_id(
        input.operation_row_id,
        "audio extraction operation",
    )?)
    .execute(&mut *tx)
    .await
    .map_err(|db_error| {
        VoomError::database_context("mark audio extraction set recovery required", db_error)
    })?;
    if result.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "audio extraction operation {} could not enter recovery",
            input.operation_row_id
        )));
    }
    commit_tx(tx).await
}

#[cfg(test)]
async fn mark_sidecar_recovery_required(
    cp: &ControlPlane,
    prepared: &PreparedSidecarCommit,
    input: &CommitAudioExtractSidecarInput,
    err: VoomError,
) -> Result<CommitAudioExtractSidecarReport, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let recovery_reason = "audio sidecar commit failed after durable prepare".to_owned();
    let error_code = err.error_code().as_str().to_owned();
    let message = err.to_string();
    let recovered = mark_recovery_required_with_event_in_tx(
        &cp.artifacts,
        &cp.events,
        &mut tx,
        RecoveryRequiredCommit {
            commit_record_id: prepared.record.id,
            artifact_handle_id: input.artifact_handle_id,
            failure: ArtifactCommitFailure {
                failure_class: "commit_failure".to_owned(),
                error_code: error_code.clone(),
                message: message.clone(),
                finished_at: now,
            },
            recovery_reason: recovery_reason.clone(),
            event: Event::ArtifactCommitRecoveryRequired(ArtifactCommitRecoveryRequiredPayload {
                commit_record_id: prepared.record.id.0,
                artifact_handle_id: input.artifact_handle_id.0,
                target_path: prepared.target_path.display().to_string(),
                temp_path: prepared.temp_path.display().to_string(),
                recovery_reason,
                error_code,
                message,
            }),
            occurred_at: now,
        },
    )
    .await?;
    commit_tx(tx).await?;
    let recovery = recovery_report(prepared, input, &err).await;
    Ok(CommitAudioExtractSidecarReport {
        commit_record_id: recovered.id,
        result_file_version_id: recovered.result_file_version_id,
        result_file_location_id: recovered.result_file_location_id,
        state: recovered.state,
        target_path: prepared.target_path.clone(),
        temp_path: prepared.temp_path.clone(),
        recovery_required: Some(recovery),
    })
}

#[cfg(test)]
async fn recovery_report(
    prepared: &PreparedSidecarCommit,
    input: &CommitAudioExtractSidecarInput,
    err: &VoomError,
) -> AudioExtractRecoveryReport {
    AudioExtractRecoveryReport {
        recovery_reason: "audio sidecar commit failed after durable prepare".to_owned(),
        commit_record_id: prepared.record.id,
        source_bundle_id: input.source_bundle_id,
        role: bundle_role(input.role).as_str(),
        target_path: prepared.target_path.clone(),
        target_exists: path_exists(&prepared.target_path).await,
        temp_path: prepared.temp_path.clone(),
        temp_exists: path_exists(&prepared.temp_path).await,
        staging_path: prepared.staging_path.clone(),
        staging_exists: path_exists(&prepared.staging_path).await,
        result_file_version_id: None,
        result_file_location_id: None,
        error_code: err.error_code().as_str(),
        message: err.to_string(),
    }
}

fn bundle_role(role: AudioBundleRole) -> BundleMemberRole {
    match role {
        AudioBundleRole::CommentaryAudio => BundleMemberRole::CommentaryAudio,
        AudioBundleRole::ExternalAudio => BundleMemberRole::ExternalAudio,
    }
}

#[cfg(test)]
async fn path_exists(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path).await.is_ok()
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
