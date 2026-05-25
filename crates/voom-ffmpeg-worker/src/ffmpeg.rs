use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use voom_worker_protocol::TranscodeVideoProfile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegConfig {
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub provider_version: String,
}

impl FfmpegConfig {
    #[must_use]
    pub fn new(ffmpeg_path: PathBuf, ffprobe_path: PathBuf, provider_version: String) -> Self {
        Self {
            ffmpeg_path,
            ffprobe_path,
            provider_version,
        }
    }
}

#[derive(Debug, Error)]
pub enum FfmpegError {
    #[error("ffmpeg failed: {0}")]
    FfmpegFailed(String),
    #[error("ffprobe failed: {0}")]
    FfprobeFailed(String),
    #[error("output facts mismatch: {0}")]
    OutputFactsMismatch(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputProbe {
    pub container: String,
    pub video_codec: String,
}

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run_ffmpeg_transcode(
    config: &FfmpegConfig,
    input: &Path,
    output: &Path,
    profile: &TranscodeVideoProfile,
) -> Result<OutputProbe, FfmpegError> {
    let mut command = Command::new(&config.ffmpeg_path);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-n")
        .arg("-i")
        .arg(input)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a?")
        .arg("-map")
        .arg("0:s?")
        .arg("-map")
        .arg("0:t?")
        .arg("-c:v")
        .arg(&profile.encoder)
        .arg("-crf")
        .arg(profile.crf.to_string())
        .arg("-preset")
        .arg(&profile.preset)
        .arg("-c:a")
        .arg("copy")
        .arg("-c:s")
        .arg("copy")
        .arg("-c:t")
        .arg("copy")
        .arg("-map_metadata")
        .arg("0")
        .arg("-f")
        .arg("matroska")
        .arg("-progress")
        .arg("pipe:2")
        .arg(output)
        .kill_on_drop(true);

    let process_output = timeout(PROCESS_TIMEOUT, command.output())
        .await
        .map_err(|_| FfmpegError::FfmpegFailed("ffmpeg timed out".to_owned()))?
        .map_err(|err| FfmpegError::FfmpegFailed(err.to_string()))?;
    if !process_output.status.success() {
        return Err(FfmpegError::FfmpegFailed(command_error(&process_output)));
    }

    probe_output(config, output).await
}

async fn probe_output(config: &FfmpegConfig, path: &Path) -> Result<OutputProbe, FfmpegError> {
    let mut command = Command::new(&config.ffprobe_path);
    command
        .arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg(path)
        .kill_on_drop(true);
    let output = timeout(PROCESS_TIMEOUT, command.output())
        .await
        .map_err(|_| FfmpegError::FfprobeFailed("ffprobe timed out".to_owned()))?
        .map_err(|err| FfmpegError::FfprobeFailed(err.to_string()))?;
    if !output.status.success() {
        return Err(FfmpegError::FfprobeFailed(command_error(&output)));
    }
    let json: Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| FfmpegError::FfprobeFailed(format!("invalid ffprobe JSON: {err}")))?;
    let container = json
        .pointer("/format/format_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let video_codec = first_video_codec(&json).unwrap_or_default();
    if !container.split(',').any(|name| name == "matroska")
        || !matches!(video_codec, "hevc" | "h265")
    {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "expected matroska/hevc output, got {container}/{video_codec}"
        )));
    }
    Ok(OutputProbe {
        container: "mkv".to_owned(),
        video_codec: "hevc".to_owned(),
    })
}

fn first_video_codec(json: &Value) -> Option<&str> {
    json.get("streams")?
        .as_array()?
        .iter()
        .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))?
        .get("codec_name")?
        .as_str()
}

fn command_error(output: &std::process::Output) -> String {
    format!(
        "status {}: {}{}",
        output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(test)]
#[path = "ffmpeg_test.rs"]
mod tests;
