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
use voom_test_support::worker::cargo_bin_or_build;
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
        (Ok(summary), Err(cleanup_err)) => Err(TestFailure(format!(
            "{summary}; cleanup failed: {cleanup_err}"
        ))),
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
                if !matches!(
                    err,
                    voom_worker_protocol::ProtocolError::UnexpectedFrameAfterTerminal
                ) {
                    return Err(TestFailure(format!(
                        "sample {sample_index}: expected UnexpectedFrameAfterTerminal got={err}"
                    )));
                }
                return Ok(throughput);
            }
            other @ NdjsonOutcome::StreamEnd => {
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
    if let Some(previous) = previous_elapsed
        && elapsed < *previous
    {
        return Err(TestFailure(format!(
            "sample {sample_index}: elapsed_worker_ns went backward previous={previous} current={elapsed}"
        )));
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
        let worker_bin = benchmark_worker_bin()?;
        let child = tokio::process::Command::new(&worker_bin)
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
        let mut pending = PendingLaunch { child: Some(child) };
        let result = async {
            let stdin = pending.child_mut()?.stdin.take();
            let stdout = pending
                .child_mut()?
                .stdout
                .take()
                .ok_or_else(|| TestFailure("benchmark-worker stdout missing".to_owned()))?;
            let mut lines = BufReader::new(stdout).lines();
            let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
                .await
                .map_err(|_| TestFailure("benchmark-worker bind line timed out".to_owned()))?
                .map_err(TestFailure::from)?
                .ok_or_else(|| {
                    TestFailure("benchmark-worker exited before bind line".to_owned())
                })?;
            let bound = line
                .strip_prefix("BOUND addr=")
                .ok_or_else(|| {
                    TestFailure(format!("malformed benchmark-worker bind line: {line}"))
                })?
                .parse::<std::net::SocketAddr>()
                .map_err(|e| {
                    TestFailure(format!("benchmark-worker bind addr parse failed: {e}"))
                })?;
            let child = pending.take_child()?;
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
        .await;
        if result.is_err() {
            let _ = pending.kill_and_wait().await;
        }
        result
    }

    async fn shutdown(&mut self) -> TestResult<()> {
        drop(self.stdin.take());
        let status = if let Ok(status) =
            tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await
        {
            status.map_err(TestFailure::from)?
        } else {
            terminate_child(&mut self.child).await?;
            return Err(TestFailure("benchmark-worker cleanup timed out".to_owned()));
        };
        if !status.success() {
            return Err(TestFailure(format!(
                "benchmark-worker exited with {status}"
            )));
        }
        Ok(())
    }
}

impl Drop for BenchmarkLaunch {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

struct PendingLaunch {
    child: Option<Child>,
}

impl PendingLaunch {
    fn child_mut(&mut self) -> TestResult<&mut Child> {
        self.child
            .as_mut()
            .ok_or_else(|| TestFailure("benchmark-worker pending child missing".to_owned()))
    }

    fn take_child(&mut self) -> TestResult<Child> {
        self.child
            .take()
            .ok_or_else(|| TestFailure("benchmark-worker pending child already taken".to_owned()))
    }

    async fn kill_and_wait(&mut self) -> TestResult<()> {
        if let Some(mut child) = self.child.take() {
            terminate_child(&mut child).await?;
        }
        Ok(())
    }
}

impl Drop for PendingLaunch {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

async fn terminate_child(child: &mut Child) -> TestResult<()> {
    let _ = child.start_kill();
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .map_err(|_| TestFailure("benchmark-worker kill cleanup timed out".to_owned()))?
        .map_err(TestFailure::from)?;
    Ok(())
}

fn benchmark_worker_bin() -> TestResult<PathBuf> {
    if let Some(path) = std::env::var_os("VOOM_BENCHMARK_WORKER_BIN") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_benchmark-worker") {
        return Ok(PathBuf::from(path));
    }
    cargo_bin_or_build("voom-fakes", "benchmark-worker")
        .map_err(|e| TestFailure(format!("benchmark-worker build lookup failed: {e}")))
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
