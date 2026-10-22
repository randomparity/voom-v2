//! Worker-protocol dispatch for `HashFile` operations.
//!
//! A malformed request payload becomes a terminal `MalformedWorkerResult`
//! error frame on HTTP 200 (so retries replay it); a wrong operation kind is
//! a protocol error that evicts the worker. Operational failures (missing or
//! unreadable artifacts, `hash_drift`) are terminal error frames carrying the
//! `(class, code)` pair the pump classifies on.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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

        stream_operation(
            lease_id,
            accepted_at,
            req.progress_idle_deadline_ms,
            async move {
                let root = PathBuf::from(&payload.provider_locator);
                Box::pin(hash_file_in_root(&root, &payload)).await
            },
        )
    })
}

fn stream_operation<F>(
    lease_id: voom_core::LeaseId,
    accepted_at: OffsetDateTime,
    progress_idle_deadline_ms: u32,
    operation: F,
) -> Result<OperationDispatch, ProtocolError>
where
    F: Future<Output = Result<HashFileResult, HashWorkerError>> + Send + 'static,
{
    let (mut writer, dispatch) = OperationDispatch::streaming(OperationResponse {
        lease_id,
        accepted_at,
    });
    writer.write_frame(&progress_frame(lease_id, 0))?;
    tokio::spawn(async move {
        let interval_ms = (u64::from(progress_idle_deadline_ms) / 2).clamp(1, 5_000);
        let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
        ticker.tick().await;
        let mut operation = Box::pin(operation);
        let mut seq = 1;
        loop {
            tokio::select! {
                biased;
                result = &mut operation => {
                    let frame = terminal_frame(lease_id, seq, result);
                    let _ = writer.write_frame(&frame);
                    return;
                }
                _ = ticker.tick() => {
                    if writer.write_frame(&progress_frame(lease_id, seq)).is_err() {
                        return;
                    }
                    seq += 1;
                }
            }
        }
    });
    Ok(dispatch)
}

fn progress_frame(lease_id: voom_core::LeaseId, seq: u64) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq,
        emitted_at: OffsetDateTime::now_utc(),
        percent: None,
        message: Some("blake3 hashing active".to_owned()),
        payload: Some(serde_json::json!({"provider": "voom-hash-worker"})),
    }
}

fn terminal_frame(
    lease_id: voom_core::LeaseId,
    seq: u64,
    result: Result<HashFileResult, HashWorkerError>,
) -> ProgressFrame {
    let result = result.and_then(|result| {
        serde_json::to_value(result).map_err(|err| {
            malformed_worker_result("encode_result", format!("operation result encode: {err}"))
        })
    });
    match result {
        Ok(payload) => ProgressFrame::Result {
            lease_id,
            seq,
            emitted_at: OffsetDateTime::now_utc(),
            payload,
        },
        Err(err) => error_frame(lease_id, seq, &err),
    }
}

fn error_dispatch(
    lease_id: voom_core::LeaseId,
    accepted_at: OffsetDateTime,
    err: &HashWorkerError,
) -> Result<OperationDispatch, ProtocolError> {
    let frame = error_frame(lease_id, 0, err);
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[frame])?,
    ))
}

fn error_frame(lease_id: voom_core::LeaseId, seq: u64, err: &HashWorkerError) -> ProgressFrame {
    ProgressFrame::Error {
        lease_id,
        seq,
        emitted_at: OffsetDateTime::now_utc(),
        class: err.failure_class(),
        code: err.error_code(),
        message: err.to_string(),
        payload: Some(err.payload().clone()),
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

#[cfg(test)]
#[path = "handler_test.rs"]
mod tests;
