use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;
use thiserror::Error;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use voom_core::{ErrorCode, FailureClass};
use voom_worker_protocol::{
    OperationDispatch, OperationFuture, OperationHandler, OperationKind, OperationRequest,
    OperationResponse, ProbeFileRequest, ProbeFileResult, ProbeFileStatus, ProgressFrame,
    ProtocolError,
};

use crate::{WorkerError, normalize_ffprobe_json, observe_file_facts};

pub const FFPROBE_BIN_ENV: &str = "VOOM_FFPROBE_BIN";
const DEFAULT_FFPROBE_BIN: &str = "ffprobe";
const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);
const FFPROBE_VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const PROVIDER_VERSION_UNKNOWN: &str = "unknown";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfprobeConfig {
    ffprobe_bin: OsString,
    provider_version: String,
}

impl FfprobeConfig {
    #[must_use]
    pub fn from_process_env() -> Self {
        let ffprobe_bin = std::env::var_os(FFPROBE_BIN_ENV)
            .unwrap_or_else(|| OsString::from(DEFAULT_FFPROBE_BIN));
        Self {
            provider_version: detect_ffprobe_version(&ffprobe_bin)
                .unwrap_or_else(|| PROVIDER_VERSION_UNKNOWN.to_owned()),
            ffprobe_bin,
        }
    }

    #[must_use]
    pub fn from_env_pairs<K, V>(pairs: impl IntoIterator<Item = (K, V)>) -> Self
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
        Self {
            provider_version: detect_ffprobe_version(&ffprobe_bin)
                .unwrap_or_else(|| PROVIDER_VERSION_UNKNOWN.to_owned()),
            ffprobe_bin,
        }
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
            | Self::MalformedWorkerResult { payload, .. } => payload.clone(),
        }
    }
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
        return Err(external_system_unavailable(
            "exit",
            format!(
                "ffprobe exited with status {}: {}",
                output
                    .status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }

    serde_json::from_slice(&output.stdout).map_err(|err| {
        malformed_worker_result(
            "ffprobe_json",
            format!("ffprobe returned invalid JSON: {err}"),
        )
    })
}

#[must_use]
pub fn handle_operation(req: OperationRequest) -> OperationFuture {
    handle_operation_with_config(req, FfprobeConfig::from_process_env())
}

#[must_use]
pub fn operation_handler_with_config(config: FfprobeConfig) -> OperationHandler {
    Arc::new(move |req| handle_operation_with_config(req, config.clone()))
}

fn handle_operation_with_config(req: OperationRequest, config: FfprobeConfig) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::ProbeFile {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }

        let lease_id = req.lease_id;
        let accepted_at = Utc::now();
        let payload: ProbeFileRequest =
            serde_json::from_value(req.payload).map_err(|err| ProtocolError::InvalidPayload {
                detail: format!("probe_file payload decode: {err}"),
            })?;

        match Box::pin(probe_file(&payload, &config)).await {
            Ok(result) => success_dispatch(lease_id, accepted_at, result),
            Err(err) => error_dispatch(lease_id, accepted_at, &err),
        }
    })
}

async fn probe_file(
    request: &ProbeFileRequest,
    config: &FfprobeConfig,
) -> Result<ProbeFileResult, FfprobeError> {
    let path = PathBuf::from(&request.path);
    let pre_probe = Box::pin(observe_file_facts(&path))
        .await
        .map_err(FfprobeError::from)?;
    verify_expected_facts("pre_probe", &pre_probe, &request.expected)?;

    let raw = run_ffprobe_json(&path, config).await?;
    let probed_at = Utc::now().to_rfc3339();
    let snapshot = normalize_ffprobe_json(raw, config.provider_version(), &probed_at)
        .map_err(FfprobeError::from)?;

    let post_probe = Box::pin(observe_file_facts(&path))
        .await
        .map_err(FfprobeError::from)?;
    verify_expected_facts("post_probe", &post_probe, &request.expected)?;
    verify_pre_post_match(&pre_probe, &post_probe)?;

    Ok(ProbeFileResult {
        status: ProbeFileStatus::Probed,
        provider: "ffprobe".to_owned(),
        provider_version: config.provider_version().to_owned(),
        pre_probe,
        post_probe,
        snapshot,
    })
}

fn detect_ffprobe_version(ffprobe_bin: &OsStr) -> Option<String> {
    let started = std::time::Instant::now();
    let mut command = std::process::Command::new(ffprobe_bin);
    command
        .arg("-version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = spawn_with_retry(&mut command).ok()?;
    loop {
        if let Some(status) = child.try_wait().ok()? {
            if !status.success() {
                return None;
            }
            let output = child.wait_with_output().ok()?;
            let stdout = String::from_utf8(output.stdout).ok()?;
            return parse_ffprobe_version(stdout.lines().next().unwrap_or_default());
        }
        if started.elapsed() >= FFPROBE_VERSION_TIMEOUT {
            let _kill = child.kill();
            let _status = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

async fn command_output(command: &mut Command) -> io::Result<std::process::Output> {
    for attempt in 0..3 {
        match command.output().await {
            Err(err) if is_text_file_busy(&err) && attempt < 2 => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            result => return result,
        }
    }
    command.output().await
}

fn spawn_with_retry(command: &mut std::process::Command) -> io::Result<std::process::Child> {
    for attempt in 0..3 {
        match command.spawn() {
            Err(err) if is_text_file_busy(&err) && attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
    command.spawn()
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

fn verify_expected_facts(
    stage: &str,
    observed: &voom_worker_protocol::ObservedFileFacts,
    expected: &voom_worker_protocol::ExpectedFileFacts,
) -> Result<(), FfprobeError> {
    if observed.size_bytes == expected.size_bytes && observed.content_hash == expected.content_hash
    {
        return Ok(());
    }
    Err(checksum_mismatch(
        stage,
        "observed file facts differ from expected size/hash",
        serde_json::json!({
            "stage": stage,
            "expected": expected,
            "observed": observed,
        }),
    ))
}

fn verify_pre_post_match(
    pre_probe: &voom_worker_protocol::ObservedFileFacts,
    post_probe: &voom_worker_protocol::ObservedFileFacts,
) -> Result<(), FfprobeError> {
    if pre_probe.size_bytes == post_probe.size_bytes
        && pre_probe.content_hash == post_probe.content_hash
    {
        return Ok(());
    }
    Err(checksum_mismatch(
        "post_probe",
        "post-probe file facts differ from pre-probe facts",
        serde_json::json!({
            "stage": "post_probe",
            "pre_probe": pre_probe,
            "post_probe": post_probe,
        }),
    ))
}

fn success_dispatch(
    lease_id: voom_core::LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    result: ProbeFileResult,
) -> Result<OperationDispatch, ProtocolError> {
    let progress = ProgressFrame::Progress {
        lease_id,
        seq: 0,
        emitted_at: accepted_at,
        percent: None,
        message: Some("ffprobe completed".to_owned()),
        payload: Some(serde_json::json!({"provider": "ffprobe"})),
    };
    let payload = serde_json::to_value(result).map_err(|err| ProtocolError::InvalidPayload {
        detail: format!("probe_file result encode: {err}"),
    })?;
    let result = ProgressFrame::Result {
        lease_id,
        seq: 1,
        emitted_at: Utc::now(),
        payload,
    };
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[progress, result])?,
    ))
}

fn error_dispatch(
    lease_id: voom_core::LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    err: &FfprobeError,
) -> Result<OperationDispatch, ProtocolError> {
    let frame = ProgressFrame::Error {
        lease_id,
        seq: 0,
        emitted_at: Utc::now(),
        class: err.failure_class(),
        code: err.error_code(),
        message: err.to_string(),
        payload: Some(err.payload()),
    };
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[frame])?,
    ))
}

fn body_from_frames(frames: &[ProgressFrame]) -> Result<Vec<u8>, ProtocolError> {
    let mut body = Vec::new();
    for frame in frames {
        body.extend_from_slice(&serde_json::to_vec(frame).map_err(|err| {
            ProtocolError::InvalidPayload {
                detail: format!("frame encode: {err}"),
            }
        })?);
        body.push(b'\n');
    }
    Ok(body)
}

impl From<WorkerError> for FfprobeError {
    fn from(value: WorkerError) -> Self {
        match value {
            WorkerError::ArtifactUnavailable(message) => Self::ArtifactUnavailable {
                payload: serde_json::json!({
                    "stage": "observe_file",
                    "message": message,
                }),
                message,
            },
            WorkerError::MalformedWorkerResult(message) => malformed_worker_result(
                "normalize_ffprobe_json",
                format!("ffprobe JSON normalization failed: {message}"),
            ),
        }
    }
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

fn malformed_worker_result(stage: &str, message: String) -> FfprobeError {
    FfprobeError::MalformedWorkerResult {
        payload: serde_json::json!({
            "stage": stage,
            "message": message,
        }),
        message,
    }
}

fn checksum_mismatch(stage: &str, message: &str, payload: serde_json::Value) -> FfprobeError {
    FfprobeError::ArtifactChecksumMismatch {
        payload,
        message: format!("{stage}: {message}"),
    }
}

#[cfg(test)]
#[path = "ffprobe_test.rs"]
mod tests;
