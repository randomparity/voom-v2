//! End-to-end test of the `voom worker run-local` two-line stdout contract.
//!
//! `run-local` is the one documented streaming exception to the
//! one-JSON-envelope-per-invocation CLI contract (`AGENTS.md` → "CLI output
//! contract"). Over a full lifecycle it writes EXACTLY two JSON lines to stdout
//! and nothing else:
//!
//! 1. a bare readiness line — `{"status":"ready",worker_id,kind,endpoint}` — with
//!    no envelope wrapper, emitted once the bundled worker has bound and
//!    registered, and
//! 2. the standard retirement envelope (`status:"ok"`, `command:"worker"`,
//!    `data.status:"retired"`), emitted on shutdown.
//!
//! Unlike `operator_execution_e2e.rs`, which interleaves many commands and only
//! scans for the readiness line and keeps the last envelope, this test collects
//! every stdout line and asserts the count is exactly two, in order — so a stray
//! `println!` or a double emit regresses loudly.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests fail loudly and preserve stderr for diagnosis"
)]

use std::io::{BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use voom_test_support::worker::cargo_build_package;

const READY_TIMEOUT: Duration = Duration::from_mins(1);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread")]
async fn run_local_emits_exactly_two_stdout_lines() {
    // `run-local` resolves the bundled mutation worker as a sibling of the
    // running `voom` binary; build it so the sibling lookup succeeds.
    cargo_build_package("voom-ffmpeg-worker").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let db = tempfile::NamedTempFile::new_in(&root).unwrap();
    let url = format!("sqlite://{}", db.path().display());

    let init = run_voom(&url, &["init"]);
    assert_ok(&init, "init");

    let mut worker = LocalWorker::spawn(&url, "ffmpeg");
    let ready = worker.wait_for_ready(READY_TIMEOUT);
    assert_ready_line(&ready);
    let worker_id = ready["worker_id"].as_u64().unwrap();

    let shutdown = worker.shutdown();

    // The full stdout stream is exactly the readiness line then the retirement
    // envelope — nothing before, between, or after.
    assert_eq!(
        worker.stdout_lines.len(),
        2,
        "run-local stdout must be exactly two lines; saw {:?}",
        worker.stdout_lines
    );
    let first = &worker.stdout_lines[0];
    let line_one: Value = serde_json::from_str(first)
        .unwrap_or_else(|err| panic!("first stdout line must be valid JSON: {first:?}: {err}"));
    assert_ready_line(&line_one);
    assert_eq!(
        line_one["worker_id"].as_u64().unwrap(),
        worker_id,
        "the collected first line is the same readiness line"
    );

    assert_retirement_envelope(&shutdown, worker_id);

    // The supervisor retired the worker it started; it is no longer live.
    let list = run_voom(&url, &["worker", "list"]);
    let list_json = assert_ok(&list, "worker list");
    assert_no_live_worker(&list_json, worker_id);
}

/// Assert a bare readiness line: the `ready` shape with NO envelope wrapper.
fn assert_ready_line(value: &Value) {
    assert_eq!(value["status"], "ready", "readiness line status: {value}");
    assert_eq!(value["kind"], "ffmpeg", "readiness line kind: {value}");
    assert!(
        value["worker_id"].as_u64().is_some_and(|id| id > 0),
        "readiness line carries a positive worker_id: {value}"
    );
    let endpoint = value["endpoint"]
        .as_str()
        .unwrap_or_else(|| panic!("readiness line carries an endpoint string: {value}"));
    endpoint
        .parse::<SocketAddr>()
        .unwrap_or_else(|err| panic!("readiness endpoint must parse: {endpoint:?}: {err}"));
    assert!(
        value.get("schema_version").is_none_or(Value::is_null),
        "readiness line is NOT an envelope (no schema_version): {value}"
    );
    assert!(
        value.get("command").is_none_or(Value::is_null),
        "readiness line is NOT an envelope (no command): {value}"
    );
}

/// Assert the shutdown line is the standard retirement envelope for `worker_id`.
fn assert_retirement_envelope(value: &Value, worker_id: u64) {
    assert_eq!(
        value["command"], "worker",
        "shutdown envelope command: {value}"
    );
    assert_eq!(
        value["status"], "ok",
        "shutdown must retire cleanly: {value}"
    );
    assert!(
        value.get("schema_version").is_some_and(|v| !v.is_null()),
        "retirement envelope carries schema_version: {value}"
    );
    assert_eq!(
        value["data"]["status"], "retired",
        "shutdown envelope reports retirement: {value}"
    );
    assert_eq!(
        value["data"]["worker_id"].as_u64().unwrap(),
        worker_id,
        "retirement names the worker that started: {value}"
    );
}

fn assert_no_live_worker(list_json: &Value, worker_id: u64) {
    let workers = list_json["data"]["workers"].as_array().unwrap();
    let live = workers.iter().find(|worker| {
        worker["id"].as_u64() == Some(worker_id)
            && matches!(worker["status"].as_str(), Some("registered" | "active"))
    });
    assert!(
        live.is_none(),
        "worker {worker_id} must not be live after shutdown: {list_json}"
    );
}

/// A `voom worker run-local` child. stdout lines are read off-thread into a
/// channel AND retained in `stdout_lines` so the test can assert the full stream;
/// stderr is drained off-thread for failure diagnostics.
struct LocalWorker {
    kind: &'static str,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_rx: Receiver<String>,
    stdout_lines: Vec<String>,
    stderr: Arc<Mutex<String>>,
}

impl LocalWorker {
    fn spawn(url: &str, kind: &'static str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_voom"))
            .env("VOOM_DATABASE_URL", url)
            .args(["worker", "run-local", "--kind", kind])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let drain = Arc::clone(&stderr_buf);
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut buf);
            drain.lock().unwrap().push_str(&buf);
        });

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            kind,
            child,
            stdin: Some(stdin),
            stdout_rx: rx,
            stdout_lines: Vec::new(),
            stderr: stderr_buf,
        }
    }

    /// Block until the child prints its readiness line, recording every stdout
    /// line seen along the way. Panics with captured stderr if the child exits
    /// first or no readiness line arrives within `timeout`.
    fn wait_for_ready(&mut self, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "run-local {} exited before ready (status={status}); stderr:\n{}",
                    self.kind,
                    self.stderr_snapshot()
                );
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for {} readiness; stderr:\n{}",
                self.kind,
                self.stderr_snapshot()
            );
            match self
                .stdout_rx
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                Ok(line) => {
                    self.stdout_lines.push(line.clone());
                    if let Ok(value) = serde_json::from_str::<Value>(&line)
                        && value["status"] == "ready"
                    {
                        return value;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => panic!(
                    "run-local {} closed stdout before ready; stderr:\n{}",
                    self.kind,
                    self.stderr_snapshot()
                ),
            }
        }
    }

    /// Close stdin (the shutdown signal), drain remaining stdout into
    /// `stdout_lines`, wait for a clean exit, and return the final JSON line.
    fn shutdown(&mut self) -> Value {
        drop(self.stdin.take());
        while let Ok(line) = self.stdout_rx.recv_timeout(SHUTDOWN_TIMEOUT) {
            self.stdout_lines.push(line);
        }
        let status = self.child.wait().unwrap();
        assert!(
            status.success(),
            "run-local {} exited nonzero ({status}); stderr:\n{}",
            self.kind,
            self.stderr_snapshot()
        );
        let last = self.stdout_lines.last().unwrap_or_else(|| {
            panic!(
                "run-local {} printed no stdout; stderr:\n{}",
                self.kind,
                self.stderr_snapshot()
            )
        });
        serde_json::from_str(last).unwrap_or_else(|err| {
            panic!(
                "run-local {} final stdout line must be JSON; got {last:?}: {err}; stderr:\n{}",
                self.kind,
                self.stderr_snapshot()
            )
        })
    }

    fn stderr_snapshot(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }
}

impl Drop for LocalWorker {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn run_voom(url: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_voom"))
        .env("VOOM_DATABASE_URL", url)
        .args(args)
        .output()
        .unwrap()
}

fn assert_ok(output: &std::process::Output, what: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{what} must exit 0; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let value: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{what} stdout must be one JSON envelope; got {stdout:?}: {e}"));
    assert_eq!(value["status"], "ok", "{what} must be ok: {value}");
    value
}
