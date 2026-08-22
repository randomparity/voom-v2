//! `scan_library` worker operation handler (ADR 0077).
//!
//! Decodes [`ScanLibraryRequest`] strictly, runs the metadata-only root walk,
//! and emits candidate groups as progress frames — each under both protocol
//! bounds — followed by a terminal result frame carrying
//! [`ScanLibraryResult`]. A malformed request payload or a root-level walk
//! failure is a terminal `Error` frame on HTTP 200 so retries replay it; a
//! protocol error would evict the idempotency row.

use std::path::PathBuf;
use std::sync::Arc;

use time::OffsetDateTime;
use voom_core::{ErrorCode, FailureClass};
use voom_worker_protocol::{
    MAX_PROGRESS_CANDIDATES, MAX_PROGRESS_PAYLOAD_BYTES, OperationDispatch, OperationFuture,
    OperationHandler, OperationKind, OperationRequest, OperationResponse, ProgressFrame,
    ProtocolError, ScanCandidate, ScanCandidateFile, ScanLibraryRequest, ScanLibraryResult,
    encode_candidate_progress,
};

use crate::walk::{RootUnavailable, WalkOutcome, scan_root};

/// Terminal failure of one `scan_library` dispatch.
#[derive(Debug, thiserror::Error)]
pub enum ScanWorkerError {
    /// The worker's own input or output was structurally unusable — a
    /// malformed request payload or a frame-bound violation. Permanent.
    #[error("malformed worker result: {message}")]
    MalformedWorkerResult { message: String },
    /// The scan root itself is unavailable (missing, unreadable, symlinked,
    /// or not a directory). Retrying cannot succeed without operator action.
    #[error("artifact unavailable: {message}")]
    RootUnavailable { message: String },
}

impl ScanWorkerError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        match self {
            Self::MalformedWorkerResult { .. } => FailureClass::MalformedWorkerResult,
            Self::RootUnavailable { .. } => FailureClass::ArtifactUnavailable,
        }
    }

    #[must_use]
    pub const fn error_code(&self) -> ErrorCode {
        self.failure_class().into_error_code()
    }

    #[must_use]
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Self::MalformedWorkerResult { message } | Self::RootUnavailable { message } => {
                serde_json::json!({ "message": message })
            }
        }
    }
}

pub(crate) fn malformed_worker_result(stage: &str, message: &str) -> ScanWorkerError {
    ScanWorkerError::MalformedWorkerResult {
        message: format!("{stage}: {message}"),
    }
}

#[must_use]
pub fn operation_handler() -> OperationHandler {
    Arc::new(|req| handle_operation(req))
}

fn handle_operation(req: OperationRequest) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::ScanLibrary {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }

        let lease_id = req.lease_id;
        let accepted_at = OffsetDateTime::now_utc();
        // A malformed payload is a terminal worker result on HTTP 200 so
        // retries replay it; a protocol error would evict the idempotency row.
        let payload: ScanLibraryRequest = match serde_json::from_value(req.payload) {
            Ok(payload) => payload,
            Err(err) => {
                return error_dispatch(
                    lease_id,
                    accepted_at,
                    &malformed_worker_result(
                        "decode_request",
                        &format!("scan_library payload decode: {err}"),
                    ),
                );
            }
        };

        // Walk and frame-build failures happen before any frame is written,
        // so they take the buffered error-dispatch path; only write failures
        // can abort a partially streamed body.
        let built = run_scan(&payload)
            .await
            .and_then(|outcome| build_frames(lease_id, &outcome));
        let frames = match built {
            Ok(frames) => frames,
            Err(err) => return error_dispatch(lease_id, accepted_at, &err),
        };

        let (mut writer, dispatch) = OperationDispatch::streaming(OperationResponse {
            lease_id,
            accepted_at,
        });
        for frame in &frames {
            writer.write_frame(frame)?;
        }
        writer.finish()?;
        Ok(dispatch)
    })
}

/// Walk the requested root on the blocking pool (the walk is sync `std::fs`).
async fn run_scan(request: &ScanLibraryRequest) -> Result<WalkOutcome, ScanWorkerError> {
    let root = PathBuf::from(&request.provider_locator);
    let extension_allowlist = request.extension_allowlist.clone();
    let walked = tokio::task::spawn_blocking(move || scan_root(&root, &extension_allowlist))
        .await
        .map_err(|err| ScanWorkerError::RootUnavailable {
            message: format!("scan worker task failed: {err}"),
        })?;
    walked.map_err(
        |failure: RootUnavailable| ScanWorkerError::RootUnavailable {
            message: failure.to_string(),
        },
    )
}

/// Build the full frame sequence for one completed walk: one progress frame
/// per candidate batch under both protocol bounds, then a terminal result
/// frame carrying [`ScanLibraryResult`]. An empty enumeration produces only
/// the result frame.
fn build_frames(
    lease_id: voom_core::LeaseId,
    outcome: &WalkOutcome,
) -> Result<Vec<ProgressFrame>, ScanWorkerError> {
    let candidates: Vec<ScanCandidate> = outcome
        .candidates
        .iter()
        .map(|candidate| ScanCandidate {
            primary: wire_file(&candidate.primary),
            sidecars: candidate.sidecars.iter().map(wire_file).collect(),
        })
        .collect();
    let discovered_count = u64::try_from(candidates.len())
        .map_err(|_| malformed_worker_result("count_candidates", "primary count exceeds u64"))?;

    let mut frames = Vec::new();
    for (seq, payload) in batch_candidate_payloads(&candidates)?
        .into_iter()
        .enumerate()
    {
        frames.push(ProgressFrame::Progress {
            lease_id,
            seq: u64::try_from(seq)
                .map_err(|_| malformed_worker_result("sequence", "seq exceeds u64"))?,
            emitted_at: OffsetDateTime::now_utc(),
            percent: None,
            message: Some("scan candidates".to_owned()),
            payload: Some(payload),
        });
    }
    let payload = serde_json::to_value(ScanLibraryResult {
        discovered_count,
        skipped_count: outcome.skipped_count,
    })
    .map_err(|err| {
        malformed_worker_result("encode_result", &format!("scan result encode: {err}"))
    })?;
    frames.push(ProgressFrame::Result {
        lease_id,
        seq: u64::try_from(frames.len())
            .map_err(|_| malformed_worker_result("sequence", "seq exceeds u64"))?,
        emitted_at: OffsetDateTime::now_utc(),
        payload,
    });
    Ok(frames)
}

fn wire_file(file: &crate::walk::WalkFile) -> ScanCandidateFile {
    ScanCandidateFile {
        provider_relative_locator: file.locator.as_str().to_owned(),
        provider_object_identity: file.provider_object_identity.clone(),
        size_bytes: file.size_bytes,
        modified_at: file.modified_at.clone(),
        kind: file.kind.map(str::to_owned),
    }
}

/// Split candidates into progress payloads that each respect both frame
/// bounds: at most [`MAX_PROGRESS_CANDIDATES`] candidates and at most
/// [`MAX_PROGRESS_PAYLOAD_BYTES`] serialized bytes. Splitting is decided by
/// actually encoding each prospective batch, so an overflow is detected
/// before a frame is ever emitted.
///
/// # Errors
///
/// Returns [`ScanWorkerError::MalformedWorkerResult`] when a single candidate
/// already exceeds the serialized-frame budget on its own — no split can make
/// such a frame legal.
pub fn batch_candidate_payloads(
    candidates: &[ScanCandidate],
) -> Result<Vec<serde_json::Value>, ScanWorkerError> {
    let mut batches = Vec::new();
    let mut current: Vec<ScanCandidate> = Vec::new();
    for candidate in candidates {
        current.push(candidate.clone());
        if encode_candidate_progress(&current).is_ok() {
            continue;
        }
        let Some(carried) = current.pop() else {
            return Err(malformed_worker_result("batch_candidates", "empty batch"));
        };
        if !current.is_empty() {
            batches.push(encoded_batch(&current)?);
            current.clear();
        }
        current.push(carried);
        // A lone candidate that still overflows cannot be split any further.
        encoded_batch(&current)?;
    }
    if !current.is_empty() {
        batches.push(encoded_batch(&current)?);
    }
    Ok(batches)
}

fn encoded_batch(batch: &[ScanCandidate]) -> Result<serde_json::Value, ScanWorkerError> {
    debug_assert!(batch.len() <= MAX_PROGRESS_CANDIDATES);
    let payload = encode_candidate_progress(batch)
        .map_err(|err| malformed_worker_result("batch_candidates", &err.to_string()))?;
    debug_assert!(
        serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= MAX_PROGRESS_PAYLOAD_BYTES)
    );
    Ok(payload)
}

fn error_frame(lease_id: voom_core::LeaseId, seq: u64, err: &ScanWorkerError) -> ProgressFrame {
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

fn error_dispatch(
    lease_id: voom_core::LeaseId,
    accepted_at: OffsetDateTime,
    err: &ScanWorkerError,
) -> Result<OperationDispatch, ProtocolError> {
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[error_frame(lease_id, 0, err)])?,
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
