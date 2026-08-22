//! Worker-protocol dispatch for `HashFile` operations.
//!
//! A malformed request payload becomes a terminal `MalformedWorkerResult`
//! error frame on HTTP 200 (so retries replay it); a wrong operation kind is
//! a protocol error that evicts the worker. Operational failures (missing or
//! unreadable artifacts, `hash_drift`) are terminal error frames carrying the
//! `(class, code)` pair the pump classifies on.

use std::path::PathBuf;
use std::sync::Arc;

use time::OffsetDateTime;
use voom_worker_protocol::{
    HashFileRequest, HashFileResult, OperationDispatch, OperationFuture, OperationHandler,
    OperationKind, OperationRequest, OperationResponse, ProgressFrame, ProtocolError,
};

use crate::hash::{HashWorkerError, hash_file_in_root, malformed_worker_result};

#[must_use]
pub fn operation_handler() -> OperationHandler {
    Arc::new(|req| handle_operation(req))
}

fn handle_operation(req: OperationRequest) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::HashFile {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }

        let lease_id = req.lease_id;
        let accepted_at = OffsetDateTime::now_utc();
        // A malformed payload is a terminal worker result on HTTP 200 so
        // retries replay it; a protocol error would evict the idempotency row.
        let payload: HashFileRequest = match serde_json::from_value(req.payload) {
            Ok(payload) => payload,
            Err(err) => {
                let worker_err = malformed_worker_result(
                    "decode_request",
                    format!("hash_file payload decode: {err}"),
                );
                return error_dispatch(lease_id, accepted_at, &worker_err);
            }
        };

        let root = PathBuf::from(&payload.provider_locator);
        match Box::pin(hash_file_in_root(&root, &payload)).await {
            Ok(result) => success_dispatch(lease_id, accepted_at, result),
            Err(err) => error_dispatch(lease_id, accepted_at, &err),
        }
    })
}

fn success_dispatch(
    lease_id: voom_core::LeaseId,
    accepted_at: OffsetDateTime,
    result: HashFileResult,
) -> Result<OperationDispatch, ProtocolError> {
    let progress = ProgressFrame::Progress {
        lease_id,
        seq: 0,
        emitted_at: accepted_at,
        percent: None,
        message: Some("blake3 hashing completed".to_owned()),
        payload: Some(serde_json::json!({"provider": "voom-hash-worker"})),
    };
    let payload = serde_json::to_value(result).map_err(|err| ProtocolError::InvalidPayload {
        detail: format!("hash_file result encode: {err}"),
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
    lease_id: voom_core::LeaseId,
    accepted_at: OffsetDateTime,
    err: &HashWorkerError,
) -> Result<OperationDispatch, ProtocolError> {
    let frame = ProgressFrame::Error {
        lease_id,
        seq: 0,
        emitted_at: OffsetDateTime::now_utc(),
        class: err.failure_class(),
        code: err.error_code(),
        message: err.to_string(),
        payload: Some(err.payload().clone()),
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

#[cfg(test)]
#[path = "handler_test.rs"]
mod tests;
