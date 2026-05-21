#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "benchmark-worker tests use direct fixture assertions"
    )
)]
#![expect(
    clippy::print_stdout,
    reason = "benchmark-worker advertises readiness with BOUND addr=..."
)]

use chrono::{DateTime, Utc};
use serde::Deserialize;
use secrecy::SecretString;
use std::hint::black_box;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use voom_worker_protocol::http::OperationDispatch;
use voom_worker_protocol::{
    HttpServer, OperationFuture, OperationKind, OperationRequest, OperationResponse,
    ProgressFrame, ProtocolError, ServerHandle, WorkerCredentials,
};

const MAX_BENCHMARK_OPERATIONS: u64 = 10_000;
const MAX_BENCHMARK_PROGRESS_FRAMES: u64 = 100;
const MAX_BENCHMARK_RESPONSE_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchmarkMode {
    Baseline,
    Benchmark,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchmarkPayload {
    path: String,
    mode: BenchmarkMode,
    operations: Option<u64>,
    emit_every: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchmarkConfig {
    path: String,
    operations: u64,
    emit_every: u64,
    progress_frames: u64,
}

#[derive(Debug, Deserialize)]
struct RawBenchmarkPayload {
    path: Option<String>,
    mode: Option<String>,
    operations: Option<u64>,
    emit_every: Option<u64>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let credentials = load_credentials()?;
    let bind: SocketAddr = std::env::var("VOOM_WORKER_BIND")
        .unwrap_or_else(|_| "127.0.0.1:0".to_owned())
        .parse()
        .map_err(|e| format!("VOOM_WORKER_BIND parse failed: {e}"))?;
    let server = HttpServer::new(credentials, Arc::new(handle_operation));
    let running = server
        .serve(bind)
        .await
        .map_err(|e| format!("serve failed: {e}"))?;
    println!("BOUND addr={}", running.bound);
    let shutdown_tx = running.shutdown;
    let joined = running.joined;
    let watchdog = tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(_)) = stdin.next_line().await {}
        let _ = shutdown_tx.send(());
    });
    let _ = watchdog.await;
    let _ = joined.await;
    Ok(())
}

fn load_credentials() -> Result<WorkerCredentials, Box<dyn std::error::Error>> {
    let secret = std::env::var("VOOM_WORKER_SECRET").map_err(|_| "VOOM_WORKER_SECRET not set")?;
    let worker_id: u64 = std::env::var("VOOM_WORKER_ID")
        .map_err(|_| "VOOM_WORKER_ID not set")?
        .parse()
        .map_err(|_| "VOOM_WORKER_ID not parseable")?;
    let worker_epoch: u64 = std::env::var("VOOM_WORKER_EPOCH")
        .map_err(|_| "VOOM_WORKER_EPOCH not set")?
        .parse()
        .map_err(|_| "VOOM_WORKER_EPOCH not parseable")?;
    Ok(WorkerCredentials {
        worker_id: voom_core::WorkerId(worker_id),
        worker_epoch,
        secret: SecretString::from(secret),
    })
}

fn handle_operation(req: OperationRequest) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::ProbeFile {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }
        let payload = parse_payload(req.payload.clone())?;
        match payload.mode {
            BenchmarkMode::Baseline => baseline_dispatch(&req, &payload.path),
            BenchmarkMode::Benchmark => {
                let config = benchmark_config(&payload)?;
                benchmark_dispatch(&req, &config)
            }
        }
    })
}

fn parse_payload(value: serde_json::Value) -> Result<BenchmarkPayload, ProtocolError> {
    let raw: RawBenchmarkPayload =
        serde_json::from_value(value).map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("benchmark payload decode: {e}"),
        })?;
    let path = raw
        .path
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| ProtocolError::InvalidPayload {
            detail: "payload missing path".to_owned(),
        })?;
    let mode = match raw.mode.as_deref().unwrap_or("baseline") {
        "baseline" => BenchmarkMode::Baseline,
        "benchmark" => BenchmarkMode::Benchmark,
        other => {
            return Err(ProtocolError::InvalidPayload {
                detail: format!("unknown benchmark mode {other}"),
            });
        }
    };
    let payload = BenchmarkPayload {
        path,
        mode,
        operations: raw.operations,
        emit_every: raw.emit_every,
    };
    if mode == BenchmarkMode::Benchmark {
        let _ = benchmark_config(&payload)?;
    }
    Ok(payload)
}

fn benchmark_config(payload: &BenchmarkPayload) -> Result<BenchmarkConfig, ProtocolError> {
    let operations = payload
        .operations
        .ok_or_else(|| ProtocolError::InvalidPayload {
            detail: "benchmark operations missing".to_owned(),
        })?;
    if operations == 0 || operations > MAX_BENCHMARK_OPERATIONS {
        return Err(ProtocolError::InvalidPayload {
            detail: format!("operations must be 1..={MAX_BENCHMARK_OPERATIONS}"),
        });
    }
    let emit_every = payload.emit_every.unwrap_or(operations);
    if emit_every == 0 || emit_every > operations {
        return Err(ProtocolError::InvalidPayload {
            detail: "emit_every must be within 1..=operations".to_owned(),
        });
    }
    let progress_frames = operations.div_ceil(emit_every);
    if progress_frames == 0 || progress_frames > MAX_BENCHMARK_PROGRESS_FRAMES {
        return Err(ProtocolError::InvalidPayload {
            detail: format!("progress_frames > {MAX_BENCHMARK_PROGRESS_FRAMES}"),
        });
    }
    Ok(BenchmarkConfig {
        path: payload.path.clone(),
        operations,
        emit_every,
        progress_frames,
    })
}

fn baseline_dispatch(
    req: &OperationRequest,
    path: &str,
) -> Result<OperationDispatch, ProtocolError> {
    let now = Utc::now();
    let progress = ProgressFrame::Progress {
        lease_id: req.lease_id,
        seq: 0,
        emitted_at: now,
        percent: None,
        message: Some(format!("benchmark baseline {path}")),
        payload: Some(serde_json::json!({"mode": "baseline", "path": path})),
    };
    let result = ProgressFrame::Result {
        lease_id: req.lease_id,
        seq: 1,
        emitted_at: now,
        payload: serde_json::json!({"mode": "baseline", "path": path}),
    };
    let body = body_from_frames(&[progress, result])?;
    Ok(OperationDispatch {
        response: OperationResponse {
            lease_id: req.lease_id,
            accepted_at: now,
        },
        body,
    })
}

fn benchmark_dispatch(
    req: &OperationRequest,
    config: &BenchmarkConfig,
) -> Result<OperationDispatch, ProtocolError> {
    benchmark_dispatch_with_body_limit(req, config, MAX_BENCHMARK_RESPONSE_BODY_BYTES)
}

fn benchmark_dispatch_with_body_limit(
    req: &OperationRequest,
    config: &BenchmarkConfig,
    max_body_bytes: usize,
) -> Result<OperationDispatch, ProtocolError> {
    let accepted_at = Utc::now();
    let started_at = Utc::now();
    let started_instant = Instant::now();
    let mut frames = Vec::new();
    let mut completed = 0_u64;
    let mut sample_index = 0_u64;
    let mut operation_accumulator = 0_u64;
    while completed < config.operations {
        let next_completed = (completed + config.emit_every).min(config.operations);
        for operation_index in completed..next_completed {
            operation_accumulator = operation_accumulator.wrapping_add(black_box(operation_index));
        }
        completed = next_completed;
        let elapsed_worker_ns = elapsed_worker_ns(started_instant);
        frames.push(ProgressFrame::Progress {
            lease_id: req.lease_id,
            seq: sample_index,
            emitted_at: Utc::now(),
            percent: None,
            message: Some(format!(
                "benchmark {completed}/{} operations",
                config.operations
            )),
            payload: Some(serde_json::json!({
                "mode": "benchmark",
                "operations_total": config.operations,
                "operations_completed": completed,
                "elapsed_worker_ns": elapsed_worker_ns,
                "sample_index": sample_index,
            })),
        });
        sample_index += 1;
    }
    let _ = black_box(operation_accumulator);
    let completed_at = Utc::now();
    let elapsed_worker_ns = elapsed_worker_ns(started_instant);
    frames.push(ProgressFrame::Result {
        lease_id: req.lease_id,
        seq: sample_index,
        emitted_at: completed_at,
        payload: benchmark_result_payload(config, sample_index, elapsed_worker_ns, started_at, completed_at),
    });
    let body = body_from_frames(&frames)?;
    enforce_benchmark_body_size(&body, max_body_bytes)?;
    Ok(OperationDispatch {
        response: OperationResponse {
            lease_id: req.lease_id,
            accepted_at,
        },
        body,
    })
}

fn benchmark_result_payload(
    config: &BenchmarkConfig,
    progress_frames: u64,
    elapsed_worker_ns: u64,
    started_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
) -> serde_json::Value {
    let worker_ops_per_second_milli = ((u128::from(config.operations) * 1_000_000_000_000_u128)
        / u128::from(elapsed_worker_ns))
    .min(u128::from(u64::MAX)) as u64;
    serde_json::json!({
        "mode": "benchmark",
        "operations_total": config.operations,
        "progress_frames": progress_frames,
        "elapsed_worker_ns": elapsed_worker_ns,
        "worker_ops_per_second_milli": worker_ops_per_second_milli,
        "first_operation_started_at": started_at,
        "completed_at": completed_at,
    })
}

fn elapsed_worker_ns(started_instant: Instant) -> u64 {
    (started_instant
        .elapsed()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64)
        .max(1)
}

fn body_from_frames(frames: &[ProgressFrame]) -> Result<Vec<u8>, ProtocolError> {
    let mut body = Vec::new();
    for frame in frames {
        body.extend_from_slice(&serde_json::to_vec(frame).map_err(|e| {
            ProtocolError::InvalidPayload {
                detail: format!("frame encode: {e}"),
            }
        })?);
        body.push(b'\n');
    }
    Ok(body)
}

fn enforce_benchmark_body_size(
    body: &[u8],
    max_body_bytes: usize,
) -> Result<(), ProtocolError> {
    if body.len() > max_body_bytes {
        Err(ProtocolError::InvalidPayload {
            detail: format!("benchmark response body {} > {max_body_bytes}", body.len()),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "benchmark_worker_test.rs"]
mod tests;
