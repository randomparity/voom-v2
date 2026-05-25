use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    OperationDispatch, OperationFuture, OperationHandler, OperationKind, OperationRequest,
    OperationResponse, ProgressFrame, ProtocolError, VerifyArtifactExpectedFacts,
    VerifyArtifactObservedFacts, VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

use crate::observe::{ObserveError, observe_file_facts};

const PROVIDER: &str = "voom-verify-artifact-worker";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyArtifactError {
    ArtifactUnavailable {
        message: String,
        payload: serde_json::Value,
    },
    ArtifactChecksumMismatch {
        message: String,
        payload: serde_json::Value,
    },
    MalformedWorkerResult {
        message: String,
        payload: serde_json::Value,
    },
}

impl VerifyArtifactError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        match self {
            Self::ArtifactUnavailable { .. } => FailureClass::ArtifactUnavailable,
            Self::ArtifactChecksumMismatch { .. } => FailureClass::ArtifactChecksumMismatch,
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
            | Self::MalformedWorkerResult { payload, .. } => payload.clone(),
        }
    }
}

impl Display for VerifyArtifactError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ArtifactUnavailable { message, .. } => {
                write!(f, "artifact unavailable: {message}")
            }
            Self::ArtifactChecksumMismatch { message, .. } => {
                write!(f, "artifact checksum mismatch: {message}")
            }
            Self::MalformedWorkerResult { message, .. } => {
                write!(f, "malformed worker result: {message}")
            }
        }
    }
}

impl std::error::Error for VerifyArtifactError {}

#[must_use]
pub fn handle_operation(req: OperationRequest) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::VerifyArtifact {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }

        let lease_id = req.lease_id;
        let accepted_at = Utc::now();
        let payload = match serde_json::from_value::<VerifyArtifactRequest>(req.payload) {
            Ok(payload) => payload,
            Err(err) => {
                return error_dispatch(
                    lease_id,
                    accepted_at,
                    &malformed_worker_result(
                        "decode_request",
                        format!("verify_artifact payload decode: {err}"),
                    ),
                    0,
                );
            }
        };

        let started = progress_frame(lease_id, accepted_at);
        match Box::pin(verify_artifact(&payload)).await {
            Ok(result) => success_dispatch(lease_id, accepted_at, started, result),
            Err(err) => error_dispatch_with_progress(lease_id, accepted_at, started, &err),
        }
    })
}

#[must_use]
pub fn operation_handler() -> OperationHandler {
    Arc::new(handle_operation)
}

async fn verify_artifact(
    request: &VerifyArtifactRequest,
) -> Result<VerifyArtifactResult, VerifyArtifactError> {
    let path = PathBuf::from(&request.path);
    let observed = Box::pin(observe_file_facts(&path))
        .await
        .map_err(VerifyArtifactError::from)?;
    verify_expected_facts(&observed, &request.expected)?;

    Ok(VerifyArtifactResult {
        status: VerifyArtifactStatus::Verified,
        provider: PROVIDER.to_owned(),
        provider_version: env!("CARGO_PKG_VERSION").to_owned(),
        observed,
    })
}

fn verify_expected_facts(
    observed: &VerifyArtifactObservedFacts,
    expected: &VerifyArtifactExpectedFacts,
) -> Result<(), VerifyArtifactError> {
    if observed.size_bytes == expected.size_bytes && observed.content_hash == expected.content_hash
    {
        return Ok(());
    }
    Err(VerifyArtifactError::ArtifactChecksumMismatch {
        message: "observed file facts differ from expected size/hash".to_owned(),
        payload: serde_json::json!({
            "stage": "verify_artifact",
            "expected": expected,
            "observed": observed,
        }),
    })
}

fn success_dispatch(
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    progress: ProgressFrame,
    result: VerifyArtifactResult,
) -> Result<OperationDispatch, ProtocolError> {
    let payload = serde_json::to_value(result).map_err(|err| ProtocolError::InvalidPayload {
        detail: format!("verify_artifact result encode: {err}"),
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
    err: &VerifyArtifactError,
    seq: u64,
) -> Result<OperationDispatch, ProtocolError> {
    let frame = error_frame(lease_id, err, seq);
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[frame])?,
    ))
}

fn error_dispatch_with_progress(
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    progress: ProgressFrame,
    err: &VerifyArtifactError,
) -> Result<OperationDispatch, ProtocolError> {
    let error = error_frame(lease_id, err, 1);
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[progress, error])?,
    ))
}

fn error_frame(lease_id: LeaseId, err: &VerifyArtifactError, seq: u64) -> ProgressFrame {
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

fn progress_frame(lease_id: LeaseId, emitted_at: chrono::DateTime<chrono::Utc>) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq: 0,
        emitted_at,
        percent: None,
        message: Some("artifact verification started".to_owned()),
        payload: Some(serde_json::json!({"provider": PROVIDER})),
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

impl From<ObserveError> for VerifyArtifactError {
    fn from(value: ObserveError) -> Self {
        match value {
            ObserveError::ArtifactUnavailable(message) => Self::ArtifactUnavailable {
                payload: serde_json::json!({
                    "stage": "observe_file",
                    "message": message,
                }),
                message,
            },
            ObserveError::ArtifactChecksumMismatch(message) => Self::ArtifactChecksumMismatch {
                payload: serde_json::json!({
                    "stage": "observe_file",
                    "message": message,
                }),
                message,
            },
        }
    }
}

fn malformed_worker_result(stage: &str, message: String) -> VerifyArtifactError {
    VerifyArtifactError::MalformedWorkerResult {
        payload: serde_json::json!({
            "stage": stage,
            "message": message,
        }),
        message,
    }
}

#[cfg(test)]
#[path = "handler_test.rs"]
mod tests;
