# Sprint 2 Phase 4 Chaos Worker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote `chaos-worker` from scaffold to active conformance target and add deterministic worker-boundary fault modes.

**Architecture:** Implement `chaos-worker` as a local HTTP/1.1 worker with a chaos-owned compatibility shim for handshake, auth/version/idempotency, response framing, and streaming fault responses. Keep `voom-worker-protocol` public API unchanged; conformance remains the compatibility oracle.

**Tech Stack:** Rust 2024, Tokio TCP I/O, `voom-worker-protocol` wire types, `serde_json`, `chrono`, `secrecy`, `blake3`, and conformance harness integration tests.

---

## File Structure

- Modify `crates/voom-fakes/Cargo.toml`: add direct deps needed by the local shim: `blake3.workspace = true`, `bytes.workspace = true`, `secrecy.workspace = true`.
- Modify `crates/voom-fakes/src/bin/chaos_worker.rs`: replace scaffold with bootstrap, local HTTP shim, parser, frame builders, idempotency cache, and sibling tests.
- Create `crates/voom-fakes/src/bin/chaos_worker_test.rs`: sibling unit tests for parser, response bytes, idempotency cache, and malformed body.
- Create `crates/voom-fakes/tests/chaos_worker.rs`: process-backed integration tests for launch, baseline, crash, stall, malformed, non-converging, deadline, and invalid payload.
- Modify `crates/voom-conformance/src/manifest.rs`: add optional `target_dir_fallback` resolution for active cross-package binaries when `CARGO_BIN_EXE_<target>` is unavailable.
- Modify `crates/voom-conformance/src/manifest_test.rs`: cover explicit path precedence and target-dir fallback path construction.
- Modify `crates/voom-conformance/tests/conformance_all.rs`: remove echo-only assumptions and run `stdin_eof_terminates_worker` against every active manifest entry.
- Modify `crates/voom-conformance/voom-fakes-manifest.toml`: promote `chaos-worker` to active and remove it from scaffold.

## Task 1: Chaos Payload Parser and Buffered Frames

**Files:**
- Modify: `crates/voom-fakes/Cargo.toml`
- Modify: `crates/voom-fakes/src/bin/chaos_worker.rs`
- Create: `crates/voom-fakes/src/bin/chaos_worker_test.rs`

- [ ] **Step 1: Add direct dependencies**

Add these dependencies to `crates/voom-fakes/Cargo.toml`:

```toml
blake3.workspace = true
bytes.workspace = true
secrecy.workspace = true
```

- [ ] **Step 2: Replace the scaffold with parser/data types and sibling tests hook**

Replace the scaffold file with an intermediate parser-only binary that has a no-op `main`, plus private types:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "chaos-worker tests use direct fixture assertions"
    )
)]
#![expect(
    clippy::print_stdout,
    reason = "chaos-worker advertises readiness with BOUND addr=..."
)]
#![expect(
    clippy::exit,
    reason = "chaos-worker crash mode intentionally terminates the worker process"
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
```

- [ ] **Step 3: Add parser and frame sibling tests**

Create `crates/voom-fakes/src/bin/chaos_worker_test.rs`:

```rust
use super::*;

#[test]
fn missing_mode_defaults_to_baseline_after_path_validation() {
    let parsed = parse_payload(serde_json::json!({"path": "/library/example.mkv"})).unwrap();
    assert_eq!(parsed.mode, ChaosMode::Baseline);
    assert_eq!(parsed.path, "/library/example.mkv");
}

#[test]
fn missing_path_is_invalid_even_when_mode_is_baseline() {
    let err = parse_payload(serde_json::json!({"mode": "baseline"})).unwrap_err();
    assert!(err.to_string().contains("payload missing path"));
}

#[test]
fn accepts_each_known_mode() {
    for mode in [
        "baseline",
        "crash",
        "stall",
        "malformed_result",
        "non_converging_progress",
        "deadline_exceeded",
    ] {
        let parsed =
            parse_payload(serde_json::json!({"path": "/library/example.mkv", "mode": mode}))
                .unwrap();
        assert_eq!(parsed.path, "/library/example.mkv");
    }
}

#[test]
fn rejects_unknown_mode() {
    let err = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "unknown"
    }))
    .unwrap_err();
    assert!(err.to_string().contains("unknown chaos mode"));
}

#[test]
fn rejects_excessive_timing_values() {
    let err = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "stall_ms": 30001
    }))
    .unwrap_err();
    assert!(err.to_string().contains("stall_ms"));
}

#[test]
fn baseline_body_has_progress_then_result() {
    let req = OperationRequest {
        operation: voom_worker_protocol::OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(42),
        payload: serde_json::json!({"path": "/library/example.mkv"}),
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    };
    let payload = parse_payload(req.payload.clone()).unwrap();
    let body = baseline_body(&req, &payload).unwrap();
    let lines = std::str::from_utf8(&body).unwrap().lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"kind\":\"progress\""));
    assert!(lines[0].contains("\"seq\":0"));
    assert!(lines[1].contains("\"kind\":\"result\""));
    assert!(lines[1].contains("\"seq\":1"));
}

#[test]
fn malformed_body_is_not_valid_progress_json() {
    assert!(serde_json::from_slice::<serde_json::Value>(&malformed_body()).is_err());
}
```

- [ ] **Step 4: Run parser tests and verify they pass**

Run:

```bash
cargo test -p voom-fakes chaos_worker --all-features
```

Expected: parser/frame tests pass. The no-op `main` is replaced by the worker bootstrap in Task 2.

- [ ] **Step 5: Commit parser slice**

```bash
git add crates/voom-fakes/Cargo.toml crates/voom-fakes/src/bin/chaos_worker.rs crates/voom-fakes/src/bin/chaos_worker_test.rs Cargo.lock
git commit -m "feat(fakes): add chaos payload parser"
```

## Task 2: Local HTTP Shim and Baseline Worker

**Files:**
- Modify: `crates/voom-fakes/src/bin/chaos_worker.rs`
- Create: `crates/voom-fakes/tests/chaos_worker.rs`

- [ ] **Step 1: Add failing launch/baseline integration tests**

Create `crates/voom-fakes/tests/chaos_worker.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    reason = "integration tests use direct process assertions"
)]

use std::process::Stdio;
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use voom_worker_protocol::{ClientHandle, HttpClient, OperationKind, OperationRequest, WorkerCredentials};

#[tokio::test]
async fn baseline_launches_and_returns_ordered_frames() {
    let mut launch = spawn_chaos().await;
    let client = HttpClient::new(launch.bound);
    let req = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(101),
        payload: serde_json::json!({"path": "/library/example.mkv"}),
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    };
    let mut stream = client
        .dispatch(&launch.credentials, "baseline-launch", req)
        .await
        .unwrap();
    let first = stream.frames.next_frame().await.unwrap();
    assert!(matches!(first, voom_worker_protocol::NdjsonOutcome::Frame(_)));
    let terminal = stream.frames.next_frame().await.unwrap();
    assert!(matches!(
        terminal,
        voom_worker_protocol::NdjsonOutcome::Terminated(_)
    ));
    launch.shutdown().await;
}

#[tokio::test]
async fn invalid_payload_returns_protocol_error_and_worker_stays_alive() {
    let mut launch = spawn_chaos().await;
    let client = HttpClient::new(launch.bound);
    let req = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(102),
        payload: serde_json::json!({}),
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    };
    let err = client
        .dispatch(&launch.credentials, "invalid-payload", req)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::InvalidPayload { .. }
    ));
    assert!(launch.child.try_wait().unwrap().is_none());
    launch.shutdown().await;
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

async fn spawn_chaos() -> TestLaunch {
    let worker_id = voom_core::WorkerId(1);
    let worker_epoch = 0;
    let secret = "phase1-bootstrap-secret";
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_chaos-worker"))
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

Run:

```bash
cargo test -p voom-fakes baseline_launches_and_returns_ordered_frames invalid_payload_returns_protocol_error_and_worker_stays_alive --all-features
```

Expected: fails because `chaos-worker` still does not start a server.

- [ ] **Step 2: Implement worker bootstrap and HTTP loop**

First expand the imports at the top of `chaos_worker.rs`:

```rust
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use voom_worker_protocol::{HandshakeRequest, OperationKind, OperationResponse, WorkerCredentials};
```

Add local compatibility-shim constants below `MAX_DURATION_MS`:

```rust
const PROTOCOL_VERSION_HEADER: &str = "x-voom-protocol-version";
const WORKER_ID_HEADER: &str = "x-voom-worker-id";
const WORKER_EPOCH_HEADER: &str = "x-voom-worker-epoch";
const IDEMPOTENCY_KEY_HEADER: &str = "x-voom-idempotency-key";
const MAX_BODY_BYTES: usize = 1 << 20;
const IDEMPOTENCY_CACHE_CAPACITY: usize = 1024;
```

Replace the no-op `main` with:

```rust
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let credentials = load_credentials()?;
    let bind: SocketAddr = std::env::var("VOOM_WORKER_BIND")
        .unwrap_or_else(|_| "127.0.0.1:0".to_owned())
        .parse()
        .map_err(|e| format!("VOOM_WORKER_BIND parse failed: {e}"))?;
    let listener = TcpListener::bind(bind).await?;
    println!("BOUND addr={}", listener.local_addr()?);
    let cache = std::sync::Arc::new(tokio::sync::Mutex::new(IdempotencyCache::new(
        IDEMPOTENCY_CACHE_CAPACITY,
    )));
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let watchdog = tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(_)) = stdin.next_line().await {}
        let _ = shutdown_tx.send(());
    });
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let credentials = credentials.clone();
                let cache = cache.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, credentials, cache).await;
                });
            }
        }
    }
    let _ = watchdog.await;
    Ok(())
}
```

Add helpers:

```rust
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

async fn handle_connection(
    mut stream: TcpStream,
    credentials: WorkerCredentials,
    cache: std::sync::Arc<tokio::sync::Mutex<IdempotencyCache>>,
) -> Result<(), ProtocolError> {
    let request = read_http_request(&mut stream).await?;
    let response = route_request(&request, &credentials, cache).await?;
    write_response(&mut stream, response).await
}
```

- [ ] **Step 3: Implement request parsing, routing, and responses**

Add these private types/functions:

```rust
#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Debug)]
enum ChaosResponse {
    Fixed {
        status: &'static str,
        content_type: &'static str,
        body: Vec<u8>,
    },
    Open {
        prefix: Vec<u8>,
        chunks: Vec<(Vec<u8>, Duration)>,
        hold: Duration,
    },
    ExitProcess(i32),
}

async fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, ProtocolError> {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 1024];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut tmp).await.map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("read headers: {e}"),
        })?;
        if n == 0 {
            return Err(ProtocolError::InvalidPayload {
                detail: "connection closed before headers".to_owned(),
            });
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_BODY_BYTES {
            return Err(ProtocolError::FrameTooLarge {
                bytes: buf.len() as u64,
                max: MAX_BODY_BYTES as u64,
            });
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.lines();
    let request_line = lines.next().ok_or_else(|| ProtocolError::InvalidPayload {
        detail: "missing request line".to_owned(),
    })?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<Vec<_>>();
    let content_length = header(&headers, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            bytes: content_length as u64,
            max: MAX_BODY_BYTES as u64,
        });
    }
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp).await.map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("read body: {e}"),
        })?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: buf[header_end..buf.len().min(header_end + content_length)].to_vec(),
    })
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

async fn route_request(
    req: &HttpRequest,
    credentials: &WorkerCredentials,
    cache: std::sync::Arc<tokio::sync::Mutex<IdempotencyCache>>,
) -> Result<ChaosResponse, ProtocolError> {
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/v1/handshake") => Ok(handle_handshake(&req.body)),
        ("POST", "/v1/operations") => handle_operation(req, credentials, cache).await,
        _ => Ok(ChaosResponse::Fixed {
            status: "404 Not Found",
            content_type: "text/plain",
            body: b"not found".to_vec(),
        }),
    }
}
```

- [ ] **Step 4: Implement handshake, operation validation, baseline response, and write path**

Add:

```rust
fn handle_handshake(body: &[u8]) -> ChaosResponse {
    let parsed = serde_json::from_slice::<HandshakeRequest>(body).map_err(|e| {
        ProtocolError::InvalidPayload {
            detail: format!("json decode: {e}"),
        }
    });
    match parsed.and_then(|req| voom_worker_protocol::negotiate(req.offered)) {
        Ok(resp) => json_response("200 OK", &resp),
        Err(err) => json_response("400 Bad Request", &err),
    }
}

async fn handle_operation(
    http: &HttpRequest,
    credentials: &WorkerCredentials,
    cache: std::sync::Arc<tokio::sync::Mutex<IdempotencyCache>>,
) -> Result<ChaosResponse, ProtocolError> {
    if let Err(err) = enforce_version(&http.headers) {
        return Ok(json_response("400 Bad Request", &err));
    }
    if let Err(err) = enforce_credentials(&http.headers, credentials) {
        return Ok(json_response("401 Unauthorized", &err));
    }
    let Some(idempotency_key) = header(&http.headers, IDEMPOTENCY_KEY_HEADER).map(str::to_owned)
    else {
        return Ok(json_response(
            "400 Bad Request",
            &ProtocolError::InvalidPayload {
                detail: format!("missing {IDEMPOTENCY_KEY_HEADER}"),
            },
        ));
    };
    if let Err(err) = reject_body_idempotency_key(&http.body) {
        return Ok(json_response("400 Bad Request", &err));
    }
    let body_hash = *blake3::hash(&http.body).as_bytes();
    if let Some(cached) = cache.lock().await.lookup(&idempotency_key, body_hash) {
        return Ok(cached);
    }
    if cache.lock().await.is_conflict(&idempotency_key, body_hash) {
        return Ok(json_response(
            "400 Bad Request",
            &ProtocolError::DuplicateIdempotencyKey {
                key: idempotency_key,
                original_status: "completed".to_owned(),
            },
        ));
    }
    let request = match serde_json::from_slice::<OperationRequest>(&http.body) {
        Ok(request) => request,
        Err(e) => {
            return Ok(json_response(
                "400 Bad Request",
                &ProtocolError::InvalidPayload {
                    detail: format!("json decode: {e}"),
                },
            ));
        }
    };
    let response = dispatch_operation(&request)?;
    if matches!(response, ChaosResponse::Fixed { .. }) {
        cache
            .lock()
            .await
            .record(idempotency_key, body_hash, response.clone());
    }
    Ok(response)
}

fn dispatch_operation(req: &OperationRequest) -> Result<ChaosResponse, ProtocolError> {
    if req.operation != OperationKind::ProbeFile {
        return Ok(json_response(
            "400 Bad Request",
            &ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            },
        ));
    }
    let payload = match parse_payload(req.payload.clone()) {
        Ok(payload) => payload,
        Err(err) => return Ok(json_response("400 Bad Request", &err)),
    };
    match payload.mode {
        ChaosMode::Baseline => fixed_operation_response(req, baseline_body(req, &payload)?),
        mode => Ok(streaming_or_fault_response(req, &payload, mode)?),
    }
}
```

Also add `Clone` for `ChaosResponse`, `IdempotencyCache`, `enforce_version`, `enforce_credentials`, `reject_body_idempotency_key`, `json_response`, `fixed_operation_response`, and `write_response`:

```rust
impl Clone for ChaosResponse {
    fn clone(&self) -> Self {
        match self {
            Self::Fixed { status, content_type, body } => Self::Fixed {
                status: *status,
                content_type: *content_type,
                body: body.clone(),
            },
            Self::Open { prefix, chunks, hold } => Self::Open {
                prefix: prefix.clone(),
                chunks: chunks.clone(),
                hold: *hold,
            },
            Self::ExitProcess(code) => Self::ExitProcess(*code),
        }
    }
}

#[derive(Debug)]
struct CacheEntry {
    hash: [u8; 32],
    response: ChaosResponse,
}

#[derive(Debug)]
struct IdempotencyCache {
    capacity: usize,
    order: VecDeque<String>,
    entries: HashMap<String, CacheEntry>,
}

impl IdempotencyCache {
    fn new(capacity: usize) -> Self {
        Self { capacity, order: VecDeque::new(), entries: HashMap::new() }
    }

    fn lookup(&self, key: &str, hash: [u8; 32]) -> Option<ChaosResponse> {
        self.entries
            .get(key)
            .filter(|entry| entry.hash == hash)
            .map(|entry| entry.response.clone())
    }

    fn is_conflict(&self, key: &str, hash: [u8; 32]) -> bool {
        self.entries.get(key).is_some_and(|entry| entry.hash != hash)
    }

    fn record(&mut self, key: String, hash: [u8; 32], response: ChaosResponse) {
        if self.capacity == 0 || self.entries.contains_key(&key) {
            return;
        }
        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, CacheEntry { hash, response });
    }
}
```

```rust
fn enforce_version(headers: &[(String, String)]) -> Result<(), ProtocolError> {
    let offered = header(headers, PROTOCOL_VERSION_HEADER)
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| ProtocolError::InvalidPayload {
            detail: format!("missing {PROTOCOL_VERSION_HEADER}"),
        })?;
    if offered == voom_core::PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::UnsupportedProtocolVersion {
            offered,
            supported_min: voom_core::PROTOCOL_VERSION_SUPPORTED_MIN,
            supported_max: voom_core::PROTOCOL_VERSION_SUPPORTED_MAX,
        })
    }
}

fn enforce_credentials(
    headers: &[(String, String)],
    credentials: &WorkerCredentials,
) -> Result<(), ProtocolError> {
    let bearer = header(headers, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(ProtocolError::UnauthorizedBearer)?
        .to_owned();
    let worker_id = header(headers, WORKER_ID_HEADER)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(ProtocolError::UnauthorizedBearer)?;
    let worker_epoch = header(headers, WORKER_EPOCH_HEADER)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(ProtocolError::UnauthorizedBearer)?;
    voom_worker_protocol::validate_credentials(
        &voom_worker_protocol::PresentedCredentials {
            worker_id: voom_core::WorkerId(worker_id),
            worker_epoch,
            secret: SecretString::from(bearer),
        },
        credentials,
    )
}

fn reject_body_idempotency_key(body: &[u8]) -> Result<(), ProtocolError> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("json decode: {e}"),
        })?;
    if contains_idempotency_key(&value) {
        Err(ProtocolError::HeaderBodyKeyMismatch)
    } else {
        Ok(())
    }
}

fn contains_idempotency_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(k, v)| k == "idempotency_key" || contains_idempotency_key(v)),
        serde_json::Value::Array(values) => values.iter().any(contains_idempotency_key),
        _ => false,
    }
}

fn json_response<T: serde::Serialize>(status: &'static str, value: &T) -> ChaosResponse {
    let body = serde_json::to_vec(value).unwrap_or_default();
    ChaosResponse::Fixed {
        status,
        content_type: "application/json",
        body,
    }
}

fn fixed_operation_response(
    req: &OperationRequest,
    body: Vec<u8>,
) -> Result<ChaosResponse, ProtocolError> {
    let mut framed = operation_response_line(req)?;
    framed.extend_from_slice(&body);
    Ok(ChaosResponse::Fixed {
        status: "200 OK",
        content_type: "application/x-ndjson",
        body: framed,
    })
}

fn operation_response_line(req: &OperationRequest) -> Result<Vec<u8>, ProtocolError> {
    let response = OperationResponse {
        lease_id: req.lease_id,
        accepted_at: Utc::now(),
    };
    let mut out = serde_json::to_vec(&response).map_err(|e| ProtocolError::InvalidPayload {
        detail: format!("response encode: {e}"),
    })?;
    out.push(b'\n');
    Ok(out)
}

async fn write_response(stream: &mut TcpStream, response: ChaosResponse) -> Result<(), ProtocolError> {
    match response {
        ChaosResponse::Fixed { status, content_type, body } => {
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.map_err(write_err)?;
            stream.write_all(&body).await.map_err(write_err)?;
        }
        ChaosResponse::Open { prefix, chunks, hold } => {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: keep-alive\r\n\r\n")
                .await
                .map_err(write_err)?;
            stream.write_all(&prefix).await.map_err(write_err)?;
            for (chunk, delay) in chunks {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                stream.write_all(&chunk).await.map_err(write_err)?;
            }
            tokio::time::sleep(hold).await;
        }
        ChaosResponse::ExitProcess(code) => std::process::exit(code),
    }
    Ok(())
}

fn write_err(e: std::io::Error) -> ProtocolError {
    ProtocolError::MalformedFrame {
        detail: format!("write: {e}"),
    }
}
```

- [ ] **Step 5: Add the intermediate non-baseline mode response**

Add an intermediate implementation for `streaming_or_fault_response` that compiles and returns `InvalidPayload` until Task 4 replaces it with process-backed fault behavior:

```rust
fn streaming_or_fault_response(
    _req: &OperationRequest,
    _payload: &ChaosPayload,
    mode: ChaosMode,
) -> Result<ChaosResponse, ProtocolError> {
    Ok(json_response(
        "400 Bad Request",
        &ProtocolError::InvalidPayload {
            detail: format!("mode {mode:?} is reserved for Task 4"),
        },
    ))
}
```

- [ ] **Step 6: Run baseline tests**

Run:

```bash
cargo test -p voom-fakes baseline_launches_and_returns_ordered_frames invalid_payload_returns_protocol_error_and_worker_stays_alive --all-features
```

Expected: both tests pass.

- [ ] **Step 7: Commit baseline worker**

```bash
git add crates/voom-fakes/src/bin/chaos_worker.rs crates/voom-fakes/tests/chaos_worker.rs
git commit -m "feat(fakes): implement chaos worker baseline"
```

## Task 3: Promote Chaos Worker to Active Conformance

**Files:**
- Modify: `crates/voom-conformance/src/manifest.rs`
- Modify: `crates/voom-conformance/src/manifest_test.rs`
- Modify: `crates/voom-conformance/tests/conformance_all.rs`
- Modify: `crates/voom-conformance/voom-fakes-manifest.toml`

- [ ] **Step 1: Add failing manifest fallback tests**

Add to `crates/voom-conformance/src/manifest_test.rs`:

```rust
#[test]
fn explicit_path_takes_precedence_over_target_dir_fallback() {
    let entry = ActiveBinary {
        name: "chaos-worker".to_owned(),
        target: "chaos-worker".to_owned(),
        status: "active".to_owned(),
        required: true,
        path: Some(std::path::PathBuf::from("/explicit/chaos-worker")),
    };
    let path = resolve_active_with_sources(&entry, |_| None, Some(std::path::Path::new("/tmp/target")))
        .unwrap();
    assert_eq!(path, std::path::PathBuf::from("/explicit/chaos-worker"));
}

#[test]
fn resolves_cross_package_binary_from_target_dir_fallback() {
    let entry = ActiveBinary {
        name: "chaos-worker".to_owned(),
        target: "chaos-worker".to_owned(),
        status: "active".to_owned(),
        required: true,
        path: None,
    };
    let path = resolve_active_with_sources(&entry, |_| None, Some(std::path::Path::new("/tmp/target")))
        .unwrap();
    assert_eq!(path, std::path::PathBuf::from("/tmp/target/debug/chaos-worker"));
}

#[test]
fn default_target_dir_fallback_points_at_workspace_target_dir() {
    let dir = default_target_dir();
    assert!(dir.ends_with("target"), "{dir:?}");
}
```

Run:

```bash
cargo test -p voom-conformance manifest --all-features
```

Expected: compile failure because `resolve_active_with_sources` does not exist.

- [ ] **Step 2: Implement target-dir fallback resolver**

In `crates/voom-conformance/src/manifest.rs`, change `resolve_active` and add `resolve_active_with_sources`:

```rust
pub fn resolve_active(entry: &ActiveBinary) -> Result<PathBuf, ManifestError> {
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_target_dir);
    resolve_active_with_sources(entry, |key| std::env::var_os(key), Some(target_dir.as_path()))
}

pub fn resolve_active_with_sources<F>(
    entry: &ActiveBinary,
    env: F,
    target_dir: Option<&Path>,
) -> Result<PathBuf, ManifestError>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    if let Some(path) = &entry.path {
        return Ok(path.clone());
    }
    let env_key = format!("CARGO_BIN_EXE_{}", entry.target);
    if let Some(path) = env(&env_key) {
        return Ok(PathBuf::from(path));
    }
    if let Some(target_dir) = target_dir {
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        return Ok(target_dir.join("debug").join(format!("{}{}", entry.target, suffix)));
    }
    Err(ManifestError::MissingActiveBinary {
        name: entry.name.clone(),
        env_key,
    })
}
```

Update the existing `resolve_active_with` to delegate:

```rust
pub fn resolve_active_with<F>(entry: &ActiveBinary, env: F) -> Result<PathBuf, ManifestError>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    resolve_active_with_sources(entry, env, None)
}

fn default_target_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(|workspace| workspace.join("target"))
        .unwrap_or_else(|| PathBuf::from("target"))
}
```

- [ ] **Step 3: Run manifest tests**

Run:

```bash
cargo test -p voom-conformance manifest --all-features
```

Expected: manifest tests pass.

- [ ] **Step 4: Promote chaos-worker in manifest**

Edit `crates/voom-conformance/voom-fakes-manifest.toml`:

```toml
[[binaries]]
name = "chaos-worker"
target = "chaos-worker"
purpose = "phase 4 chaos worker - baseline conformance and process-backed fault modes"
status = "active"
required = true
```

Remove `"chaos-worker"` from `[scaffold].binaries`.

- [ ] **Step 5: Update conformance integration assumptions**

In `crates/voom-conformance/tests/conformance_all.rs`, remove:

```rust
assert_eq!(manifest.active.len(), 1);
assert!(manifest.scaffold.iter().any(|s| s == "chaos-worker"));
```

Replace with:

```rust
assert!(manifest.active.iter().any(|entry| entry.name == "echo-worker"));
assert!(manifest.active.iter().any(|entry| entry.name == "chaos-worker"));
assert!(!manifest.scaffold.iter().any(|s| s == "chaos-worker"));
```

Change `stdin_eof_terminates_worker()` so it iterates every active entry and records `<name>::stdin_eof_terminates_worker` instead of only checking `echo-worker`.

- [ ] **Step 6: Build cross-package binary and run conformance**

Run:

```bash
cargo build -p voom-fakes --bin chaos-worker
cargo test -p voom-conformance --all-features
```

Expected: conformance passes for `echo-worker` and `chaos-worker`.

- [ ] **Step 7: Commit conformance promotion**

```bash
git add crates/voom-conformance/src/manifest.rs crates/voom-conformance/src/manifest_test.rs crates/voom-conformance/tests/conformance_all.rs crates/voom-conformance/voom-fakes-manifest.toml
git commit -m "test(conformance): promote chaos worker"
```

## Task 4: Process-Backed Fault Modes

**Files:**
- Modify: `crates/voom-fakes/src/bin/chaos_worker.rs`
- Modify: `crates/voom-fakes/tests/chaos_worker.rs`

- [ ] **Step 1: Add failing fault-mode integration tests**

First, update the integration-test import in `crates/voom-fakes/tests/chaos_worker.rs`:

```rust
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
```

Then append tests and raw helpers:

```rust
#[tokio::test]
async fn crash_mode_exits_non_zero() {
    let mut launch = spawn_chaos().await;
    let body = operation_body(201, serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "crash"
    }));
    let _ = raw_request(launch.bound, body, "crash-mode").await;
    let status = tokio::time::timeout(Duration::from_secs(5), launch.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
}

#[tokio::test]
async fn malformed_result_is_rejected_by_reader() {
    let mut launch = spawn_chaos().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(202, serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "malformed_result"
    }));
    let mut stream = client
        .dispatch(&launch.credentials, "malformed-result", req)
        .await
        .unwrap();
    let err = stream.frames.next_frame().await.unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::MalformedFrame { .. }
    ));
    launch.shutdown().await;
}

#[tokio::test]
async fn stall_mode_keeps_response_body_pending() {
    let mut launch = spawn_chaos().await;
    let body = operation_body(203, serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "stall",
        "stall_ms": 1000
    }));
    let outcome = tokio::time::timeout(
        Duration::from_millis(250),
        raw_request(launch.bound, body, "stall-mode"),
    )
    .await;
    assert!(outcome.is_err());
    launch.shutdown().await;
}

#[tokio::test]
async fn non_converging_progress_yields_progress_without_terminal() {
    let mut launch = spawn_chaos().await;
    let body = operation_body(204, serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "non_converging_progress",
        "progress_count": 2
    }));
    let mut stream = tokio::net::TcpStream::connect(launch.bound).await.unwrap();
    write_raw_operation(&mut stream, launch.bound, body, "non-converging").await;
    let mut buf = vec![0_u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buf[..n]);
    assert!(text.contains("\"kind\":\"progress\""));
    assert!(!text.contains("\"kind\":\"result\""));
    let later = tokio::time::timeout(Duration::from_millis(250), stream.read(&mut buf)).await;
    assert!(later.is_err());
    launch.shutdown().await;
}

#[tokio::test]
async fn deadline_exceeded_delays_progress_past_short_timeout() {
    let mut launch = spawn_chaos().await;
    let body = operation_body(205, serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "deadline_exceeded",
        "progress_interval_ms": 1000,
        "progress_count": 1
    }));
    let mut stream = tokio::net::TcpStream::connect(launch.bound).await.unwrap();
    write_raw_operation(&mut stream, launch.bound, body, "deadline-exceeded").await;
    let mut buf = vec![0_u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buf[..n]);
    assert!(text.contains("HTTP/1.1 200 OK"));
    assert!(!text.contains("\"kind\":\"result\""));
    launch.shutdown().await;
}

fn operation_request(lease_id: u64, payload: serde_json::Value) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(lease_id),
        payload,
        heartbeat_deadline_ms: 100,
        progress_idle_deadline_ms: 100,
    }
}

fn operation_body(lease_id: u64, payload: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&operation_request(lease_id, payload)).unwrap()
}

async fn raw_request(
    bound: std::net::SocketAddr,
    body: Vec<u8>,
    idempotency_key: &str,
) -> Vec<u8> {
    let mut stream = tokio::net::TcpStream::connect(bound).await.unwrap();
    write_raw_operation(&mut stream, bound, body, idempotency_key).await;
    let mut out = vec![0; 4096];
    let n = stream.read(&mut out).await.unwrap();
    out.truncate(n);
    out
}

async fn write_raw_operation(
    stream: &mut tokio::net::TcpStream,
    bound: std::net::SocketAddr,
    body: Vec<u8>,
    idempotency_key: &str,
) {
    let req = format!(
        "POST /v1/operations HTTP/1.1\r\n\
         Host: {bound}\r\n\
         Content-Length: {}\r\n\
         X-Voom-Protocol-Version: {}\r\n\
         Authorization: Bearer phase1-bootstrap-secret\r\n\
         X-Voom-Worker-Id: 1\r\n\
         X-Voom-Worker-Epoch: 0\r\n\
         X-Voom-Idempotency-Key: {idempotency_key}\r\n\
         \r\n",
        body.len(),
        voom_core::PROTOCOL_VERSION
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
}
```

Run:

```bash
cargo test -p voom-fakes crash_mode_exits_non_zero malformed_result_is_rejected_by_reader stall_mode_keeps_response_body_pending non_converging_progress_yields_progress_without_terminal deadline_exceeded_delays_progress_past_short_timeout --all-features
```

Expected: failures because modes still return invalid payload.

- [ ] **Step 2: Implement fault responses**

Add `progress_body`, then replace `streaming_or_fault_response`:

```rust
fn progress_body(req: &OperationRequest, count: usize) -> Result<Vec<u8>, ProtocolError> {
    let mut body = Vec::new();
    for seq in 0..count {
        let frame = ProgressFrame::Progress {
            lease_id: req.lease_id,
            seq: seq as u64,
            emitted_at: Utc::now(),
            percent: None,
            message: Some("chaos progress".to_owned()),
            payload: Some(serde_json::json!({"mode": "progress"})),
        };
        push_frame(&mut body, &frame)?;
    }
    Ok(body)
}

fn streaming_or_fault_response(
    req: &OperationRequest,
    payload: &ChaosPayload,
    mode: ChaosMode,
) -> Result<ChaosResponse, ProtocolError> {
    match mode {
        ChaosMode::Crash => Ok(ChaosResponse::ExitProcess(101)),
        ChaosMode::MalformedResult => {
            let mut body = operation_response_line(req)?;
            body.extend_from_slice(&malformed_body());
            Ok(ChaosResponse::Fixed {
                status: "200 OK",
                content_type: "application/x-ndjson",
                body,
            })
        }
        ChaosMode::Stall => Ok(ChaosResponse::Open {
            prefix: operation_response_line(req)?,
            chunks: Vec::new(),
            hold: payload.stall,
        }),
        ChaosMode::NonConvergingProgress => Ok(ChaosResponse::Open {
            prefix: operation_response_line(req)?,
            chunks: vec![(progress_body(req, payload.progress_count)?, Duration::ZERO)],
            hold: payload.stall,
        }),
        ChaosMode::DeadlineExceeded => Ok(ChaosResponse::Open {
            prefix: operation_response_line(req)?,
            chunks: vec![(progress_body(req, payload.progress_count)?, payload.progress_interval)],
            hold: payload.stall,
        }),
        ChaosMode::Baseline => fixed_operation_response(req, baseline_body(req, payload)?),
    }
}
```

- [ ] **Step 3: Do not cache deliberate chaos responses**

In `handle_operation`, only cache fixed responses for `ChaosMode::Baseline`. Implement by parsing payload before dispatch and passing `cacheable: bool` or by adding:

```rust
let cacheable = request
    .payload
    .get("mode")
    .and_then(serde_json::Value::as_str)
    .is_none_or(|mode| mode == "baseline");
if cacheable && matches!(response, ChaosResponse::Fixed { .. }) {
    cache.lock().await.record(idempotency_key, body_hash, response.clone());
}
```

- [ ] **Step 4: Run fault-mode tests**

Run:

```bash
cargo test -p voom-fakes crash_mode_exits_non_zero malformed_result_is_rejected_by_reader stall_mode_keeps_response_body_pending non_converging_progress_yields_progress_without_terminal deadline_exceeded_delays_progress_past_short_timeout --all-features
```

Expected: all five fault-mode tests pass.

- [ ] **Step 5: Run all fakes tests**

Run:

```bash
cargo test -p voom-fakes --all-features
```

Expected: all `voom-fakes` tests pass.

- [ ] **Step 6: Commit fault modes**

```bash
git add crates/voom-fakes/src/bin/chaos_worker.rs crates/voom-fakes/tests/chaos_worker.rs
git commit -m "feat(fakes): add chaos worker fault modes"
```

## Task 5: Full Verification and Cleanup

**Files:**
- Modify only if verification finds lint/test issues in files touched above.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt
```

Expected: formatter completes with no errors.

- [ ] **Step 2: Run package tests**

Run:

```bash
cargo test -p voom-fakes --all-features
cargo build -p voom-fakes --bin chaos-worker
cargo test -p voom-conformance --all-features
```

Expected: all pass.

- [ ] **Step 3: Run full CI**

Run:

```bash
just ci
```

Expected: full workspace CI passes.

- [ ] **Step 4: Commit verification cleanup if needed**

If formatting or lint fixes changed files:

Stage only the files changed by formatting or verification cleanup, then commit:

```bash
git add crates/voom-fakes/Cargo.toml crates/voom-fakes/src/bin/chaos_worker.rs crates/voom-fakes/src/bin/chaos_worker_test.rs crates/voom-fakes/tests/chaos_worker.rs crates/voom-conformance/src/manifest.rs crates/voom-conformance/src/manifest_test.rs crates/voom-conformance/tests/conformance_all.rs crates/voom-conformance/voom-fakes-manifest.toml Cargo.lock
git commit -m "fix(fakes): satisfy chaos worker verification"
```

If no files changed, do not create a commit.

## Self-Review Checklist

- Spec coverage:
  - Conformance promotion: Task 3.
  - Required `path` before baseline default: Task 1 parser tests and implementation.
  - Local compatibility shim/no protocol API expansion: Task 2 local constants and Task 3 conformance drift guard.
  - Open-stream modes: Task 4 raw/streaming integration tests and implementation.
  - Verification: Task 5.
- Red-flag scan: no unresolved implementation choices should remain in this plan.
- Type consistency:
  - `ChaosMode`, `ChaosPayload`, `RawChaosPayload`, `ChaosResponse`, `IdempotencyCache`, and helper names are introduced before use.
  - The plan uses `ProbeFile`, `payload.path`, and `payload.mode` consistently with the spec.
