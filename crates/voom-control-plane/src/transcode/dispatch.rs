use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use voom_core::{LeaseId, VoomError, WorkerId};
use voom_worker_protocol::{
    ClientHandle, OperationKind, TRANSCODE_VIDEO_CODEC, TRANSCODE_VIDEO_CONTAINER,
    TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoProfile,
    TranscodeVideoRequest, TranscodeVideoResult, WorkerCredentials,
    is_supported_transcode_video_codec, is_supported_transcode_video_container,
};

use super::TranscodeVideoDispatcher;
use super::source::SelectedSource;
use crate::artifact::fs::observe_regular_file;
use crate::artifact::worker::{
    BundledWorkerProcess, NoopWorkerProgressHandler, WorkerCommand, WorkerOperationDispatch,
    WorkerStreamLabels, bundled_worker_command_from, dispatch_operation_with_client,
};

const FFMPEG_WORKER_BIN_ENV: &str = "VOOM_FFMPEG_WORKER_BIN";
const DISPATCH_IDLE_DEADLINE_MS: u32 = 30_000;
const HEARTBEAT_DEADLINE_MS: u32 = 30_000;

#[derive(Debug, Clone, Copy)]
pub struct BundledTranscodeVideoDispatcher;

#[async_trait]
impl TranscodeVideoDispatcher for BundledTranscodeVideoDispatcher {
    async fn dispatch_transcode_video(
        &self,
        request: TranscodeVideoRequest,
    ) -> Result<TranscodeVideoResult, VoomError> {
        let command = bundled_ffmpeg_worker_command();
        let worker = BundledWorkerProcess::launch(WorkerId(0), command)
            .await
            .map_err(|err| VoomError::WorkerCrash(err.to_string()))?;
        let result =
            dispatch_transcode_video_with_client(&worker.client, &worker.credentials, request)
                .await;
        let _status = worker.shutdown(Duration::from_secs(5)).await;
        result
    }
}

pub fn request_for(
    selected: &SelectedSource,
    staging_root: &Path,
    staging_path: &Path,
) -> Result<TranscodeVideoRequest, VoomError> {
    Ok(TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: selected.location.value.clone(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: selected.version.size_bytes,
                content_hash: selected.version.content_hash.clone(),
                modified_at: None,
                local_file_key: None,
            },
        },
        output: TranscodeVideoOutput {
            staging_root: staging_root.to_string_lossy().into_owned(),
            path: staging_path.to_string_lossy().into_owned(),
            container: TRANSCODE_VIDEO_CONTAINER.to_owned(),
            video_codec: TRANSCODE_VIDEO_CODEC.to_owned(),
            overwrite: false,
        },
        profile: TranscodeVideoProfile::default_hevc(),
    })
}

pub fn validate_result(
    selected: &SelectedSource,
    result: &TranscodeVideoResult,
) -> Result<(), VoomError> {
    if !is_supported_transcode_video_container(&result.output_container)
        || !is_supported_transcode_video_codec(&result.output_video_codec)
    {
        return Err(VoomError::MalformedWorkerResult(format!(
            "transcode_video result expected mkv/hevc, got {}/{}",
            result.output_container, result.output_video_codec
        )));
    }
    if result.input_pre != result.input_post {
        return Err(VoomError::ArtifactChecksumMismatch(
            "transcode_video source changed during worker execution".to_owned(),
        ));
    }
    if result.input_pre.size_bytes != selected.version.size_bytes
        || result.input_pre.content_hash != selected.version.content_hash
    {
        return Err(VoomError::ArtifactChecksumMismatch(
            "transcode_video source facts do not match selected file_version".to_owned(),
        ));
    }
    Ok(())
}

pub async fn require_output_file_matches_result(
    staging_path: &Path,
    result: &TranscodeVideoResult,
) -> Result<(), VoomError> {
    let facts = observe_regular_file(staging_path).await?;
    if facts.size_bytes != result.output.size_bytes
        || facts.content_hash != result.output.content_hash
    {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "transcode output facts do not match staged file {}",
            staging_path.display()
        )));
    }
    Ok(())
}

pub(crate) async fn dispatch_transcode_video_with_client<C>(
    client: &C,
    credentials: &WorkerCredentials,
    transcode: TranscodeVideoRequest,
) -> Result<TranscodeVideoResult, VoomError>
where
    C: ClientHandle + ?Sized,
{
    let mut progress = NoopWorkerProgressHandler;
    dispatch_operation_with_client(
        client,
        credentials,
        WorkerOperationDispatch {
            idempotency_key: "transcode-video-control-plane",
            operation: OperationKind::TranscodeVideo,
            lease_id: LeaseId(0),
            payload: transcode,
            heartbeat_deadline_ms: HEARTBEAT_DEADLINE_MS,
            progress_idle_deadline_ms: DISPATCH_IDLE_DEADLINE_MS,
            labels: transcode_stream_labels(),
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

const fn transcode_stream_labels() -> WorkerStreamLabels {
    WorkerStreamLabels {
        payload_encode: "transcode_video payload encode",
        dispatch_failed: "transcode dispatch failed",
        progress_idle_timeout: "transcode worker progress idle timeout",
        stream_protocol: "transcode stream",
        terminal_frame_as_progress: "transcode worker sent terminal frame as non-terminal progress",
        progress_terminal: "progress frame cannot terminate transcode stream",
        stream_ended: "transcode worker stream ended before terminal frame",
        result_decode: "transcode_video result decode",
    }
}

#[cfg(test)]
#[path = "dispatch_test.rs"]
mod tests;
