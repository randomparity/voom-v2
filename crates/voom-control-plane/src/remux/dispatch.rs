use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use voom_core::{LeaseId, VoomError, WorkerId};
use voom_worker_protocol::{
    ClientHandle, OperationKind, PercentBps, REMUX_CONTAINER_MKV, RemuxExpectedFacts, RemuxInput,
    RemuxOutput, RemuxRequest, RemuxResult, RemuxSelection, WorkerCredentials,
    is_supported_remux_container,
};

use super::RemuxDispatcher;
use super::source::SelectedSource;
use crate::artifact::fs::observe_regular_file;
use crate::worker_process::{
    BundledWorkerProcess, WorkerCommand, WorkerOperationDispatch, WorkerProgressHandler,
    WorkerStreamLabels, bundled_worker_command_from, dispatch_operation_with_client,
};

const MKVTOOLNIX_WORKER_BIN_ENV: &str = "VOOM_MKVTOOLNIX_WORKER_BIN";
const DISPATCH_IDLE_DEADLINE_MS: u32 = 30_000;
const HEARTBEAT_DEADLINE_MS: u32 = 30_000;

#[async_trait]
pub trait RemuxProgressSink: Send {
    async fn record_remux_progress(
        &mut self,
        percent: Option<PercentBps>,
        message: Option<String>,
    ) -> Result<(), VoomError>;
}

#[derive(Debug, Clone, Copy)]
pub struct NoopRemuxProgressSink;

#[async_trait]
impl RemuxProgressSink for NoopRemuxProgressSink {
    async fn record_remux_progress(
        &mut self,
        _percent: Option<PercentBps>,
        _message: Option<String>,
    ) -> Result<(), VoomError> {
        Ok(())
    }
}

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

    async fn dispatch_remux_with_progress(
        &self,
        request: RemuxRequest,
        progress: &mut dyn RemuxProgressSink,
    ) -> Result<RemuxResult, VoomError> {
        let command = bundled_mkvtoolnix_worker_command();
        let worker = BundledWorkerProcess::launch(WorkerId(0), command)
            .await
            .map_err(|err| VoomError::WorkerCrash(err.to_string()))?;
        let result = dispatch_remux_with_client_context_and_progress(
            &worker.client,
            &worker.credentials,
            "remux-control-plane",
            LeaseId(0),
            request,
            progress,
        )
        .await;
        let _status = worker.shutdown(Duration::from_secs(5)).await;
        result
    }
}

pub fn request_for(
    selected: &SelectedSource,
    selection: &RemuxSelection,
    staging_root: &Path,
    staging_path: &Path,
) -> RemuxRequest {
    RemuxRequest {
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
    }
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
    let kept_ids_match = result
        .kept_snapshot_stream_ids
        .iter()
        .map(String::as_str)
        .eq(selection
            .keep_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str()));
    if !kept_ids_match {
        return Err(VoomError::MalformedWorkerResult(
            "remux result kept stream ids do not match request".to_owned(),
        ));
    }
    let default_ids_match = result
        .default_snapshot_stream_ids
        .iter()
        .map(String::as_str)
        .eq(selection
            .default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str()));
    if !default_ids_match {
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
    let mut progress = NoopRemuxProgressSink;
    dispatch_remux_with_client_context_and_progress(
        client,
        credentials,
        "remux-control-plane",
        LeaseId(0),
        remux,
        &mut progress,
    )
    .await
}

pub(crate) async fn dispatch_remux_with_client_context_and_progress<C>(
    client: &C,
    credentials: &WorkerCredentials,
    idempotency_key: &str,
    lease_id: LeaseId,
    remux: RemuxRequest,
    progress: &mut dyn RemuxProgressSink,
) -> Result<RemuxResult, VoomError>
where
    C: ClientHandle + ?Sized,
{
    let mut progress = RemuxWorkerProgressHandler { inner: progress };
    dispatch_operation_with_client(
        client,
        credentials,
        WorkerOperationDispatch {
            idempotency_key,
            operation: OperationKind::Remux,
            lease_id,
            payload: remux,
            heartbeat_deadline_ms: HEARTBEAT_DEADLINE_MS,
            progress_idle_deadline_ms: DISPATCH_IDLE_DEADLINE_MS,
            labels: remux_stream_labels(),
        },
        &mut progress,
    )
    .await
}

fn bundled_mkvtoolnix_worker_command() -> WorkerCommand {
    bundled_mkvtoolnix_worker_command_from(
        std::env::var_os(MKVTOOLNIX_WORKER_BIN_ENV),
        std::env::current_exe(),
    )
}

fn bundled_mkvtoolnix_worker_command_from(
    configured_bin: Option<std::ffi::OsString>,
    current_exe: std::io::Result<std::path::PathBuf>,
) -> WorkerCommand {
    bundled_worker_command_from(
        configured_bin,
        current_exe,
        "voom-mkvtoolnix-worker",
        |command, _worker_dir| command,
    )
}

struct RemuxWorkerProgressHandler<'a> {
    inner: &'a mut dyn RemuxProgressSink,
}

#[async_trait]
impl WorkerProgressHandler for RemuxWorkerProgressHandler<'_> {
    async fn record_progress(
        &mut self,
        percent: Option<PercentBps>,
        message: Option<String>,
    ) -> Result<(), VoomError> {
        self.inner.record_remux_progress(percent, message).await
    }
}

const fn remux_stream_labels() -> WorkerStreamLabels {
    WorkerStreamLabels {
        payload_encode: "remux payload encode",
        dispatch_failed: "remux dispatch failed",
        progress_idle_timeout: "remux worker progress idle timeout",
        stream_protocol: "remux stream",
        terminal_frame_as_progress: "remux worker sent terminal frame as non-terminal progress",
        progress_terminal: "progress frame cannot terminate remux stream",
        stream_ended: "remux worker stream ended before terminal frame",
        result_decode: "remux result decode",
    }
}

#[cfg(test)]
#[path = "dispatch_test.rs"]
mod tests;
