use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use voom_core::{ErrorCode, FailureClass, VoomError, WorkerId};
use voom_worker_protocol::{
    ClientHandle, NdjsonOutcome, OperationKind, OperationRequest, ProgressFrame,
    TRANSCODE_VIDEO_CODEC, TRANSCODE_VIDEO_CONTAINER, TranscodeVideoExpectedFacts,
    TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoProfile, TranscodeVideoRequest,
    TranscodeVideoResult, WorkerCredentials, is_supported_transcode_video_codec,
    is_supported_transcode_video_container,
};

use super::TranscodeVideoDispatcher;
use super::source::SelectedSource;
use crate::artifact::fs::observe_regular_file;
use crate::artifact::worker::{BundledWorkerProcess, WorkerCommand};

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
    let payload = serde_json::to_value(transcode)
        .map_err(|err| VoomError::Internal(format!("transcode_video payload encode: {err}")))?;
    let request = OperationRequest {
        operation: OperationKind::TranscodeVideo,
        lease_id: voom_core::LeaseId(0),
        payload,
        heartbeat_deadline_ms: HEARTBEAT_DEADLINE_MS,
        progress_idle_deadline_ms: DISPATCH_IDLE_DEADLINE_MS,
    };
    let dispatch = client
        .dispatch(credentials, "transcode-video-control-plane", request)
        .await
        .map_err(|err| VoomError::WorkerCrash(format!("transcode dispatch failed: {err}")))?;
    consume_transcode_stream(dispatch).await
}

async fn consume_transcode_stream(
    mut dispatch: voom_worker_protocol::DispatchStream,
) -> Result<TranscodeVideoResult, VoomError> {
    loop {
        let outcome = tokio::time::timeout(
            Duration::from_millis(u64::from(DISPATCH_IDLE_DEADLINE_MS)),
            dispatch.frames.next_frame(),
        )
        .await
        .map_err(|_| VoomError::WorkerTimeout("transcode worker progress idle timeout".to_owned()))?
        .map_err(|err| VoomError::MalformedWorkerResult(format!("transcode stream: {err}")))?;
        match outcome {
            NdjsonOutcome::Frame(ProgressFrame::Progress { .. }) => {}
            NdjsonOutcome::Frame(_) => {
                return Err(VoomError::MalformedWorkerResult(
                    "transcode worker sent terminal frame as non-terminal progress".to_owned(),
                ));
            }
            NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => {
                return serde_json::from_value::<TranscodeVideoResult>(payload).map_err(|err| {
                    VoomError::MalformedWorkerResult(format!(
                        "transcode_video result decode: {err}"
                    ))
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
                    "progress frame cannot terminate transcode stream".to_owned(),
                ));
            }
            NdjsonOutcome::StreamEnd { .. } | NdjsonOutcome::Closed => {
                return Err(VoomError::WorkerCrash(
                    "transcode worker stream ended before terminal frame".to_owned(),
                ));
            }
        }
    }
}

fn bundled_ffmpeg_worker_command() -> WorkerCommand {
    if let Some(configured) = std::env::var_os(FFMPEG_WORKER_BIN_ENV) {
        return WorkerCommand::new(configured);
    }
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(exe_dir) = current_exe.parent()
    {
        for worker_dir in worker_search_dirs(exe_dir) {
            let sibling = worker_dir.join(format!(
                "voom-ffmpeg-worker{}",
                std::env::consts::EXE_SUFFIX
            ));
            if sibling.is_file() {
                return WorkerCommand::new(sibling);
            }
        }
    }
    WorkerCommand::new("voom-ffmpeg-worker")
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
