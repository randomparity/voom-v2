use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    OperationDispatch, OperationFuture, OperationHandler, OperationKind, OperationRequest,
    OperationResponse, ProgressFrame, ProtocolError, TranscodeVideoExpectedFacts,
    TranscodeVideoObservedFacts, TranscodeVideoRequest, TranscodeVideoResult, TranscodeVideoStatus,
};

use crate::ffmpeg::{FfmpegConfig, FfmpegError, run_ffmpeg_transcode};
use crate::observe::{ObserveError, observe_file_facts};

const PROVIDER: &str = "ffmpeg";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscodeVideoError {
    ConfigInvalid {
        message: String,
        payload: serde_json::Value,
    },
    ArtifactUnavailable {
        message: String,
        payload: serde_json::Value,
    },
    ArtifactChecksumMismatch {
        message: String,
        payload: serde_json::Value,
    },
    ExternalSystemUnavailable {
        message: String,
        payload: serde_json::Value,
    },
    MalformedWorkerResult {
        message: String,
        payload: serde_json::Value,
    },
}

impl TranscodeVideoError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        match self {
            Self::ConfigInvalid { .. } | Self::MalformedWorkerResult { .. } => {
                FailureClass::MalformedWorkerResult
            }
            Self::ArtifactUnavailable { .. } => FailureClass::ArtifactUnavailable,
            Self::ArtifactChecksumMismatch { .. } => FailureClass::ArtifactChecksumMismatch,
            Self::ExternalSystemUnavailable { .. } => FailureClass::ExternalSystemUnavailable,
        }
    }

    #[must_use]
    pub const fn error_code(&self) -> ErrorCode {
        match self {
            Self::ConfigInvalid { .. } => ErrorCode::ConfigInvalid,
            _ => self.failure_class().into_error_code(),
        }
    }

    #[must_use]
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Self::ConfigInvalid { payload, .. }
            | Self::ArtifactUnavailable { payload, .. }
            | Self::ArtifactChecksumMismatch { payload, .. }
            | Self::ExternalSystemUnavailable { payload, .. }
            | Self::MalformedWorkerResult { payload, .. } => payload.clone(),
        }
    }
}

impl Display for TranscodeVideoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigInvalid { message, .. } => write!(f, "config invalid: {message}"),
            Self::ArtifactUnavailable { message, .. } => {
                write!(f, "artifact unavailable: {message}")
            }
            Self::ArtifactChecksumMismatch { message, .. } => {
                write!(f, "artifact checksum mismatch: {message}")
            }
            Self::ExternalSystemUnavailable { message, .. } => {
                write!(f, "external system unavailable: {message}")
            }
            Self::MalformedWorkerResult { message, .. } => {
                write!(f, "malformed worker result: {message}")
            }
        }
    }
}

impl std::error::Error for TranscodeVideoError {}

#[must_use]
pub fn handle_operation(req: OperationRequest) -> OperationFuture {
    handle_operation_with_config(req, None)
}

#[must_use]
pub fn operation_handler(config: FfmpegConfig) -> OperationHandler {
    Arc::new(move |req| handle_operation_with_config(req, Some(config.clone())))
}

fn handle_operation_with_config(
    req: OperationRequest,
    config: Option<FfmpegConfig>,
) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::TranscodeVideo {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }

        let lease_id = req.lease_id;
        let accepted_at = Utc::now();
        let payload = match serde_json::from_value::<TranscodeVideoRequest>(req.payload) {
            Ok(payload) => payload,
            Err(err) => {
                return error_dispatch(
                    lease_id,
                    accepted_at,
                    &malformed_worker_result(
                        "decode_request",
                        format!("transcode_video payload decode: {err}"),
                    ),
                    0,
                );
            }
        };
        let Some(config) = config else {
            return error_dispatch(
                lease_id,
                accepted_at,
                &config_invalid("preflight", "ffmpeg config was not provided".to_owned()),
                0,
            );
        };

        let started = progress_frame(lease_id, accepted_at);
        match Box::pin(handle_transcode_video(&payload, &config)).await {
            Ok(result) => success_dispatch(lease_id, accepted_at, started, result),
            Err(err) => error_dispatch_with_progress(lease_id, accepted_at, started, &err),
        }
    })
}

pub async fn handle_transcode_video(
    request: &TranscodeVideoRequest,
    config: &FfmpegConfig,
) -> Result<TranscodeVideoResult, TranscodeVideoError> {
    if request.output.overwrite {
        return Err(config_invalid(
            "request",
            "overwrite must be false".to_owned(),
        ));
    }
    validate_request_contract(request)?;
    let input_path = PathBuf::from(&request.input.path);
    let output_path = PathBuf::from(&request.output.path);
    validate_staging_path(Path::new(&request.output.staging_root), &output_path)?;
    if tokio::fs::try_exists(&output_path)
        .await
        .map_err(|err| config_invalid("output_path", err.to_string()))?
    {
        return Err(config_invalid(
            "output_path",
            "output path already exists".to_owned(),
        ));
    }

    let input_pre = observe_file_facts(&input_path)
        .await
        .map_err(TranscodeVideoError::from)?;
    verify_expected_facts("input_pre", &input_pre, &request.input.expected)?;
    let probe = run_ffmpeg_transcode(config, &input_path, &output_path, &request.profile)
        .await
        .map_err(TranscodeVideoError::from)?;
    let input_post = observe_file_facts(&input_path)
        .await
        .map_err(TranscodeVideoError::from)?;
    verify_observed_match("input_post", &input_pre, &input_post)?;
    let output = observe_file_facts(&output_path)
        .await
        .map_err(TranscodeVideoError::from)?;

    Ok(TranscodeVideoResult {
        status: TranscodeVideoStatus::Transcoded,
        provider: PROVIDER.to_owned(),
        provider_version: config.provider_version.clone(),
        input_pre,
        input_post,
        output,
        output_container: probe.container,
        output_video_codec: probe.video_codec,
    })
}

fn validate_request_contract(request: &TranscodeVideoRequest) -> Result<(), TranscodeVideoError> {
    if request.output.container != "mkv"
        || !matches!(request.output.video_codec.as_str(), "hevc" | "h265")
    {
        return Err(config_invalid(
            "request",
            "transcode_video output must request hevc video in mkv".to_owned(),
        ));
    }
    if request.profile != voom_worker_protocol::TranscodeVideoProfile::default_hevc() {
        return Err(config_invalid(
            "request",
            "transcode_video profile must be default-hevc".to_owned(),
        ));
    }
    Ok(())
}

fn validate_staging_path(
    staging_root: &Path,
    output_path: &Path,
) -> Result<(), TranscodeVideoError> {
    let root = canonical_existing_dir_no_symlink(staging_root)?;
    let parent = output_path.parent().ok_or_else(|| {
        config_invalid(
            "output_path",
            "output path has no parent directory".to_owned(),
        )
    })?;
    let parent = canonical_existing_dir_no_symlink(parent)?;
    if !parent.starts_with(root) {
        return Err(config_invalid(
            "output_path",
            "output parent escapes staging root".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_existing_dir_no_symlink(path: &Path) -> Result<PathBuf, TranscodeVideoError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|err| config_invalid("path", format!("{}: {err}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(config_invalid(
            "path",
            format!("path is not a non-symlink directory: {}", path.display()),
        ));
    }
    path.canonicalize()
        .map_err(|err| config_invalid("path", format!("{}: {err}", path.display())))
}

fn verify_expected_facts(
    stage: &str,
    observed: &TranscodeVideoObservedFacts,
    expected: &TranscodeVideoExpectedFacts,
) -> Result<(), TranscodeVideoError> {
    if observed.size_bytes == expected.size_bytes && observed.content_hash == expected.content_hash
    {
        Ok(())
    } else {
        Err(TranscodeVideoError::ArtifactChecksumMismatch {
            message: "input facts differ from expected size/hash".to_owned(),
            payload: serde_json::json!({"stage": stage, "expected": expected, "observed": observed}),
        })
    }
}

fn verify_observed_match(
    stage: &str,
    before: &TranscodeVideoObservedFacts,
    after: &TranscodeVideoObservedFacts,
) -> Result<(), TranscodeVideoError> {
    if before.size_bytes == after.size_bytes && before.content_hash == after.content_hash {
        Ok(())
    } else {
        Err(TranscodeVideoError::ArtifactChecksumMismatch {
            message: "input changed while transcode was running".to_owned(),
            payload: serde_json::json!({"stage": stage, "before": before, "after": after}),
        })
    }
}

fn success_dispatch(
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    progress: ProgressFrame,
    result: TranscodeVideoResult,
) -> Result<OperationDispatch, ProtocolError> {
    let payload = serde_json::to_value(result).map_err(|err| ProtocolError::InvalidPayload {
        detail: format!("transcode_video result encode: {err}"),
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
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    err: &TranscodeVideoError,
    seq: u64,
) -> Result<OperationDispatch, ProtocolError> {
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[error_frame(lease_id, err, seq)])?,
    ))
}

fn error_dispatch_with_progress(
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    progress: ProgressFrame,
    err: &TranscodeVideoError,
) -> Result<OperationDispatch, ProtocolError> {
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[progress, error_frame(lease_id, err, 1)])?,
    ))
}

fn progress_frame(lease_id: LeaseId, emitted_at: chrono::DateTime<chrono::Utc>) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq: 0,
        emitted_at,
        percent: None,
        message: Some("video transcode started".to_owned()),
        payload: Some(serde_json::json!({"provider": PROVIDER})),
    }
}

fn error_frame(lease_id: LeaseId, err: &TranscodeVideoError, seq: u64) -> ProgressFrame {
    ProgressFrame::Error {
        lease_id,
        seq,
        emitted_at: Utc::now(),
        class: err.failure_class(),
        code: err.error_code(),
        message: err.to_string(),
        payload: Some(err.payload()),
    }
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

impl From<ObserveError> for TranscodeVideoError {
    fn from(value: ObserveError) -> Self {
        match value {
            ObserveError::ArtifactUnavailable(message) => Self::ArtifactUnavailable {
                payload: serde_json::json!({"stage": "observe_file", "message": message}),
                message,
            },
            ObserveError::ArtifactChecksumMismatch(message) => Self::ArtifactChecksumMismatch {
                payload: serde_json::json!({"stage": "observe_file", "message": message}),
                message,
            },
        }
    }
}

impl From<FfmpegError> for TranscodeVideoError {
    fn from(value: FfmpegError) -> Self {
        match value {
            FfmpegError::FfmpegFailed(message) | FfmpegError::FfprobeFailed(message) => {
                Self::ExternalSystemUnavailable {
                    payload: serde_json::json!({"stage": "ffmpeg", "message": message}),
                    message,
                }
            }
            FfmpegError::OutputFactsMismatch(message) => Self::MalformedWorkerResult {
                payload: serde_json::json!({"stage": "output_probe", "message": message}),
                message,
            },
        }
    }
}

fn config_invalid(stage: &str, message: String) -> TranscodeVideoError {
    TranscodeVideoError::ConfigInvalid {
        payload: serde_json::json!({"stage": stage, "message": message}),
        message,
    }
}

fn malformed_worker_result(stage: &str, message: String) -> TranscodeVideoError {
    TranscodeVideoError::MalformedWorkerResult {
        payload: serde_json::json!({"stage": stage, "message": message}),
        message,
    }
}

#[cfg(test)]
#[path = "handler_test.rs"]
mod tests;
