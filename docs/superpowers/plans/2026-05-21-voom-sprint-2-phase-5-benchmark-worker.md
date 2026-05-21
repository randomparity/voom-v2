# Sprint 2 Phase 5 Benchmark Worker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote `benchmark-worker` from scaffold to active conformance target and add bounded, schema-stable benchmark metric frames.

**Architecture:** Implement `benchmark-worker` as a local HTTP worker using `voom-worker-protocol::HttpServer`, because benchmark mode only needs buffered complete response bodies. Keep benchmark behavior in `crates/voom-fakes/src/bin/benchmark_worker.rs`, add sibling parser/frame tests, add process-backed integration tests, then promote the binary in the conformance manifest.

**Tech Stack:** Rust 2024, Tokio, `voom-worker-protocol` HTTP transport and wire types, `serde_json`, `chrono`, `secrecy`, `voom-conformance` manifest/integration tests.

---

## File Structure

- Modify `crates/voom-fakes/src/bin/benchmark_worker.rs`: replace scaffold with credential/bootstrap code, payload parser, baseline frame builder, benchmark frame builder, and `HttpServer` operation handler.
- Create `crates/voom-fakes/src/bin/benchmark_worker_test.rs`: sibling unit tests for payload parsing, frame cadence, body-size guard, and response payload schema.
- Create `crates/voom-fakes/tests/benchmark_worker.rs`: process-backed integration tests for launch, baseline, benchmark mode, idempotent replay, idempotency conflict, and invalid payloads.
- Modify `crates/voom-conformance/voom-fakes-manifest.toml`: promote `benchmark-worker` from scaffold to active required worker.
- Modify `crates/voom-conformance/tests/conformance_all.rs`: assert `benchmark-worker` is active and no longer scaffolded.
- Modify `crates/voom-conformance/src/manifest_test.rs`: update scaffold expectations and add/extend active-entry parsing coverage for `benchmark-worker`.

## Task 1: Benchmark Payload Parser and Frame Builders

**Files:**
- Modify: `crates/voom-fakes/src/bin/benchmark_worker.rs`
- Create: `crates/voom-fakes/src/bin/benchmark_worker_test.rs`

- [ ] **Step 1: Replace scaffold with parser-first implementation**

Replace `crates/voom-fakes/src/bin/benchmark_worker.rs` with this parser/frame-builder skeleton. Keep `main` empty for this task so the unit tests can drive the internal behavior first:

```rust
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
use voom_worker_protocol::{
    OperationKind, OperationRequest, OperationResponse, ProgressFrame, ProtocolError,
};
use voom_worker_protocol::http::OperationDispatch;

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

fn main() {}

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
    let operations = payload.operations.ok_or_else(|| ProtocolError::InvalidPayload {
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

fn baseline_dispatch(req: &OperationRequest, path: &str) -> Result<OperationDispatch, ProtocolError> {
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
    let accepted_at = Utc::now();
    let started_at = Utc::now();
    let completed_at = Utc::now();
    let mut frames = Vec::new();
    let mut completed = 0_u64;
    let mut sample_index = 0_u64;
    while completed < config.operations {
        completed = (completed + config.emit_every).min(config.operations);
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
                "elapsed_worker_ns": 0_u64,
                "sample_index": sample_index,
            })),
        });
        sample_index += 1;
    }
    frames.push(ProgressFrame::Result {
        lease_id: req.lease_id,
        seq: sample_index,
        emitted_at: completed_at,
        payload: benchmark_result_payload(config, sample_index, started_at, completed_at),
    });
    let body = body_from_frames(&frames)?;
    enforce_benchmark_body_size(&body)?;
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
    started_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
) -> serde_json::Value {
    serde_json::json!({
        "mode": "benchmark",
        "operations_total": config.operations,
        "progress_frames": progress_frames,
        "elapsed_worker_ns": 0_u64,
        "worker_ops_per_second_milli": config.operations * 1000,
        "first_operation_started_at": started_at,
        "completed_at": completed_at,
    })
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

fn enforce_benchmark_body_size(body: &[u8]) -> Result<(), ProtocolError> {
    if body.len() > MAX_BENCHMARK_RESPONSE_BODY_BYTES {
        Err(ProtocolError::InvalidPayload {
            detail: format!(
                "benchmark response body {} > {MAX_BENCHMARK_RESPONSE_BODY_BYTES}",
                body.len()
            ),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "benchmark_worker_test.rs"]
mod tests;
```

- [ ] **Step 2: Add sibling parser and frame tests**

Create `crates/voom-fakes/src/bin/benchmark_worker_test.rs`:

```rust
use super::*;

fn request(lease_id: u64, payload: serde_json::Value) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(lease_id),
        payload,
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    }
}

#[test]
fn missing_mode_defaults_to_baseline_after_path_validation() {
    let parsed = parse_payload(serde_json::json!({"path": "/library/example.mkv"})).unwrap();
    assert_eq!(parsed.mode, BenchmarkMode::Baseline);
    assert_eq!(parsed.path, "/library/example.mkv");
    assert_eq!(parsed.operations, None);
    assert_eq!(parsed.emit_every, None);
}

#[test]
fn missing_path_is_invalid_even_when_mode_is_baseline() {
    let err = parse_payload(serde_json::json!({"mode": "baseline"})).unwrap_err();
    assert!(err.to_string().contains("payload missing path"));
}

#[test]
fn accepts_benchmark_with_valid_operations_and_emit_every() {
    let parsed = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "benchmark",
        "operations": 100,
        "emit_every": 10
    }))
    .unwrap();
    let config = benchmark_config(&parsed).unwrap();
    assert_eq!(config.operations, 100);
    assert_eq!(config.emit_every, 10);
    assert_eq!(config.progress_frames, 10);
}

#[test]
fn missing_emit_every_defaults_to_operations() {
    let parsed = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "benchmark",
        "operations": 25
    }))
    .unwrap();
    let config = benchmark_config(&parsed).unwrap();
    assert_eq!(config.emit_every, 25);
    assert_eq!(config.progress_frames, 1);
}

#[test]
fn rejects_unknown_mode() {
    let err = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "fast"
    }))
    .unwrap_err();
    assert!(err.to_string().contains("unknown benchmark mode"));
}

#[test]
fn rejects_missing_zero_and_excessive_operations() {
    for payload in [
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark"}),
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark", "operations": 0}),
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark", "operations": 10_001}),
    ] {
        let err = parse_payload(payload).unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
    }
}

#[test]
fn rejects_invalid_emit_every() {
    for payload in [
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark", "operations": 10, "emit_every": 0}),
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark", "operations": 10, "emit_every": 11}),
    ] {
        let err = parse_payload(payload).unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
    }
}

#[test]
fn accepts_max_operations_when_progress_frame_count_is_capped() {
    let parsed = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "benchmark",
        "operations": 10_000,
        "emit_every": 100
    }))
    .unwrap();
    assert_eq!(benchmark_config(&parsed).unwrap().progress_frames, 100);
}

#[test]
fn rejects_max_operations_with_one_frame_per_operation() {
    let err = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "benchmark",
        "operations": 10_000,
        "emit_every": 1
    }))
    .unwrap_err();
    assert!(err.to_string().contains("progress_frames"));
}

#[test]
fn baseline_dispatch_emits_progress_and_result() {
    let req = request(7, serde_json::json!({"path": "/library/example.mkv"}));
    let dispatch = baseline_dispatch(&req, "/library/example.mkv").unwrap();
    let body = String::from_utf8(dispatch.body).unwrap();
    assert!(body.contains("\"kind\":\"progress\""));
    assert!(body.contains("\"kind\":\"result\""));
    assert!(body.contains("\"mode\":\"baseline\""));
}

#[test]
fn benchmark_dispatch_emits_cadence_and_final_totals() {
    let req = request(8, serde_json::json!({}));
    let config = BenchmarkConfig {
        path: "/library/example.mkv".to_owned(),
        operations: 25,
        emit_every: 10,
        progress_frames: 3,
    };
    let dispatch = benchmark_dispatch(&req, &config).unwrap();
    let body = String::from_utf8(dispatch.body).unwrap();
    assert_eq!(body.matches("\"kind\":\"progress\"").count(), 3);
    assert!(body.contains("\"operations_total\":25"));
    assert!(body.contains("\"operations_completed\":25"));
    assert!(body.contains("\"progress_frames\":3"));
    assert!(body.contains("\"kind\":\"result\""));
}

#[test]
fn body_size_guard_accepts_at_or_below_limit() {
    let body = vec![b'x'; MAX_BENCHMARK_RESPONSE_BODY_BYTES];
    enforce_benchmark_body_size(&body).unwrap();
}

#[test]
fn body_size_guard_rejects_above_limit() {
    let body = vec![b'x'; MAX_BENCHMARK_RESPONSE_BODY_BYTES + 1];
    let err = enforce_benchmark_body_size(&body).unwrap_err();
    assert!(err.to_string().contains("benchmark response body"));
}
```

- [ ] **Step 3: Run the parser tests and verify they pass**

Run:

```bash
cargo test -p voom-fakes --bin benchmark-worker --all-features
```

Expected: all `benchmark_worker_test.rs` tests pass.

- [ ] **Step 4: Commit parser and frame builders**

```bash
git add crates/voom-fakes/src/bin/benchmark_worker.rs crates/voom-fakes/src/bin/benchmark_worker_test.rs
git commit -m "feat(fakes): add benchmark worker parser"
```

## Task 2: Worker Bootstrap and Baseline Protocol Behavior

**Files:**
- Modify: `crates/voom-fakes/src/bin/benchmark_worker.rs`
- Create: `crates/voom-fakes/tests/benchmark_worker.rs`

- [ ] **Step 1: Write process-backed baseline integration tests**

Create `crates/voom-fakes/tests/benchmark_worker.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    reason = "integration tests use direct process assertions"
)]

use std::process::Stdio;
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, ProtocolError,
    WorkerCredentials,
};

#[tokio::test]
async fn baseline_launches_and_returns_ordered_frames() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(101, serde_json::json!({"path": "/library/example.mkv"}));
    let mut stream = client
        .dispatch(&launch.credentials, "baseline-launch", req)
        .await
        .unwrap();
    let first = stream.frames.next_frame().await.unwrap();
    assert!(matches!(first, NdjsonOutcome::Frame(_)));
    let terminal = stream.frames.next_frame().await.unwrap();
    assert!(matches!(terminal, NdjsonOutcome::Terminated(_)));
    launch.shutdown().await;
}

#[tokio::test]
async fn invalid_payload_returns_protocol_error_and_worker_stays_alive() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(102, serde_json::json!({}));
    let err = client
        .dispatch(&launch.credentials, "invalid-payload", req)
        .await
        .unwrap_err();
    assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
    assert!(launch.child.try_wait().unwrap().is_none());
    launch.shutdown().await;
}

fn operation_request(lease_id: u64, payload: serde_json::Value) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(lease_id),
        payload,
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    }
}

struct TestLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
    bound: std::net::SocketAddr,
    credentials: WorkerCredentials,
}

impl TestLaunch {
    async fn shutdown(&mut self) {
        drop(self.stdin.take());
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "status={status}");
    }
}

async fn spawn_benchmark() -> TestLaunch {
    let worker_id = voom_core::WorkerId(1);
    let worker_epoch = 0;
    let secret = "phase1-bootstrap-secret";
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_benchmark-worker"))
        .env("VOOM_WORKER_SECRET", secret)
        .env("VOOM_WORKER_ID", worker_id.0.to_string())
        .env("VOOM_WORKER_EPOCH", worker_epoch.to_string())
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let bound = line
        .strip_prefix("BOUND addr=")
        .unwrap()
        .parse::<std::net::SocketAddr>()
        .unwrap();
    TestLaunch {
        child,
        stdin,
        bound,
        credentials: WorkerCredentials {
            worker_id,
            worker_epoch,
            secret: SecretString::from(secret),
        },
    }
}
```

- [ ] **Step 2: Run the new integration tests to verify the scaffold fails**

Run:

```bash
cargo test -p voom-fakes --test benchmark_worker --all-features
```

Expected: fail because the scaffold does not print `BOUND addr=...` or serve `/v1/operations`.

- [ ] **Step 3: Implement bootstrap and operation handler**

Update imports and `main` in `crates/voom-fakes/src/bin/benchmark_worker.rs`:

```rust
use std::net::SocketAddr;
use std::sync::Arc;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, BufReader};
use voom_worker_protocol::{
    HttpServer, OperationFuture, ServerHandle, WorkerCredentials,
};
```

Replace `fn main() {}` with:

```rust
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
```

- [ ] **Step 4: Run baseline integration and bin tests**

Run:

```bash
cargo test -p voom-fakes --bin benchmark-worker --all-features
cargo test -p voom-fakes --test benchmark_worker --all-features
```

Expected: both pass.

- [ ] **Step 5: Commit baseline worker**

```bash
git add crates/voom-fakes/src/bin/benchmark_worker.rs crates/voom-fakes/tests/benchmark_worker.rs
git commit -m "feat(fakes): implement benchmark worker baseline"
```

## Task 3: Conformance Promotion

**Files:**
- Modify: `crates/voom-conformance/voom-fakes-manifest.toml`
- Modify: `crates/voom-conformance/tests/conformance_all.rs`
- Modify: `crates/voom-conformance/src/manifest_test.rs`

- [ ] **Step 1: Add failing manifest/conformance expectations**

In `crates/voom-conformance/tests/conformance_all.rs`, add these assertions beside the existing `echo-worker` and `chaos-worker` assertions:

```rust
assert!(
    manifest
        .active
        .iter()
        .any(|entry| entry.name == "benchmark-worker")
);
assert!(!manifest.scaffold.iter().any(|s| s == "benchmark-worker"));
```

In `crates/voom-conformance/src/manifest_test.rs`, update `VALID` to model `benchmark-worker` as active:

```toml
[[binaries]]
name = "benchmark-worker"
target = "benchmark-worker"
status = "active"
required = true

[scaffold]
binaries = ["chaos-worker"]
```

Update `parses_active_and_scaffold_entries` to assert both active names:

```rust
assert_eq!(
    manifest
        .active
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>(),
    vec!["echo-worker", "benchmark-worker"]
);
assert_eq!(manifest.scaffold, vec!["chaos-worker"]);
```

- [ ] **Step 2: Run manifest/conformance tests to verify promotion is not done**

Run:

```bash
cargo test -p voom-conformance manifest --all-features
cargo test -p voom-conformance --test conformance_all --all-features
```

Expected: `manifest` tests pass after local test fixture update; `conformance_all` fails because the real manifest still lists `benchmark-worker` under scaffold.

- [ ] **Step 3: Promote benchmark-worker in the real manifest**

In `crates/voom-conformance/voom-fakes-manifest.toml`, add:

```toml
[[binaries]]
name = "benchmark-worker"
target = "benchmark-worker"
purpose = "phase 5 benchmark worker - baseline conformance and bounded worker-level metric frames"
status = "active"
required = true
```

Remove `"benchmark-worker"` from `[scaffold].binaries`.

- [ ] **Step 4: Build benchmark-worker and run conformance**

Run:

```bash
cargo build -p voom-fakes --bin benchmark-worker
cargo test -p voom-conformance --all-features
```

Expected: conformance launches `echo-worker`, `chaos-worker`, and `benchmark-worker`; all suites pass.

- [ ] **Step 5: Commit promotion**

```bash
git add crates/voom-conformance/voom-fakes-manifest.toml crates/voom-conformance/tests/conformance_all.rs crates/voom-conformance/src/manifest_test.rs
git commit -m "test(conformance): promote benchmark worker"
```

## Task 4: Benchmark Mode Integration and Idempotency Tests

**Files:**
- Modify: `crates/voom-fakes/tests/benchmark_worker.rs`
- Modify: `crates/voom-fakes/src/bin/benchmark_worker.rs`

- [ ] **Step 1: Add process-backed benchmark and idempotency tests**

Append these tests to `crates/voom-fakes/tests/benchmark_worker.rs`:

```rust
#[tokio::test]
async fn benchmark_mode_returns_cadence_progress_and_summary() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(
        201,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 25,
            "emit_every": 10
        }),
    );
    let mut stream = client
        .dispatch(&launch.credentials, "benchmark-mode", req)
        .await
        .unwrap();
    let mut progress = 0;
    loop {
        match stream.frames.next_frame().await.unwrap() {
            NdjsonOutcome::Frame(frame) => {
                progress += 1;
                let ProgressFrame::Progress { payload, .. } = frame else {
                    panic!("expected progress frame");
                };
                let payload = payload.unwrap();
                assert_eq!(payload["mode"], "benchmark");
                assert_eq!(payload["operations_total"], 25);
            }
            NdjsonOutcome::Terminated(frame) => {
                let ProgressFrame::Result { payload, .. } = frame else {
                    panic!("expected result frame");
                };
                assert_eq!(progress, 3);
                assert_eq!(payload["mode"], "benchmark");
                assert_eq!(payload["operations_total"], 25);
                assert_eq!(payload["progress_frames"], 3);
                assert!(payload["worker_ops_per_second_milli"].as_u64().unwrap() > 0);
                break;
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    launch.shutdown().await;
}

#[tokio::test]
async fn idempotent_replay_returns_same_benchmark_response() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(
        202,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 10,
            "emit_every": 5
        }),
    );
    let first = collect_body(
        client
            .dispatch(&launch.credentials, "benchmark-replay", req.clone())
            .await
            .unwrap(),
    )
    .await;
    let second = collect_body(
        client
            .dispatch(&launch.credentials, "benchmark-replay", req)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(first, second);
    launch.shutdown().await;
}

#[tokio::test]
async fn same_idempotency_key_with_different_benchmark_body_is_rejected() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let first = operation_request(
        203,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 10,
            "emit_every": 5
        }),
    );
    let second = operation_request(
        203,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 10,
            "emit_every": 10
        }),
    );
    let _ = client
        .dispatch(&launch.credentials, "benchmark-conflict", first)
        .await
        .unwrap();
    let err = client
        .dispatch(&launch.credentials, "benchmark-conflict", second)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::DuplicateIdempotencyKey { .. }
    ));
    launch.shutdown().await;
}

#[tokio::test]
async fn too_many_progress_frames_is_rejected_without_crashing() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(
        204,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 10_000,
            "emit_every": 1
        }),
    );
    let err = client
        .dispatch(&launch.credentials, "benchmark-too-many-frames", req)
        .await
        .unwrap_err();
    assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
    assert!(launch.child.try_wait().unwrap().is_none());
    launch.shutdown().await;
}

async fn collect_body(mut stream: voom_worker_protocol::DispatchStream) -> Vec<ProgressFrame> {
    let mut frames = Vec::new();
    loop {
        match stream.frames.next_frame().await.unwrap() {
            NdjsonOutcome::Frame(frame) => frames.push(frame),
            NdjsonOutcome::Terminated(frame) => {
                frames.push(frame);
                return frames;
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }
}
```

Also add `ProgressFrame` to the existing `use voom_worker_protocol::{ ... }` import list.

- [ ] **Step 2: Run benchmark integration tests**

Run:

```bash
cargo test -p voom-fakes --test benchmark_worker --all-features
```

Expected: tests pass, including exact equality for the repeated benchmark response.

- [ ] **Step 3: Run focused bin tests**

Run:

```bash
cargo test -p voom-fakes --bin benchmark-worker --all-features
```

Expected: sibling tests pass.

- [ ] **Step 4: Commit benchmark mode tests and any implementation fixes**

```bash
git add crates/voom-fakes/src/bin/benchmark_worker.rs crates/voom-fakes/tests/benchmark_worker.rs
git commit -m "feat(fakes): add benchmark worker metrics"
```

## Task 5: Final Verification and Cleanup

**Files:**
- Modify only files touched by Tasks 1-4 if formatting or clippy requires it.

- [ ] **Step 1: Run formatter check**

Run:

```bash
cargo fmt --all -- --check
```

Expected: pass. If it fails, run `cargo fmt --all`, inspect `git diff`, and include formatting changes in the final cleanup commit.

- [ ] **Step 2: Run full fake-worker verification**

Run:

```bash
cargo test -p voom-fakes --all-features
```

Expected: all `voom-fakes` tests pass, including `chaos_worker` and `benchmark_worker`.

- [ ] **Step 3: Run conformance verification**

Run:

```bash
cargo build -p voom-fakes --bin benchmark-worker
cargo test -p voom-conformance --all-features
```

Expected: conformance launches all active entries: `echo-worker`, `chaos-worker`, and `benchmark-worker`.

- [ ] **Step 4: Run workspace CI**

Run:

```bash
just ci
```

Expected: CI ends with `==> All CI checks passed`.

- [ ] **Step 5: Commit cleanup if needed**

If any formatter, clippy, or final verification fixes were needed:

```bash
git add crates/voom-fakes/src/bin/benchmark_worker.rs crates/voom-fakes/src/bin/benchmark_worker_test.rs crates/voom-fakes/tests/benchmark_worker.rs crates/voom-conformance/voom-fakes-manifest.toml crates/voom-conformance/tests/conformance_all.rs crates/voom-conformance/src/manifest_test.rs
git commit -m "fix(fakes): satisfy benchmark worker verification"
```

If no files changed after Task 4, do not create an empty commit.
