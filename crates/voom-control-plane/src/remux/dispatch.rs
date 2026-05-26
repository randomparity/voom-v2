use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use voom_core::{ErrorCode, FailureClass, LeaseId, VoomError, WorkerId};
use voom_worker_protocol::{
    ClientHandle, NdjsonOutcome, OperationKind, OperationRequest, ProgressFrame,
    REMUX_CONTAINER_MKV, RemuxExpectedFacts, RemuxInput, RemuxOutput, RemuxRequest, RemuxResult,
    RemuxSelection, WorkerCredentials, is_supported_remux_container,
};

use super::RemuxDispatcher;
use super::source::SelectedSource;
use crate::artifact::fs::observe_regular_file;
use crate::artifact::worker::{BundledWorkerProcess, WorkerCommand};

const MKVTOOLNIX_WORKER_BIN_ENV: &str = "VOOM_MKVTOOLNIX_WORKER_BIN";
const DISPATCH_IDLE_DEADLINE_MS: u32 = 30_000;
const HEARTBEAT_DEADLINE_MS: u32 = 30_000;

#[derive(Debug, Clone, Copy)]
pub struct BundledRemuxDispatcher;

#[async_trait]
impl RemuxDispatcher for BundledRemuxDispatcher {
    async fn dispatch_remux(&self, request: RemuxRequest) -> Result<RemuxResult, VoomError> {
        let command = bundled_mkvtoolnix_worker_command();
        let worker = BundledWorkerProcess::launch(WorkerId(0), command)
            .await
            .map_err(|err| VoomError::WorkerCrash(err.to_string()))?;
        let result = dispatch_remux_with_client(&worker.client, &worker.credentials, request).await;
        let _status = worker.shutdown(Duration::from_secs(5)).await;
        result
    }
}

pub fn request_for(
    selected: &SelectedSource,
    selection: &RemuxSelection,
    staging_root: &Path,
    staging_path: &Path,
) -> Result<RemuxRequest, VoomError> {
    Ok(RemuxRequest {
        input: RemuxInput {
            path: selected.canonical_path.to_string_lossy().into_owned(),
            expected: RemuxExpectedFacts {
                size_bytes: selected.version.size_bytes,
                content_hash: selected.version.content_hash.clone(),
                modified_at: None,
                local_file_key: None,
            },
        },
        output: RemuxOutput {
            staging_root: staging_root.to_string_lossy().into_owned(),
            path: staging_path.to_string_lossy().into_owned(),
            container: REMUX_CONTAINER_MKV.to_owned(),
            overwrite: false,
        },
        selection: selection.clone(),
    })
}

pub async fn revalidate_source_file(selected: &SelectedSource) -> Result<(), VoomError> {
    let facts = observe_regular_file(&selected.canonical_path).await?;
    if facts.size_bytes != selected.version.size_bytes
        || facts.content_hash != selected.version.content_hash
    {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "remux source facts do not match selected file_version at {}",
            selected.location.value
        )));
    }
    Ok(())
}

pub fn validate_result(
    selected: &SelectedSource,
    selection: &RemuxSelection,
    result: &RemuxResult,
) -> Result<(), VoomError> {
    if !is_supported_remux_container(&result.output_container) {
        return Err(VoomError::MalformedWorkerResult(format!(
            "remux result expected mkv, got {}",
            result.output_container
        )));
    }
    if result.input_pre != result.input_post {
        return Err(VoomError::ArtifactChecksumMismatch(
            "remux source changed during worker execution".to_owned(),
        ));
    }
    if result.input_pre.size_bytes != selected.version.size_bytes
        || result.input_pre.content_hash != selected.version.content_hash
    {
        return Err(VoomError::ArtifactChecksumMismatch(
            "remux source facts do not match selected file_version".to_owned(),
        ));
    }
    let expected_kept = selection
        .keep_streams
        .iter()
        .map(|stream| stream.snapshot_stream_id.as_str())
        .collect::<Vec<_>>();
    let actual_kept = result
        .kept_snapshot_stream_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if actual_kept != expected_kept {
        return Err(VoomError::MalformedWorkerResult(
            "remux result kept stream ids do not match request".to_owned(),
        ));
    }
    let expected_defaults = selection
        .default_streams
        .iter()
        .map(|stream| stream.snapshot_stream_id.as_str())
        .collect::<Vec<_>>();
    let actual_defaults = result
        .default_snapshot_stream_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if actual_defaults != expected_defaults {
        return Err(VoomError::MalformedWorkerResult(
            "remux result default stream ids do not match request".to_owned(),
        ));
    }
    Ok(())
}

pub async fn require_output_file_matches_result(
    staging_path: &Path,
    result: &RemuxResult,
) -> Result<(), VoomError> {
    let facts = observe_regular_file(staging_path).await?;
    if facts.size_bytes != result.output.size_bytes
        || facts.content_hash != result.output.content_hash
    {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "remux output facts do not match staged file {}",
            staging_path.display()
        )));
    }
    Ok(())
}

pub(crate) async fn dispatch_remux_with_client<C>(
    client: &C,
    credentials: &WorkerCredentials,
    remux: RemuxRequest,
) -> Result<RemuxResult, VoomError>
where
    C: ClientHandle + ?Sized,
{
    dispatch_remux_with_client_context(
        client,
        credentials,
        "remux-control-plane",
        LeaseId(0),
        remux,
    )
    .await
}

pub(crate) async fn dispatch_remux_with_client_context<C>(
    client: &C,
    credentials: &WorkerCredentials,
    idempotency_key: &str,
    lease_id: LeaseId,
    remux: RemuxRequest,
) -> Result<RemuxResult, VoomError>
where
    C: ClientHandle + ?Sized,
{
    let payload = serde_json::to_value(remux)
        .map_err(|err| VoomError::Internal(format!("remux payload encode: {err}")))?;
    let request = OperationRequest {
        operation: OperationKind::Remux,
        lease_id,
        payload,
        heartbeat_deadline_ms: HEARTBEAT_DEADLINE_MS,
        progress_idle_deadline_ms: DISPATCH_IDLE_DEADLINE_MS,
    };
    let dispatch = client
        .dispatch(credentials, idempotency_key, request)
        .await
        .map_err(|err| VoomError::WorkerCrash(format!("remux dispatch failed: {err}")))?;
    consume_remux_stream(dispatch).await
}

async fn consume_remux_stream(
    mut dispatch: voom_worker_protocol::DispatchStream,
) -> Result<RemuxResult, VoomError> {
    loop {
        let outcome = tokio::time::timeout(
            Duration::from_millis(u64::from(DISPATCH_IDLE_DEADLINE_MS)),
            dispatch.frames.next_frame(),
        )
        .await
        .map_err(|_| VoomError::WorkerTimeout("remux worker progress idle timeout".to_owned()))?
        .map_err(|err| VoomError::MalformedWorkerResult(format!("remux stream: {err}")))?;
        match outcome {
            NdjsonOutcome::Frame(ProgressFrame::Progress { .. }) => {}
            NdjsonOutcome::Frame(_) => {
                return Err(VoomError::MalformedWorkerResult(
                    "remux worker sent terminal frame as non-terminal progress".to_owned(),
                ));
            }
            NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => {
                return serde_json::from_value::<RemuxResult>(payload).map_err(|err| {
                    VoomError::MalformedWorkerResult(format!("remux result decode: {err}"))
                });
            }
            NdjsonOutcome::Terminated(ProgressFrame::Error {
                class,
                code,
                message,
                ..
            }) => {
                return Err(worker_error(class, code, message));
            }
            NdjsonOutcome::Terminated(ProgressFrame::Progress { .. }) => {
                return Err(VoomError::MalformedWorkerResult(
                    "progress frame cannot terminate remux stream".to_owned(),
                ));
            }
            NdjsonOutcome::StreamEnd { .. } | NdjsonOutcome::Closed => {
                return Err(VoomError::WorkerCrash(
                    "remux worker stream ended before terminal frame".to_owned(),
                ));
            }
        }
    }
}

fn bundled_mkvtoolnix_worker_command() -> WorkerCommand {
    if let Some(configured) = std::env::var_os(MKVTOOLNIX_WORKER_BIN_ENV) {
        return WorkerCommand::new(configured);
    }
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(exe_dir) = current_exe.parent()
    {
        for worker_dir in worker_search_dirs(exe_dir) {
            let sibling = worker_dir.join(format!(
                "voom-mkvtoolnix-worker{}",
                std::env::consts::EXE_SUFFIX
            ));
            if sibling.is_file() {
                return WorkerCommand::new(sibling);
            }
        }
    }
    WorkerCommand::new("voom-mkvtoolnix-worker")
}

fn worker_search_dirs(exe_dir: &Path) -> Vec<std::path::PathBuf> {
    if exe_dir.file_name().is_some_and(|name| name == "deps")
        && let Some(parent) = exe_dir.parent()
    {
        return vec![parent.to_path_buf(), exe_dir.to_path_buf()];
    }
    vec![exe_dir.to_path_buf()]
}

fn worker_error(class: FailureClass, code: ErrorCode, message: String) -> VoomError {
    match code {
        ErrorCode::ArtifactUnavailable => VoomError::ArtifactUnavailable(message),
        ErrorCode::ArtifactChecksumMismatch => VoomError::ArtifactChecksumMismatch(message),
        ErrorCode::MalformedWorkerResult => VoomError::MalformedWorkerResult(message),
        ErrorCode::WorkerTimeout => VoomError::WorkerTimeout(message),
        ErrorCode::WorkerCrash => VoomError::WorkerCrash(message),
        _ if class == FailureClass::MalformedWorkerResult => {
            VoomError::MalformedWorkerResult(message)
        }
        _ => VoomError::WorkerCrash(message),
    }
}

#[cfg(test)]
#[path = "dispatch_test.rs"]
mod tests;
