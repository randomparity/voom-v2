use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use std::time::Duration;

use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, VoomError, WorkerId,
};
use voom_events::payload::ArtifactStagedPayload;
use voom_events::{Event, SubjectType};
use voom_store::repo::artifacts::{ArtifactRepo, NewArtifactHandle, NewArtifactLocation};
use voom_store::repo::identity::MediaSnapshot;
use voom_worker_protocol::{
    ExpectedFileFacts, ProbeFileRequest, ProbeFileResult, RemuxObservedFacts, RemuxResult,
};

use super::ExecuteRemuxInput;
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};
use crate::scan::persist::{ObservedCandidateFacts, snapshot_with_stream_ids, verify_probe_facts};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedRemuxArtifact {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProbedRemuxResult {
    pub worker_id: WorkerId,
    pub result: ProbeFileResult,
}

#[async_trait]
pub(crate) trait RemuxResultProbeDispatcher: Send + Sync {
    async fn dispatch_result_probe(
        &self,
        cp: &ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<ProbedRemuxResult, VoomError>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BundledRemuxResultProbeDispatcher;

#[async_trait]
impl RemuxResultProbeDispatcher for BundledRemuxResultProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        cp: &ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<ProbedRemuxResult, VoomError> {
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
        Ok(ProbedRemuxResult { worker_id, result })
    }
}

pub async fn record_staged_remux(
    cp: &ControlPlane,
    input: &ExecuteRemuxInput,
    source_file_location_id: FileLocationId,
    staging_path: &Path,
    result: &RemuxResult,
) -> Result<StagedRemuxArtifact, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let handle = cp
        .artifacts
        .create_handle_in_tx(
            &mut tx,
            NewArtifactHandle {
                size_bytes: Some(i64::try_from(result.output.size_bytes).map_err(|err| {
                    VoomError::Internal(format!("remux output size exceeds SQLite integer: {err}"))
                })?),
                checksum: Some(result.output.content_hash.clone()),
                privacy_class: "internal".to_owned(),
                durability_class: "staging".to_owned(),
                allowed_access_modes: vec!["local_path".to_owned()],
                mutability: "immutable".to_owned(),
                source_lineage: Some(json!({
                    "operation": "remux",
                    "source_file_version_id": input.source_file_version_id.0,
                    "source_file_location_id": source_file_location_id.0,
                    "kept_snapshot_stream_ids": result.kept_snapshot_stream_ids,
                    "default_snapshot_stream_ids": result.default_snapshot_stream_ids,
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
    Ok(StagedRemuxArtifact {
        artifact_handle_id: handle.id,
        artifact_location_id: location.id,
    })
}

pub(crate) async fn record_result_snapshot_with_dispatcher(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    target_path: &Path,
    result: &RemuxResult,
    dispatcher: &dyn RemuxResultProbeDispatcher,
) -> Result<MediaSnapshot, VoomError> {
    let expected = result_probe_expected_facts(&result.output);
    let request = result_probe_request(target_path, &expected)?;
    let probed = dispatcher.dispatch_result_probe(cp, request).await?;
    verify_probe_facts(&expected, &probed.result)
        .map_err(|err| VoomError::ArtifactChecksumMismatch(err.message().to_owned()))?;
    let payload = snapshot_with_stream_ids(&probed.result.snapshot)?;
    cp.record_media_snapshot(
        file_version_id,
        Some(probed.worker_id),
        payload,
        cp.clock().now(),
    )
    .await
}

fn result_probe_expected_facts(output: &RemuxObservedFacts) -> ObservedCandidateFacts {
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
            "remux target path is not valid UTF-8 and cannot be sent to worker: {}",
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
            VoomError::Database(format!("remux result probe worker begin: {err}"))
        })?;
    let worker = crate::scan::bootstrap::ensure_builtin_ffprobe_worker_in_tx(cp, &mut tx).await?;
    tx.commit()
        .await
        .map_err(|err| VoomError::Database(format!("remux result probe worker commit: {err}")))?;
    Ok(worker.id)
}

fn result_probe_worker_error(err: &crate::scan::worker::ScanWorkerError) -> VoomError {
    VoomError::ExternalSystemUnavailable(format!("remux result probe failed: {err}"))
}

#[cfg(test)]
#[path = "commit_test.rs"]
mod tests;
