use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use voom_core::{ErrorCode, FailureClass};
use voom_worker_protocol::FFPROBE_VERSION_TIMEOUT;

pub const FFPROBE_BIN_ENV: &str = "VOOM_FFPROBE_BIN";
const DEFAULT_FFPROBE_BIN: &str = "ffprobe";
const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfprobeConfig {
    ffprobe_bin: OsString,
    provider_version: String,
}

impl FfprobeConfig {
    pub fn from_process_env() -> Result<Self, FfprobeConfigError> {
        let ffprobe_bin = std::env::var_os(FFPROBE_BIN_ENV)
            .unwrap_or_else(|| OsString::from(DEFAULT_FFPROBE_BIN));
        Self::from_bin(ffprobe_bin)
    }

    pub fn from_env_pairs<K, V>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, FfprobeConfigError>
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let ffprobe_bin = pairs
            .into_iter()
            .find_map(|(key, value)| {
                if key.as_ref() == OsStr::new(FFPROBE_BIN_ENV) {
                    Some(value.as_ref().to_os_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| OsString::from(DEFAULT_FFPROBE_BIN));
        Self::from_bin(ffprobe_bin)
    }

    fn from_bin(ffprobe_bin: OsString) -> Result<Self, FfprobeConfigError> {
        Self::from_bin_with_version_timeout(ffprobe_bin, FFPROBE_VERSION_TIMEOUT)
    }

    fn from_bin_with_version_timeout(
        ffprobe_bin: OsString,
        version_timeout: Duration,
    ) -> Result<Self, FfprobeConfigError> {
        Ok(Self {
            provider_version: detect_ffprobe_version(&ffprobe_bin, version_timeout)?,
            ffprobe_bin,
        })
    }

    fn ffprobe_bin(&self) -> &OsStr {
        &self.ffprobe_bin
    }

    #[must_use]
    pub fn provider_version(&self) -> &str {
        &self.provider_version
    }
}

#[derive(Debug, Error)]
pub enum FfprobeConfigError {
    #[error("ffprobe dependency {binary:?} failed during {operation}: {source}")]
    Io {
        binary: PathBuf,
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error(
        "ffprobe dependency {binary:?} version check exceeded {} seconds",
        timeout.as_secs()
    )]
    Timeout {
        binary: PathBuf,
        timeout: std::time::Duration,
    },
    #[error("ffprobe dependency {binary:?} exited with {status}: {stderr}")]
    Exit {
        binary: PathBuf,
        status: std::process::ExitStatus,
        stderr: String,
    },
    #[error("ffprobe dependency {binary:?} returned malformed version output: {output:?}")]
    MalformedVersion { binary: PathBuf, output: String },
}

#[derive(Debug, Error)]
pub enum FfprobeError {
    #[error("artifact unavailable: {message}")]
    ArtifactUnavailable {
        message: String,
        payload: serde_json::Value,
    },
    #[error("artifact checksum mismatch: {message}")]
    ArtifactChecksumMismatch {
        message: String,
        payload: serde_json::Value,
    },
    #[error("external system unavailable: {message}")]
    ExternalSystemUnavailable {
        message: String,
        payload: serde_json::Value,
    },
    #[error("malformed media: {message}")]
    MalformedMedia {
        message: String,
        payload: serde_json::Value,
    },
    #[error("malformed worker result: {message}")]
    MalformedWorkerResult {
        message: String,
        payload: serde_json::Value,
    },
}

impl FfprobeError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        match self {
            Self::ArtifactUnavailable { .. } => FailureClass::ArtifactUnavailable,
            Self::ArtifactChecksumMismatch { .. } => FailureClass::ArtifactChecksumMismatch,
            Self::ExternalSystemUnavailable { .. } => FailureClass::ExternalSystemUnavailable,
            Self::MalformedMedia { .. } => FailureClass::MalformedMedia,
            Self::MalformedWorkerResult { .. } => FailureClass::MalformedWorkerResult,
        }
    }

    #[must_use]
    pub const fn error_code(&self) -> ErrorCode {
        self.failure_class().into_error_code()
    }

    #[must_use]
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Self::ArtifactUnavailable { payload, .. }
            | Self::ArtifactChecksumMismatch { payload, .. }
            | Self::ExternalSystemUnavailable { payload, .. }
            | Self::MalformedMedia { payload, .. }
            | Self::MalformedWorkerResult { payload, .. } => payload.clone(),
        }
    }
}

/// Diagnostics that mean the *input bytes* are structurally unusable regardless
/// of the ffprobe build — a permanent [`FailureClass::MalformedMedia`], not a
/// transient tool failure. Deliberately narrow (precision over recall): a missed
/// signature degrades to the pre-existing retriable `ExternalSystemUnavailable`,
/// whereas a false positive would wrongly condemn a transient failure. See
/// `docs/adr/0024`. Signatures like "End of file"/"partial file" (a file still
/// being written) and "unknown format"/"could not find codec parameters" (a
/// demuxer another build might have) are intentionally excluded.
pub(crate) fn is_malformed_media_stderr(stderr: &str) -> bool {
    const SIGNATURES: [&str; 4] = [
        "invalid data found when processing input",
        "moov atom not found",
        "error opening input",
        "header missing",
    ];
    let lowered = stderr.to_ascii_lowercase();
    SIGNATURES
        .iter()
        .any(|signature| lowered.contains(signature))
}

pub async fn run_ffprobe_json(path: &Path, config: &FfprobeConfig) -> Result<Value, FfprobeError> {
    let mut command = Command::new(config.ffprobe_bin());
    command
        .arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg(path)
        .kill_on_drop(true);

    let output = timeout(FFPROBE_TIMEOUT, command_output(&mut command))
        .await
        .map_err(|_| {
            external_system_unavailable(
                "timeout",
                format!("ffprobe exceeded {} seconds", FFPROBE_TIMEOUT.as_secs()),
            )
        })?
        .map_err(|err| external_system_unavailable("spawn", err.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = format!(
            "ffprobe exited with status {}: {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr.trim()
        );
        // A non-zero exit whose diagnostics name a structural input fault is a
        // permanent MalformedMedia (the file cannot be probed on any retry); a
        // signal kill or any other non-zero exit stays the transient
        // ExternalSystemUnavailable. Signal kills carry no code and no matching
        // stderr, so they take the transient branch.
        if output.status.code().is_some() && is_malformed_media_stderr(&stderr) {
            return Err(malformed_media("exit", message));
        }
        return Err(external_system_unavailable("exit", message));
    }

    serde_json::from_slice(&output.stdout).map_err(|err| {
        malformed_worker_result(
            "ffprobe_json",
            format!("ffprobe returned invalid JSON: {err}"),
        )
    })
}

fn detect_ffprobe_version(
    ffprobe_bin: &OsStr,
    version_timeout: Duration,
) -> Result<String, FfprobeConfigError> {
    let binary = PathBuf::from(ffprobe_bin);
    let started = std::time::Instant::now();
    let mut command = std::process::Command::new(ffprobe_bin);
    command
        .arg("-version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = spawn_with_retry(&mut command).map_err(|source| FfprobeConfigError::Io {
        binary: binary.clone(),
        operation: "start version check",
        source,
    })?;
    loop {
        let status = child.try_wait().map_err(|source| FfprobeConfigError::Io {
            binary: binary.clone(),
            operation: "poll version check",
            source,
        })?;
        if let Some(status) = status {
            let output = child
                .wait_with_output()
                .map_err(|source| FfprobeConfigError::Io {
                    binary: binary.clone(),
                    operation: "collect version output",
                    source,
                })?;
            if !status.success() {
                return Err(FfprobeConfigError::Exit {
                    binary,
                    status,
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                });
            }
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let Some(version) = parse_ffprobe_version(stdout.lines().next().unwrap_or_default())
            else {
                return Err(FfprobeConfigError::MalformedVersion {
                    binary,
                    output: stdout,
                });
            };
            return Ok(version);
        }
        if started.elapsed() >= version_timeout {
            child.kill().map_err(|source| FfprobeConfigError::Io {
                binary: binary.clone(),
                operation: "terminate timed-out version check",
                source,
            })?;
            child.wait().map_err(|source| FfprobeConfigError::Io {
                binary: binary.clone(),
                operation: "reap timed-out version check",
                source,
            })?;
            return Err(FfprobeConfigError::Timeout {
                binary,
                timeout: version_timeout,
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

async fn command_output(command: &mut Command) -> io::Result<std::process::Output> {
    let mut attempts_remaining = 3;
    loop {
        attempts_remaining -= 1;
        match command.output().await {
            Err(err) if is_text_file_busy(&err) && attempts_remaining > 0 => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            result => return result,
        }
    }
}

fn spawn_with_retry(command: &mut std::process::Command) -> io::Result<std::process::Child> {
    let mut attempts_remaining = 3;
    loop {
        attempts_remaining -= 1;
        match command.spawn() {
            Err(err) if is_text_file_busy(&err) && attempts_remaining > 0 => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

fn is_text_file_busy(err: &io::Error) -> bool {
    err.raw_os_error() == Some(26)
}

fn parse_ffprobe_version(line: &str) -> Option<String> {
    line.strip_prefix("ffprobe version ")
        .and_then(|tail| tail.split_whitespace().next())
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
}

fn external_system_unavailable(stage: &str, message: String) -> FfprobeError {
    FfprobeError::ExternalSystemUnavailable {
        payload: serde_json::json!({
            "stage": stage,
            "message": message,
        }),
        message,
    }
}

fn malformed_media(stage: &str, message: String) -> FfprobeError {
    FfprobeError::MalformedMedia {
        payload: serde_json::json!({
            "stage": stage,
            "message": message,
        }),
        message,
    }
}

pub(crate) fn malformed_worker_result(stage: &str, message: String) -> FfprobeError {
    FfprobeError::MalformedWorkerResult {
        payload: serde_json::json!({
            "stage": stage,
            "message": message,
        }),
        message,
    }
}

#[cfg(test)]
#[path = "ffprobe_test.rs"]
mod tests;
