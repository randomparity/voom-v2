use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use std::time::Duration;

use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, VoomError, WorkerId,
};
use voom_events::payload::ArtifactStagedPayload;
use voom_events::{Event, SubjectType};
use voom_store::repo::artifacts::{NewArtifactHandle, NewArtifactLocation};
use voom_store::repo::identity::MediaSnapshot;
use voom_worker_protocol::{
    ExpectedFileFacts, ProbeFileRequest, ProbeFileResult, TranscodeVideoObservedFacts,
    TranscodeVideoResult,
};

use super::ExecuteTranscodeVideoInput;
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};
use crate::scan::persist::{ObservedCandidateFacts, snapshot_with_stream_ids, verify_probe_facts};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedTranscodeArtifact {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProbedTranscodeResult {
    pub worker_id: WorkerId,
    pub result: ProbeFileResult,
}

#[async_trait]
pub(crate) trait TranscodeResultProbeDispatcher: Send + Sync {
    async fn dispatch_result_probe(
        &self,
        cp: &ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<ProbedTranscodeResult, VoomError>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BundledTranscodeResultProbeDispatcher;

#[async_trait]
impl TranscodeResultProbeDispatcher for BundledTranscodeResultProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        cp: &ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<ProbedTranscodeResult, VoomError> {
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
        Ok(ProbedTranscodeResult { worker_id, result })
    }
}

pub async fn record_staged_transcode(
    cp: &ControlPlane,
    input: &ExecuteTranscodeVideoInput,
    source_file_location_id: FileLocationId,
    staging_path: &Path,
    result: &TranscodeVideoResult,
) -> Result<StagedTranscodeArtifact, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let handle = cp
        .artifacts
        .create_handle_in_tx(
            &mut tx,
            NewArtifactHandle {
                size_bytes: Some(i64::try_from(result.output.size_bytes).map_err(|err| {
                    VoomError::Internal(format!(
                        "transcode output size exceeds SQLite integer: {err}"
                    ))
                })?),
                checksum: Some(result.output.content_hash.clone()),
                privacy_class: "internal".to_owned(),
                durability_class: "staging".to_owned(),
                allowed_access_modes: vec!["local_path".to_owned()],
                mutability: "immutable".to_owned(),
                source_lineage: Some(json!({
                    "operation": "transcode_video",
                    "source_file_version_id": input.source_file_version_id.0,
                    "source_file_location_id": source_file_location_id.0,
                })),
                file_version_id: Some(input.source_file_version_id),
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
            source_file_version_id: input.source_file_version_id.0,
            source_file_location_id: Some(source_file_location_id.0),
            staging_path: location.value.clone(),
            size_bytes: result.output.size_bytes,
            checksum: result.output.content_hash.clone(),
        }),
    )
    .await?;
    commit_tx(tx).await?;
    Ok(StagedTranscodeArtifact {
        artifact_handle_id: handle.id,
        artifact_location_id: location.id,
    })
}

/// The normalized media-snapshot payload probed from the staged artifact,
/// paired with the probe worker so the post-commit record step can attribute it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProbedResultPayload {
    pub worker_id: WorkerId,
    pub payload: serde_json::Value,
}

/// Probes the STAGED artifact (the content-hash-verified file at the staging
/// path) and returns its normalized media-snapshot payload WITHOUT recording.
///
/// The staged file is byte-identical to the committed target (commit is an
/// add-only promotion), so probing it yields the same stream/codec/dims facts.
/// Running this fallible external probe before commit lets a transient probe
/// failure retry cleanly from staging without orphaning a committed artifact.
///
/// # Errors
/// Returns the probe dispatch error, or `ArtifactChecksumMismatch` when the
/// probed facts drift from the worker-reported output facts.
pub(crate) async fn probe_staged_result(
    cp: &ControlPlane,
    staging_path: &Path,
    result: &TranscodeVideoResult,
    dispatcher: &dyn TranscodeResultProbeDispatcher,
) -> Result<ProbedResultPayload, VoomError> {
    let expected = result_probe_expected_facts(&result.output);
    let request = result_probe_request(staging_path, &expected)?;
    let probed = dispatcher.dispatch_result_probe(cp, request).await?;
    verify_probe_facts(&expected, &probed.result)
        .map_err(|err| VoomError::ArtifactChecksumMismatch(err.message().to_owned()))?;
    let payload = snapshot_with_stream_ids(&probed.result.snapshot)?;
    Ok(ProbedResultPayload {
        worker_id: probed.worker_id,
        payload,
    })
}

/// Records the already-probed media-snapshot payload against the committed
/// result file version. Only a local DB write remains here, so this runs
/// AFTER commit (see `commit_and_probe_transcode_result`).
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

fn result_probe_expected_facts(output: &TranscodeVideoObservedFacts) -> ObservedCandidateFacts {
    ObservedCandidateFacts {
        size_bytes: output.size_bytes,
        content_hash: output.content_hash.clone(),
        modified_at: None,
    }
}

fn result_probe_request(
    target_path: &Path,
    expected: &ObservedCandidateFacts,
) -> Result<ProbeFileRequest, VoomError> {
    let path = target_path.to_str().ok_or_else(|| {
        VoomError::Config(format!(
            "transcode target path is not valid UTF-8 and cannot be sent to worker: {}",
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
            VoomError::database_context("transcode result probe worker begin", err)
        })?;
    let worker = crate::scan::bootstrap::ensure_builtin_ffprobe_worker_in_tx(cp, &mut tx).await?;
    tx.commit()
        .await
        .map_err(|err| VoomError::database_context("transcode result probe worker commit", err))?;
    Ok(worker.id)
}

fn result_probe_worker_error(err: &crate::scan::worker::ScanWorkerError) -> VoomError {
    VoomError::ExternalSystemUnavailable(format!("transcode result probe failed: {err}"))
}

#[cfg(test)]
#[path = "commit_test.rs"]
mod tests;
