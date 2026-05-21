#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "chaos-worker tests use direct fixture assertions"
    )
)]

use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;
use voom_worker_protocol::{OperationRequest, ProgressFrame, ProtocolError};

const MAX_DURATION_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChaosMode {
    Baseline,
    Crash,
    Stall,
    MalformedResult,
    NonConvergingProgress,
    DeadlineExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChaosPayload {
    path: String,
    mode: ChaosMode,
    progress_count: usize,
    progress_interval: Duration,
    stall: Duration,
}

#[derive(Debug, Deserialize)]
struct RawChaosPayload {
    path: Option<String>,
    mode: Option<String>,
    progress_count: Option<u64>,
    progress_interval_ms: Option<u64>,
    stall_ms: Option<u64>,
}

fn main() {}

fn parse_payload(value: serde_json::Value) -> Result<ChaosPayload, ProtocolError> {
    let raw: RawChaosPayload =
        serde_json::from_value(value).map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("chaos payload decode: {e}"),
        })?;
    let path = raw
        .path
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| ProtocolError::InvalidPayload {
            detail: "payload missing path".to_owned(),
        })?;
    let mode = match raw.mode.as_deref().unwrap_or("baseline") {
        "baseline" => ChaosMode::Baseline,
        "crash" => ChaosMode::Crash,
        "stall" => ChaosMode::Stall,
        "malformed_result" => ChaosMode::MalformedResult,
        "non_converging_progress" => ChaosMode::NonConvergingProgress,
        "deadline_exceeded" => ChaosMode::DeadlineExceeded,
        other => {
            return Err(ProtocolError::InvalidPayload {
                detail: format!("unknown chaos mode {other}"),
            });
        }
    };
    let progress_count = raw.progress_count.unwrap_or(3);
    if progress_count > 128 {
        return Err(ProtocolError::InvalidPayload {
            detail: "progress_count > 128".to_owned(),
        });
    }
    let progress_interval = checked_duration("progress_interval_ms", raw.progress_interval_ms, 50)?;
    let stall = checked_duration("stall_ms", raw.stall_ms, 500)?;
    Ok(ChaosPayload {
        path,
        mode,
        progress_count: progress_count as usize,
        progress_interval,
        stall,
    })
}

fn checked_duration(
    field: &'static str,
    value: Option<u64>,
    default_ms: u64,
) -> Result<Duration, ProtocolError> {
    let ms = value.unwrap_or(default_ms);
    if ms > MAX_DURATION_MS {
        return Err(ProtocolError::InvalidPayload {
            detail: format!("{field} > {MAX_DURATION_MS}"),
        });
    }
    Ok(Duration::from_millis(ms))
}

fn baseline_body(req: &OperationRequest, payload: &ChaosPayload) -> Result<Vec<u8>, ProtocolError> {
    let now = Utc::now();
    let progress = ProgressFrame::Progress {
        lease_id: req.lease_id,
        seq: 0,
        emitted_at: now,
        percent: None,
        message: Some(format!("chaos baseline {}", payload.path)),
        payload: Some(serde_json::json!({"mode": "baseline", "path": payload.path})),
    };
    let result = ProgressFrame::Result {
        lease_id: req.lease_id,
        seq: 1,
        emitted_at: now,
        payload: serde_json::json!({"mode": "baseline", "path": payload.path}),
    };
    let mut body = Vec::new();
    push_frame(&mut body, &progress)?;
    push_frame(&mut body, &result)?;
    Ok(body)
}

fn malformed_body() -> Vec<u8> {
    b"{not-json\n".to_vec()
}

fn push_frame(out: &mut Vec<u8>, frame: &ProgressFrame) -> Result<(), ProtocolError> {
    out.extend_from_slice(&serde_json::to_vec(frame).map_err(|e| {
        ProtocolError::InvalidPayload {
            detail: format!("frame encode: {e}"),
        }
    })?);
    out.push(b'\n');
    Ok(())
}

#[cfg(test)]
#[path = "chaos_worker_test.rs"]
mod tests;
