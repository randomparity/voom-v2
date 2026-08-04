use std::path::PathBuf;
use std::sync::Arc;

use time::OffsetDateTime;
use voom_core::format_iso8601;
use voom_worker_protocol::{
    OperationDispatch, OperationFuture, OperationHandler, OperationKind, OperationRequest,
    OperationResponse, ProbeFileRequest, ProbeFileResult, ProbeFileStatus, ProgressFrame,
    ProtocolError,
};

use crate::ffprobe::{FfprobeConfig, FfprobeError, malformed_worker_result, run_ffprobe_json};
use crate::{WorkerError, normalize_ffprobe_json, observe_file_facts};

#[must_use]
pub fn operation_handler(config: FfprobeConfig) -> OperationHandler {
    Arc::new(move |req| handle_operation(req, config.clone()))
}

fn handle_operation(req: OperationRequest, config: FfprobeConfig) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::ProbeFile {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }

        let lease_id = req.lease_id;
        let accepted_at = OffsetDateTime::now_utc();
        // A malformed payload is a terminal worker result on HTTP 200 so
        // retries replay it; a protocol error would evict the idempotency row.
        let payload: ProbeFileRequest = match serde_json::from_value(req.payload) {
            Ok(payload) => payload,
            Err(err) => {
                let worker_err = malformed_worker_result(
                    "decode_request",
                    format!("probe_file payload decode: {err}"),
                );
                return error_dispatch(lease_id, accepted_at, &worker_err);
            }
        };

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
    let probed_at = format_iso8601(OffsetDateTime::now_utc());
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
    accepted_at: OffsetDateTime,
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
    err: &FfprobeError,
) -> Result<OperationDispatch, ProtocolError> {
    let frame = ProgressFrame::Error {
        lease_id,
        seq: 0,
        emitted_at: OffsetDateTime::now_utc(),
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
            WorkerError::ArtifactChecksumMismatch(message) => Self::ArtifactChecksumMismatch {
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

fn checksum_mismatch(stage: &str, message: &str, payload: serde_json::Value) -> FfprobeError {
    FfprobeError::ArtifactChecksumMismatch {
        payload,
        message: format!("{stage}: {message}"),
    }
}
