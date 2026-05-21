# Control-Plane Benchmark Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a test-only `voom-control-plane` benchmark harness that launches `benchmark-worker`, drives benchmark-mode requests over `HttpClient`, validates metric frames, and records lint-compatible summary diagnostics.

**Architecture:** Keep all benchmark harness logic private to `crates/voom-control-plane/tests/benchmark.rs`. The test uses only public worker protocol APIs and a spawned `benchmark-worker`; it does not introduce supervisor, scheduler, durable lease, or production benchmark code.

**Tech Stack:** Rust 2024, Tokio integration tests, `voom-worker-protocol::HttpClient`, `voom_core` IDs, `secrecy::SecretString`, process spawning via `tokio::process::Command`.

---

### Task 1: Add Control-Plane Test Dependencies

**Files:**
- Modify: `crates/voom-control-plane/Cargo.toml`

- [ ] **Step 1: Update dev-dependencies**

Change the `[dev-dependencies]` block so `voom-control-plane` tests can launch `benchmark-worker` and speak the worker protocol:

```toml
[dev-dependencies]
secrecy = { workspace = true }
tempfile = { workspace = true }
tokio = { workspace = true, features = ["io-util", "process", "time"] }
# FrozenRng/SeededRng from voom-core::rng_test_support, used by lease
# case tests to pin the backoff jitter.
voom-core = { workspace = true, features = ["test-support"] }
voom-worker-protocol = { workspace = true }
```

- [ ] **Step 2: Verify dependency metadata**

Run:

```bash
cargo check -p voom-control-plane --tests --all-features
```

Expected: passes after the dependency edit.

- [ ] **Step 3: Commit dependency change**

```bash
git add crates/voom-control-plane/Cargo.toml
git commit -m "test(control-plane): add benchmark harness dependencies"
```

### Task 2: Add the Complete Benchmark Harness Test

**Files:**
- Create: `crates/voom-control-plane/tests/benchmark.rs`

- [ ] **Step 1: Create the integration test**

Add this complete file:

```rust
#![expect(
    clippy::unwrap_used,
    reason = "fixed non-empty sample vectors are sorted for deterministic summary checks"
)]

use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HttpClient, NdjsonOutcome, OperationKind, OperationRequest,
    ProgressFrame, WorkerCredentials,
};

const OPERATIONS: u64 = 1_000;
const EMIT_EVERY: u64 = 100;
const EXPECTED_PROGRESS_FRAMES: usize = 10;
const WARMUP_SAMPLES: usize = 1;
const MEASURED_SAMPLES: usize = 5;
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(5);
const DISPATCH_ACK_CEILING: Duration = Duration::from_secs(1);
const STREAM_COMPLETION_CEILING: Duration = Duration::from_secs(5);

type TestResult<T> = Result<T, TestFailure>;

#[derive(Debug)]
struct TestFailure(String);

impl Display for TestFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TestFailure {}

impl From<std::io::Error> for TestFailure {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<voom_worker_protocol::ProtocolError> for TestFailure {
    fn from(value: voom_worker_protocol::ProtocolError) -> Self {
        Self(value.to_string())
    }
}

#[tokio::test]
async fn benchmark_worker_protocol_boundary_reports_metrics() -> TestResult<()> {
    let mut launch = BenchmarkLaunch::spawn().await?;
    let result = run_benchmark_samples(&launch).await;
    let cleanup = launch.shutdown().await;

    match (result, cleanup) {
        (Ok(summary), Ok(())) => {
            summary.assert_sane()?;
            Ok(())
        }
        (Err(err), Ok(())) => Err(err),
        (Ok(summary), Err(cleanup_err)) => {
            Err(TestFailure(format!("{summary}; cleanup failed: {cleanup_err}")))
        }
        (Err(err), Err(cleanup_err)) => {
            Err(TestFailure(format!("{err}; cleanup failed: {cleanup_err}")))
        }
    }
}

async fn run_benchmark_samples(launch: &BenchmarkLaunch) -> TestResult<BenchmarkSummary> {
    let client = HttpClient::new(launch.bound);
    let mut samples = Vec::with_capacity(MEASURED_SAMPLES);
    let total_samples = WARMUP_SAMPLES + MEASURED_SAMPLES;

    for sample_index in 0..total_samples {
        let lease_id = voom_core::LeaseId(10_000 + u64::try_from(sample_index).unwrap());
        let idempotency_key = format!("control-plane-benchmark-{sample_index}");
        let request = benchmark_request(lease_id);
        let request_start = Instant::now();

        let dispatch = tokio::time::timeout(
            SAMPLE_TIMEOUT,
            client.dispatch(&launch.credentials, &idempotency_key, request),
        )
        .await
        .map_err(|_| TestFailure(format!("sample {sample_index}: dispatch timed out")))??;
        let dispatch_ack_latency = request_start.elapsed();

        let worker_ops_per_second_milli =
            collect_and_validate_stream(sample_index, lease_id, dispatch).await?;
        let stream_completion_latency = request_start.elapsed();

        if sample_index >= WARMUP_SAMPLES {
            samples.push(BenchmarkSample {
                dispatch_ack_latency,
                stream_completion_latency,
                worker_ops_per_second_milli,
            });
        }
    }

    Ok(BenchmarkSummary { samples })
}

async fn collect_and_validate_stream(
    sample_index: usize,
    lease_id: voom_core::LeaseId,
    mut dispatch: DispatchStream,
) -> TestResult<u64> {
    if dispatch.response.lease_id != lease_id {
        return Err(TestFailure(format!(
            "sample {sample_index}: response lease mismatch expected={lease_id:?} got={:?}",
            dispatch.response.lease_id
        )));
    }

    let mut progress_frames = 0usize;
    let mut previous_elapsed = None;

    loop {
        let outcome = tokio::time::timeout(SAMPLE_TIMEOUT, dispatch.frames.next_frame())
            .await
            .map_err(|_| TestFailure(format!("sample {sample_index}: stream read timed out")))??;

        match outcome {
            NdjsonOutcome::Frame(frame) => {
                let ProgressFrame::Progress { payload, .. } = frame else {
                    return Err(TestFailure(format!(
                        "sample {sample_index}: non-progress frame before terminal"
                    )));
                };
                let payload = payload.ok_or_else(|| {
                    TestFailure(format!("sample {sample_index}: progress payload missing"))
                })?;
                validate_progress_payload(
                    sample_index,
                    progress_frames,
                    &payload,
                    &mut previous_elapsed,
                )?;
                progress_frames += 1;
            }
            NdjsonOutcome::Terminated(frame) => {
                let ProgressFrame::Result { payload, .. } = frame else {
                    return Err(TestFailure(format!(
                        "sample {sample_index}: expected terminal result frame"
                    )));
                };
                if progress_frames != EXPECTED_PROGRESS_FRAMES {
                    return Err(TestFailure(format!(
                        "sample {sample_index}: progress frame count expected={EXPECTED_PROGRESS_FRAMES} got={progress_frames}"
                    )));
                }
                let throughput = validate_result_payload(sample_index, &payload)?;
                let err = dispatch.frames.next_frame().await.unwrap_err();
                if !matches!(err, voom_worker_protocol::ProtocolError::UnexpectedFrameAfterTerminal)
                {
                    return Err(TestFailure(format!(
                        "sample {sample_index}: expected UnexpectedFrameAfterTerminal got={err}"
                    )));
                }
                return Ok(throughput);
            }
            other => {
                return Err(TestFailure(format!(
                    "sample {sample_index}: unexpected stream outcome {other:?}"
                )));
            }
        }
    }
}

fn validate_progress_payload(
    sample_index: usize,
    progress_index: usize,
    payload: &serde_json::Value,
    previous_elapsed: &mut Option<u64>,
) -> TestResult<()> {
    expect_str(sample_index, payload, "mode", "benchmark")?;
    expect_u64(sample_index, payload, "operations_total", OPERATIONS)?;
    expect_u64(
        sample_index,
        payload,
        "sample_index",
        u64::try_from(progress_index).unwrap(),
    )?;
    expect_u64(
        sample_index,
        payload,
        "operations_completed",
        EMIT_EVERY * (u64::try_from(progress_index).unwrap() + 1),
    )?;
    let elapsed = get_u64(sample_index, payload, "elapsed_worker_ns")?;
    if let Some(previous) = previous_elapsed {
        if elapsed < *previous {
            return Err(TestFailure(format!(
                "sample {sample_index}: elapsed_worker_ns went backward previous={previous} current={elapsed}"
            )));
        }
    }
    *previous_elapsed = Some(elapsed);
    Ok(())
}

fn validate_result_payload(sample_index: usize, payload: &serde_json::Value) -> TestResult<u64> {
    expect_str(sample_index, payload, "mode", "benchmark")?;
    expect_u64(sample_index, payload, "operations_total", OPERATIONS)?;
    expect_u64(
        sample_index,
        payload,
        "progress_frames",
        u64::try_from(EXPECTED_PROGRESS_FRAMES).unwrap(),
    )?;
    let elapsed = get_u64(sample_index, payload, "elapsed_worker_ns")?;
    if elapsed == 0 {
        return Err(TestFailure(format!(
            "sample {sample_index}: terminal elapsed_worker_ns must be positive"
        )));
    }
    let throughput = get_u64(sample_index, payload, "worker_ops_per_second_milli")?;
    if throughput == 0 {
        return Err(TestFailure(format!(
            "sample {sample_index}: worker_ops_per_second_milli must be positive"
        )));
    }
    Ok(throughput)
}

fn expect_str(
    sample_index: usize,
    payload: &serde_json::Value,
    key: &str,
    expected: &str,
) -> TestResult<()> {
    let actual = payload.get(key).and_then(serde_json::Value::as_str);
    if actual != Some(expected) {
        return Err(TestFailure(format!(
            "sample {sample_index}: {key} expected={expected:?} got={actual:?}"
        )));
    }
    Ok(())
}

fn expect_u64(
    sample_index: usize,
    payload: &serde_json::Value,
    key: &str,
    expected: u64,
) -> TestResult<()> {
    let actual = get_u64(sample_index, payload, key)?;
    if actual != expected {
        return Err(TestFailure(format!(
            "sample {sample_index}: {key} expected={expected} got={actual}"
        )));
    }
    Ok(())
}

fn get_u64(sample_index: usize, payload: &serde_json::Value, key: &str) -> TestResult<u64> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| TestFailure(format!("sample {sample_index}: {key} missing or not u64")))
}

fn benchmark_request(lease_id: voom_core::LeaseId) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id,
        payload: serde_json::json!({
            "path": "/library/benchmark.mkv",
            "mode": "benchmark",
            "operations": OPERATIONS,
            "emit_every": EMIT_EVERY
        }),
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    }
}

struct BenchmarkLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
    bound: std::net::SocketAddr,
    credentials: WorkerCredentials,
}

impl BenchmarkLaunch {
    async fn spawn() -> TestResult<Self> {
        let worker_id = voom_core::WorkerId(1);
        let worker_epoch = 0;
        let secret = "control-plane-benchmark-secret";
        let worker_bin = benchmark_worker_bin();
        let mut child = tokio::process::Command::new(&worker_bin)
            .env("VOOM_WORKER_SECRET", secret)
            .env("VOOM_WORKER_ID", worker_id.0.to_string())
            .env("VOOM_WORKER_EPOCH", worker_epoch.to_string())
            .env("VOOM_WORKER_BIND", "127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                TestFailure(format!(
                    "spawn benchmark-worker at {} failed: {e}",
                    worker_bin.display()
                ))
            })?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TestFailure("benchmark-worker stdout missing".to_owned()))?;
        let mut lines = BufReader::new(stdout).lines();
        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .map_err(|_| TestFailure("benchmark-worker bind line timed out".to_owned()))?
            .map_err(TestFailure::from)?
            .ok_or_else(|| TestFailure("benchmark-worker exited before bind line".to_owned()))?;
        let bound = line
            .strip_prefix("BOUND addr=")
            .ok_or_else(|| TestFailure(format!("malformed benchmark-worker bind line: {line}")))?
            .parse::<std::net::SocketAddr>()
            .map_err(|e| TestFailure(format!("benchmark-worker bind addr parse failed: {e}")))?;
        Ok(Self {
            child,
            stdin,
            bound,
            credentials: WorkerCredentials {
                worker_id,
                worker_epoch,
                secret: SecretString::from(secret),
            },
        })
    }

    async fn shutdown(&mut self) -> TestResult<()> {
        drop(self.stdin.take());
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .map_err(|_| {
                let _ = self.child.start_kill();
                TestFailure("benchmark-worker cleanup timed out".to_owned())
            })?
            .map_err(TestFailure::from)?;
        if !status.success() {
            return Err(TestFailure(format!("benchmark-worker exited with {status}")));
        }
        Ok(())
    }
}

impl Drop for BenchmarkLaunch {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn benchmark_worker_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("VOOM_BENCHMARK_WORKER_BIN") {
        return PathBuf::from(path);
    }
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_benchmark-worker") {
        return PathBuf::from(path);
    }
    let target_dir =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(default_target_dir, PathBuf::from);
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    target_dir
        .join("debug")
        .join(format!("benchmark-worker{suffix}"))
}

fn default_target_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map_or_else(|| PathBuf::from("target"), |workspace| workspace.join("target"))
}

#[derive(Debug)]
struct BenchmarkSample {
    dispatch_ack_latency: Duration,
    stream_completion_latency: Duration,
    worker_ops_per_second_milli: u64,
}

#[derive(Debug)]
struct BenchmarkSummary {
    samples: Vec<BenchmarkSample>,
}

impl BenchmarkSummary {
    fn assert_sane(&self) -> TestResult<()> {
        if self.samples.len() != MEASURED_SAMPLES {
            return Err(TestFailure(format!(
                "{self}: measured sample count expected={MEASURED_SAMPLES} got={}",
                self.samples.len()
            )));
        }
        for sample in &self.samples {
            if sample.dispatch_ack_latency > DISPATCH_ACK_CEILING {
                return Err(TestFailure(format!(
                    "{self}: dispatch ack latency exceeded ceiling sample={:?} ceiling={:?}",
                    sample.dispatch_ack_latency, DISPATCH_ACK_CEILING
                )));
            }
            if sample.stream_completion_latency > STREAM_COMPLETION_CEILING {
                return Err(TestFailure(format!(
                    "{self}: stream completion latency exceeded ceiling sample={:?} ceiling={:?}",
                    sample.stream_completion_latency, STREAM_COMPLETION_CEILING
                )));
            }
            if sample.worker_ops_per_second_milli == 0 {
                return Err(TestFailure(format!(
                    "{self}: worker_ops_per_second_milli must be positive"
                )));
            }
        }
        Ok(())
    }

    fn duration_stats(&self, f: impl Fn(&BenchmarkSample) -> Duration) -> DurationStats {
        let mut values: Vec<Duration> = self.samples.iter().map(f).collect();
        values.sort();
        DurationStats {
            min: values[0],
            median: values[values.len() / 2],
            max: values[values.len() - 1],
        }
    }

    fn throughput_stats(&self) -> ThroughputStats {
        let mut values: Vec<u64> = self
            .samples
            .iter()
            .map(|sample| sample.worker_ops_per_second_milli)
            .collect();
        values.sort_unstable();
        ThroughputStats {
            min: values[0],
            median: values[values.len() / 2],
            max: values[values.len() - 1],
        }
    }
}

impl Display for BenchmarkSummary {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let dispatch = self.duration_stats(|sample| sample.dispatch_ack_latency);
        let stream = self.duration_stats(|sample| sample.stream_completion_latency);
        let throughput = self.throughput_stats();
        write!(
            f,
            "benchmark summary: samples={} dispatch_ack={dispatch} stream_completion={stream} worker_ops_per_second_milli={throughput}",
            self.samples.len()
        )
    }
}

#[derive(Debug)]
struct DurationStats {
    min: Duration,
    median: Duration,
    max: Duration,
}

impl Display for DurationStats {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "min={:?}, median={:?}, max={:?}",
            self.min, self.median, self.max
        )
    }
}

#[derive(Debug)]
struct ThroughputStats {
    min: u64,
    median: u64,
    max: u64,
}

impl Display for ThroughputStats {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "min={}, median={}, max={}",
            self.min, self.median, self.max
        )
    }
}
```

- [ ] **Step 2: Run the test with a forced missing worker path**

Run:

```bash
VOOM_BENCHMARK_WORKER_BIN=/tmp/voom-missing-benchmark-worker \
  cargo test -p voom-control-plane --test benchmark --all-features
```

Expected: FAIL with a setup error like `spawn benchmark-worker at /tmp/voom-missing-benchmark-worker failed`. This verifies the new test is active and proves missing binaries fail instead of being skipped.

### Task 3: Build the Worker and Turn the Harness Green

**Files:**
- Modify only if required: `crates/voom-control-plane/tests/benchmark.rs`

- [ ] **Step 1: Build benchmark-worker**

```bash
cargo build -p voom-fakes --bin benchmark-worker
```

Expected: build succeeds and produces `target/debug/benchmark-worker`, or the corresponding binary under `CARGO_TARGET_DIR`.

- [ ] **Step 2: Run the control-plane benchmark test**

```bash
cargo test -p voom-control-plane --test benchmark --all-features
```

Expected: one integration test passes.

- [ ] **Step 3: Fix only concrete compile or behavior issues**

If the test fails, keep fixes inside `crates/voom-control-plane/tests/benchmark.rs` unless the failure proves the existing `benchmark-worker` violates its already-approved contract. Preserve these requirements:

```rust
// No println!/eprintln!.
// Ordinary validation failures return TestFailure.
// Drop keeps the Child::start_kill fallback.
// Local timing assertions enforce only upper bounds.
```

- [ ] **Step 4: Commit the benchmark harness**

```bash
git add crates/voom-control-plane/Cargo.toml crates/voom-control-plane/tests/benchmark.rs
git commit -m "test(control-plane): add benchmark worker harness"
```

### Task 4: Run Boundary Regression Checks

**Files:**
- Verify only; no planned source edits.

- [ ] **Step 1: Run benchmark-worker integration tests**

```bash
cargo test -p voom-fakes --test benchmark_worker --all-features
```

Expected: all benchmark-worker process tests pass.

- [ ] **Step 2: Run benchmark-worker unit tests**

```bash
cargo test -p voom-fakes --bin benchmark-worker --all-features
```

Expected: all benchmark-worker binary unit tests pass.

- [ ] **Step 3: Run control-plane benchmark test again**

```bash
cargo build -p voom-fakes --bin benchmark-worker
cargo test -p voom-control-plane --test benchmark --all-features
```

Expected: one control-plane benchmark integration test passes.

### Task 5: Final Verification

**Files:**
- Verify only; no planned source edits unless a verification command exposes a concrete issue.

- [ ] **Step 1: Run formatting check**

```bash
cargo fmt --all -- --check
```

Expected: no formatting diffs.

- [ ] **Step 2: Run full CI**

```bash
just ci
```

Expected: all checks pass. This includes clippy with `print_stdout`, `print_stderr`, and `allow_attributes` denied, so the benchmark test must not use print macros or local lint allows.

- [ ] **Step 3: Commit any verification fixes**

If formatting or CI requires a code change, commit the minimal fix:

```bash
git add crates/voom-control-plane/Cargo.toml crates/voom-control-plane/tests/benchmark.rs
git commit -m "fix(control-plane): satisfy benchmark harness verification"
```

If no changes are needed, do not create an empty commit.

## Self-Review Checklist

- The plan covers the spec's test-only control-plane harness and does not add supervisor, scheduler, durable lease, watchdog, or production benchmark APIs.
- Binary resolution is decision-complete and matches the spec: `VOOM_BENCHMARK_WORKER_BIN`, `CARGO_BIN_EXE_benchmark-worker`, then `target/debug/benchmark-worker` under `CARGO_TARGET_DIR` or workspace `target`.
- The primary verification includes `cargo build -p voom-fakes --bin benchmark-worker` before the control-plane benchmark test.
- The test design returns `Result`, records validation errors, runs explicit cleanup, and keeps a `Drop` fallback with `start_kill`.
- The summary is exposed through `Display` and failure messages; the plan does not use `println!`, `eprintln!`, or local lint `allow` attributes.
- Local timing checks only enforce upper bounds. Worker throughput remains positive.
- The existing dirty file `docs/superpowers/plans/2026-05-21-voom-sprint-2-phase-4-chaos-worker.md` is unrelated and must not be touched.
