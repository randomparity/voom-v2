use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use voom_core::{LeaseId, VoomError, WorkerId};
use voom_worker_protocol::{
    ClientHandle, OperationKind, TranscodeVideoExpectedFacts, TranscodeVideoInput,
    TranscodeVideoOutput, TranscodeVideoRequest, TranscodeVideoResult, WorkerCredentials,
    is_supported_transcode_video_codec, is_supported_transcode_video_container,
};

use super::TranscodeVideoDispatcher;
use super::resolve::ResolvedProfile;
use super::source::SelectedSource;
use crate::artifact::fs::observe_regular_file;
use crate::worker_process::{
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
        // This dispatcher launches a fresh worker per call, so its idempotency
        // cache starts empty; a stable key is enough to dedup a retried call.
        let result = dispatch_transcode_video_with_client(
            &worker.client,
            &worker.credentials,
            "transcode-video-control-plane",
            request,
        )
        .await;
        let _status = worker.shutdown(Duration::from_secs(5)).await;
        result
    }
}

pub fn request_for(
    selected: &SelectedSource,
    resolved: &ResolvedProfile,
    copy_video: bool,
    staging_root: &Path,
    staging_path: &Path,
) -> TranscodeVideoRequest {
    TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: selected.canonical_path.to_string_lossy().into_owned(),
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
            container: resolved.output_container.clone(),
            video_codec: resolved.profile.target_codec.clone(),
            overwrite: false,
        },
        profile: resolved.profile.clone(),
        copy_video,
    }
}

pub async fn revalidate_source_file(selected: &SelectedSource) -> Result<(), VoomError> {
    let facts = observe_regular_file(&selected.canonical_path).await?;
    if facts.size_bytes != selected.version.size_bytes
        || facts.content_hash != selected.version.content_hash
    {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "transcode_video source facts do not match selected file_version at {}",
            selected.location.value
        )));
    }
    Ok(())
}

pub fn validate_result(
    selected: &SelectedSource,
    request: &TranscodeVideoRequest,
    result: &TranscodeVideoResult,
) -> Result<(), VoomError> {
    if !is_supported_transcode_video_container(&result.output_container)
        || !is_supported_transcode_video_codec(&result.output_video_codec)
    {
        return Err(VoomError::MalformedWorkerResult(format!(
            "transcode_video result has unsupported container/codec: {}/{}",
            result.output_container, result.output_video_codec
        )));
    }
    // Container and codec must match what was requested.
    if !result
        .output_container
        .eq_ignore_ascii_case(&request.output.container)
    {
        return Err(VoomError::MalformedWorkerResult(format!(
            "transcode_video result container `{}` does not match requested `{}`",
            result.output_container, request.output.container
        )));
    }
    if !voom_worker_protocol::canonical_video_codec(&result.output_video_codec)
        .is_some_and(|c| c.eq_ignore_ascii_case(&request.output.video_codec))
    {
        return Err(VoomError::MalformedWorkerResult(format!(
            "transcode_video result codec `{}` does not match requested `{}`",
            result.output_video_codec, request.output.video_codec
        )));
    }
    validate_output_facts(request, result)?;
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

/// Validates the worker-observed output facts against the requested profile:
/// the `copy_video`/`copied_video` flags must agree, output dimensions must
/// respect the profile's `max_width`/`max_height` caps when constrained, and the
/// output pixel format must match the constrained `pixel_format`.
///
/// # Errors
/// Returns [`VoomError::MalformedWorkerResult`] on the first violation.
pub fn validate_output_facts(
    request: &TranscodeVideoRequest,
    result: &TranscodeVideoResult,
) -> Result<(), VoomError> {
    if result.copied_video != request.copy_video {
        return Err(VoomError::MalformedWorkerResult(format!(
            "transcode_video result copied_video={} but request copy_video={}",
            result.copied_video, request.copy_video
        )));
    }
    if let Some(cap_w) = request.profile.max_width
        && result.output_width > cap_w
    {
        return Err(VoomError::MalformedWorkerResult(format!(
            "transcode_video result output_width {} exceeds cap {}",
            result.output_width, cap_w
        )));
    }
    if let Some(cap_h) = request.profile.max_height
        && result.output_height > cap_h
    {
        return Err(VoomError::MalformedWorkerResult(format!(
            "transcode_video result output_height {} exceeds cap {}",
            result.output_height, cap_h
        )));
    }
    if let Some(target_pf) = request.profile.pixel_format.as_deref()
        && !result.output_pixel_format.eq_ignore_ascii_case(target_pf)
    {
        return Err(VoomError::MalformedWorkerResult(format!(
            "transcode_video result pixel_format `{}` does not match requested `{}`",
            result.output_pixel_format, target_pf
        )));
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
    idempotency_key: &str,
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
            idempotency_key,
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
