use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

use time::OffsetDateTime;
use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    BackUpFileRequest, BackUpFileResult, BackUpFileStatus, OperationDispatch, OperationFuture,
    OperationHandler, OperationKind, OperationRequest, OperationResponse, ProgressFrame,
    ProtocolError,
};

use crate::backup::{BackupIoError, back_up_file};

const PROVIDER: &str = "voom-backup-worker";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackUpFileError {
    ArtifactUnavailable {
        message: String,
        payload: serde_json::Value,
    },
    BackupFailure {
        message: String,
        payload: serde_json::Value,
    },
    MalformedWorkerResult {
        message: String,
        payload: serde_json::Value,
    },
}

impl BackUpFileError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        match self {
            Self::ArtifactUnavailable { .. } => FailureClass::ArtifactUnavailable,
            Self::BackupFailure { .. } => FailureClass::BackupFailure,
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
            | Self::BackupFailure { payload, .. }
            | Self::MalformedWorkerResult { payload, .. } => payload.clone(),
        }
    }
}

impl Display for BackUpFileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ArtifactUnavailable { message, .. } => {
                write!(f, "artifact unavailable: {message}")
            }
            Self::BackupFailure { message, .. } => write!(f, "backup failed: {message}"),
            Self::MalformedWorkerResult { message, .. } => {
                write!(f, "malformed worker result: {message}")
            }
        }
    }
}

impl std::error::Error for BackUpFileError {}

impl From<BackupIoError> for BackUpFileError {
    fn from(value: BackupIoError) -> Self {
        match value {
            BackupIoError::ArtifactUnavailable(message) => Self::ArtifactUnavailable {
                payload: serde_json::json!({ "stage": "back_up_file", "message": message }),
                message,
            },
            BackupIoError::BackupFailure(message) => Self::BackupFailure {
                payload: serde_json::json!({ "stage": "back_up_file", "message": message }),
                message,
            },
        }
    }
}

#[must_use]
pub fn handle_operation(req: OperationRequest) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::BackUpFile {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }

        let lease_id = req.lease_id;
        let accepted_at = OffsetDateTime::now_utc();
        let request = match serde_json::from_value::<BackUpFileRequest>(req.payload) {
            Ok(request) => request,
            Err(err) => {
                return error_dispatch(
                    lease_id,
                    accepted_at,
                    &malformed_worker_result(format!("back_up_file payload decode: {err}")),
                );
            }
        };

        let started = progress_frame(lease_id, accepted_at);
        match Box::pin(run_backup(&request)).await {
            Ok(result) => success_dispatch(lease_id, accepted_at, started, result),
            Err(err) => error_dispatch_with_progress(lease_id, accepted_at, started, &err),
        }
    })
}

#[must_use]
pub fn operation_handler() -> OperationHandler {
    Arc::new(handle_operation)
}

async fn run_backup(request: &BackUpFileRequest) -> Result<BackUpFileResult, BackUpFileError> {
    let source = PathBuf::from(&request.source_path);
    let destination = PathBuf::from(&request.destination_path);
    let outcome = back_up_file(&source, &destination)
        .await
        .map_err(BackUpFileError::from)?;

    Ok(BackUpFileResult {
        status: BackUpFileStatus::BackedUp,
        provider: PROVIDER.to_owned(),
        provider_version: env!("CARGO_PKG_VERSION").to_owned(),
        destination_path: request.destination_path.clone(),
        size_bytes: outcome.size_bytes,
        checksum: outcome.checksum,
    })
}

fn success_dispatch(
    lease_id: LeaseId,
    accepted_at: OffsetDateTime,
    progress: ProgressFrame,
    result: BackUpFileResult,
) -> Result<OperationDispatch, ProtocolError> {
    let payload = serde_json::to_value(result).map_err(|err| ProtocolError::InvalidPayload {
        detail: format!("back_up_file result encode: {err}"),
    })?;
    let result = ProgressFrame::Result {
        lease_id,
        seq: 1,
        emitted_at: OffsetDateTime::now_utc(),
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
    accepted_at: OffsetDateTime,
    err: &BackUpFileError,
) -> Result<OperationDispatch, ProtocolError> {
    let frame = error_frame(lease_id, err, 0);
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
    accepted_at: OffsetDateTime,
    progress: ProgressFrame,
    err: &BackUpFileError,
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

fn error_frame(lease_id: LeaseId, err: &BackUpFileError, seq: u64) -> ProgressFrame {
    ProgressFrame::Error {
        lease_id,
        seq,
        emitted_at: OffsetDateTime::now_utc(),
        class: err.failure_class(),
        code: err.error_code(),
        message: err.to_string(),
        payload: Some(err.payload()),
    }
}

fn progress_frame(lease_id: LeaseId, emitted_at: OffsetDateTime) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq: 0,
        emitted_at,
        percent: None,
        message: Some("backup started".to_owned()),
        payload: Some(serde_json::json!({ "provider": PROVIDER })),
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

fn malformed_worker_result(message: String) -> BackUpFileError {
    BackUpFileError::MalformedWorkerResult {
        payload: serde_json::json!({ "stage": "decode_request", "message": message }),
        message,
    }
}

#[cfg(test)]
#[path = "handler_test.rs"]
mod tests;
