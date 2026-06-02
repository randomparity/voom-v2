use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::json;
use tokio::fs;
use voom_artifact::commit_pipeline::{
    PendingCommitRecordError, RecoveryRequiredCommit, append_commit_event_in_tx,
    create_pending_commit_with_started_event_in_tx, mark_recovery_required_with_event_in_tx,
};
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId, BundleId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, VoomError, WorkerId,
};
use voom_events::payload::{
    ArtifactCommitCompletedPayload, ArtifactCommitRecoveryRequiredPayload,
    ArtifactCommitStartedPayload, ArtifactStagedPayload,
};
use voom_events::{Event, SubjectType};
use voom_plan::audio::AudioBundleRole;
use voom_store::repo::artifacts::{
    ArtifactCommitFailure, ArtifactCommitRecord, ArtifactCommitState, NewArtifactCommitRecord,
    NewArtifactHandle, NewArtifactLocation, NewSidecarArtifactCommit,
};
use voom_store::repo::bundles::{BundleMemberRole, NewBundleMember};
use voom_store::repo::identity::MediaSnapshot;
use voom_worker_protocol::{
    AudioObservedFacts, AudioOutputStreamFact, ExpectedFileFacts, ExtractAudioResult,
    ProbeFileRequest, ProbeFileResult, TranscodeAudioResult,
};

use super::selection::ExtractAudioSelectionPlan;
use super::{ExecuteExtractAudioInput, ExecuteTranscodeAudioInput};
use crate::ControlPlane;
use crate::artifact::fs::{
    ArtifactFileFacts, canonical_new_leaf_no_symlink, promote_staged_add_only_with_temp,
    require_expected_staging_facts, unique_temp_sibling_path,
};
use crate::cases::{append_event, begin_tx, commit_tx};
use crate::scan::persist::{ObservedCandidateFacts, snapshot_with_stream_ids, verify_probe_facts};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedAudioArtifact {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
pub struct CommitAudioExtractSidecarReport {
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
    pub state: ArtifactCommitState,
    pub target_path: PathBuf,
    pub temp_path: PathBuf,
    pub recovery_required: Option<AudioExtractRecoveryReport>,
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
            "intended_role": bundle_role(selection.role).as_str(),
        }),
    )
    .await
}

/// The normalized media-snapshot payload probed from the staged artifact (with
/// audio output facts merged in), paired with the probe worker so the
/// post-commit record step can attribute it.
#[derive(Debug, Clone, PartialEq)]
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
    let handle = cp
        .artifacts
        .create_handle_in_tx(
            &mut tx,
            NewArtifactHandle {
                size_bytes: Some(i64::try_from(size_bytes).map_err(|err| {
                    VoomError::Internal(format!("audio output size exceeds SQLite integer: {err}"))
                })?),
                checksum: Some(checksum.to_owned()),
                privacy_class: "internal".to_owned(),
                durability_class: "staging".to_owned(),
                allowed_access_modes: vec!["local_path".to_owned()],
                mutability: "immutable".to_owned(),
                source_lineage: Some(lineage),
                file_version_id: Some(source_file_version_id),
                created_at: now,
            },
        )
        .await?;
    let location = cp
        .artifacts
        .record_location_in_tx(
            &mut tx,
            NewArtifactLocation {
                artifact_handle_id: handle.id,
                kind: "staging".to_owned(),
                value: staging_path.display().to_string(),
                observed_at: now,
            },
        )
        .await?;
    append_event(
        &cp.events,
        &mut tx,
        SubjectType::ArtifactHandle,
        Some(handle.id.0),
        now,
        Event::ArtifactStaged(ArtifactStagedPayload {
            artifact_handle_id: handle.id.0,
            artifact_location_id: location.id.0,
            source_file_version_id: source_file_version_id.0,
            source_file_location_id: Some(source_file_location_id.0),
            staging_path: location.value.clone(),
            size_bytes,
            checksum: checksum.to_owned(),
        }),
    )
    .await?;
    commit_tx(tx).await?;
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
}

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
    })
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

pub(super) async fn extract_post_commit_recovery(
    committed: &CommitAudioExtractSidecarReport,
    source_bundle_id: BundleId,
    role: AudioBundleRole,
    staging_path: &Path,
    err: &VoomError,
) -> AudioExtractRecoveryReport {
    AudioExtractRecoveryReport {
        recovery_reason: "audio extract post-commit reporting failed".to_owned(),
        commit_record_id: committed.commit_record_id,
        source_bundle_id,
        role: bundle_role(role).as_str(),
        target_path: committed.target_path.clone(),
        target_exists: path_exists(&committed.target_path).await,
        temp_path: committed.temp_path.clone(),
        temp_exists: path_exists(&committed.temp_path).await,
        staging_path: staging_path.to_path_buf(),
        staging_exists: path_exists(staging_path).await,
        result_file_version_id: committed.result_file_version_id,
        result_file_location_id: committed.result_file_location_id,
        error_code: err.error_code().as_str(),
        message: err.to_string(),
    }
}

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

async fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).await.is_ok()
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
    let mut tx =
        cp.pool.begin().await.map_err(|err| {
            VoomError::Database(format!("audio result probe worker begin: {err}"))
        })?;
    let worker = crate::scan::bootstrap::ensure_builtin_ffprobe_worker_in_tx(cp, &mut tx).await?;
    tx.commit()
        .await
        .map_err(|err| VoomError::Database(format!("audio result probe worker commit: {err}")))?;
    Ok(worker.id)
}

fn result_probe_worker_error(err: &crate::scan::worker::ScanWorkerError) -> VoomError {
    VoomError::ExternalSystemUnavailable(format!("audio result probe failed: {err}"))
}

#[cfg(test)]
#[path = "commit_test.rs"]
mod tests;
