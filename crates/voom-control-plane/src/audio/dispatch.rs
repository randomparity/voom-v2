use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use voom_core::{LeaseId, VoomError, WorkerId};
use voom_worker_protocol::{
    ClientHandle, ExtractAudioRequest, ExtractAudioResult, OperationKind, TranscodeAudioRequest,
    TranscodeAudioResult, WorkerCredentials,
};

use super::{ExtractAudioDispatcher, TranscodeAudioDispatcher};
use crate::worker_process::{
    BundledWorkerProcess, NoopWorkerProgressHandler, WorkerCommand, WorkerOperationDispatch,
    WorkerStreamLabels, bundled_worker_command_from, dispatch_operation_with_client,
};

const FFMPEG_WORKER_BIN_ENV: &str = "VOOM_FFMPEG_WORKER_BIN";
const DISPATCH_IDLE_DEADLINE_MS: u32 = 30_000;
const HEARTBEAT_DEADLINE_MS: u32 = 30_000;

#[derive(Debug, Clone, Copy)]
pub struct BundledTranscodeAudioDispatcher;

#[derive(Debug, Clone, Copy)]
pub struct BundledExtractAudioDispatcher;

#[async_trait]
impl TranscodeAudioDispatcher for BundledTranscodeAudioDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        let command = bundled_ffmpeg_worker_command();
        let worker = BundledWorkerProcess::launch(WorkerId(0), command)
            .await
            .map_err(|err| VoomError::WorkerCrash(err.to_string()))?;
        let result =
            dispatch_transcode_audio_with_client(&worker.client, &worker.credentials, request)
                .await;
        let _status = worker.shutdown(Duration::from_secs(5)).await;
        result
    }
}

#[async_trait]
impl ExtractAudioDispatcher for BundledExtractAudioDispatcher {
    async fn dispatch_extract_audio(
        &self,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        let command = bundled_ffmpeg_worker_command();
        let worker = BundledWorkerProcess::launch(WorkerId(0), command)
            .await
            .map_err(|err| VoomError::WorkerCrash(err.to_string()))?;
        let result =
            dispatch_extract_audio_with_client(&worker.client, &worker.credentials, request).await;
        let _status = worker.shutdown(Duration::from_secs(5)).await;
        result
    }
}

pub(crate) async fn dispatch_transcode_audio_with_client<C>(
    client: &C,
    credentials: &WorkerCredentials,
    request: TranscodeAudioRequest,
) -> Result<TranscodeAudioResult, VoomError>
where
    C: ClientHandle + ?Sized,
{
    dispatch_transcode_audio_with_client_context(
        client,
        credentials,
        LeaseId(0),
        "transcode-audio-control-plane",
        request,
    )
    .await
}

pub(crate) async fn dispatch_transcode_audio_with_client_context<C>(
    client: &C,
    credentials: &WorkerCredentials,
    lease_id: LeaseId,
    idempotency_key: &str,
    request: TranscodeAudioRequest,
) -> Result<TranscodeAudioResult, VoomError>
where
    C: ClientHandle + ?Sized,
{
    dispatch_audio_operation_with_client_context(
        client,
        credentials,
        OperationKind::TranscodeAudio,
        lease_id,
        idempotency_key,
        request,
    )
    .await
}

pub(crate) async fn dispatch_extract_audio_with_client<C>(
    client: &C,
    credentials: &WorkerCredentials,
    request: ExtractAudioRequest,
) -> Result<ExtractAudioResult, VoomError>
where
    C: ClientHandle + ?Sized,
{
    dispatch_extract_audio_with_client_context(
        client,
        credentials,
        LeaseId(0),
        "extract-audio-control-plane",
        request,
    )
    .await
}

pub(crate) async fn dispatch_extract_audio_with_client_context<C>(
    client: &C,
    credentials: &WorkerCredentials,
    lease_id: LeaseId,
    idempotency_key: &str,
    request: ExtractAudioRequest,
) -> Result<ExtractAudioResult, VoomError>
where
    C: ClientHandle + ?Sized,
{
    dispatch_audio_operation_with_client_context(
        client,
        credentials,
        OperationKind::ExtractAudio,
        lease_id,
        idempotency_key,
        request,
    )
    .await
}

async fn dispatch_audio_operation_with_client_context<C, Request, Response>(
    client: &C,
    credentials: &WorkerCredentials,
    operation: OperationKind,
    lease_id: LeaseId,
    idempotency_key: &str,
    request: Request,
) -> Result<Response, VoomError>
where
    C: ClientHandle + ?Sized,
    Request: Serialize + Send,
    Response: DeserializeOwned,
{
    let mut progress = NoopWorkerProgressHandler;
    dispatch_operation_with_client(
        client,
        credentials,
        WorkerOperationDispatch {
            idempotency_key,
            operation,
            lease_id,
            payload: request,
            heartbeat_deadline_ms: HEARTBEAT_DEADLINE_MS,
            progress_idle_deadline_ms: DISPATCH_IDLE_DEADLINE_MS,
            labels: audio_stream_labels(),
        },
        &mut progress,
    )
    .await
}

fn bundled_ffmpeg_worker_command() -> WorkerCommand {
    bundled_ffmpeg_worker_command_from(
        std::env::var_os(FFMPEG_WORKER_BIN_ENV),
        std::env::current_exe(),
    )
}

fn bundled_ffmpeg_worker_command_from(
    configured_bin: Option<std::ffi::OsString>,
    current_exe: std::io::Result<std::path::PathBuf>,
) -> WorkerCommand {
    bundled_worker_command_from(
        configured_bin,
        current_exe,
        "voom-ffmpeg-worker",
        |command, _worker_dir| command,
    )
}

const fn audio_stream_labels() -> WorkerStreamLabels {
    WorkerStreamLabels {
        payload_encode: "audio payload encode",
        dispatch_failed: "audio dispatch failed",
        progress_idle_timeout: "audio worker progress idle timeout",
        stream_protocol: "audio stream",
        terminal_frame_as_progress: "audio worker sent terminal frame as non-terminal progress",
        progress_terminal: "progress frame cannot terminate audio stream",
        stream_ended: "audio worker stream ended before terminal frame",
        result_decode: "audio result decode",
    }
}
