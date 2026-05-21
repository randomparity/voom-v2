//! Black-box harness that launches a worker binary and drives it
//! over the public `voom-worker-protocol`. Phase 1 design §4.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::time::timeout;
use voom_worker_protocol::WorkerCredentials;

#[derive(Debug, Default, Clone)]
pub struct SuiteResult {
    pub passed: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl SuiteResult {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.passed.is_empty() && self.failed.is_empty()
    }

    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.failed.is_empty()
    }

    pub fn pass(&mut self, name: impl Into<String>) {
        self.passed.push(name.into());
    }

    pub fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.failed.push((name.into(), detail.into()));
    }

    pub fn extend(&mut self, other: Self) {
        self.passed.extend(other.passed);
        self.failed.extend(other.failed);
    }

    pub fn fail_if_empty_for(&mut self, binary_name: &str) {
        if self.is_empty() {
            self.fail(
                format!("{binary_name}::empty_suite"),
                "active binary executed zero conformance checks",
            );
        }
    }
}

#[derive(Debug, Clone)]
pub struct Harness {
    worker_binary: PathBuf,
    extra_env: Vec<(String, String)>,
}

impl Harness {
    pub fn new(worker_binary: impl Into<PathBuf>) -> Self {
        Self {
            worker_binary: worker_binary.into(),
            extra_env: Vec::new(),
        }
    }

    #[must_use]
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.extra_env.push((k.into(), v.into()));
        self
    }

    /// Spawn the worker binary and return a `WorkerLaunch` handle.
    pub async fn launch(&self) -> std::io::Result<WorkerLaunch> {
        let worker_id: u64 = 1;
        let worker_epoch: u64 = 0;
        // Sprint 2 generates a per-spawn random secret; for the
        // bootstrap echo-worker test path we use a deterministic
        // value so failure diagnostics are predictable.
        let secret = "phase1-bootstrap-secret".to_owned();

        let mut cmd = tokio::process::Command::new(&self.worker_binary);
        cmd.env("VOOM_WORKER_SECRET", &secret)
            .env("VOOM_WORKER_ID", worker_id.to_string())
            .env("VOOM_WORKER_EPOCH", worker_epoch.to_string())
            .env("VOOM_WORKER_BIND", "127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("missing stdin handle"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("missing stdout handle"))?;
        let mut reader = BufReader::new(stdout).lines();
        // Worker prints `BOUND addr=127.0.0.1:NNNN` once it's listening.
        let line = match timeout(Duration::from_secs(5), reader.next_line()).await {
            Ok(Ok(Some(s))) => s,
            Ok(Ok(None)) => return Err(std::io::Error::other("worker exited before BOUND line")),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(std::io::Error::other("timed out waiting for BOUND line")),
        };
        let addr_str = line
            .strip_prefix("BOUND addr=")
            .ok_or_else(|| std::io::Error::other(format!("unexpected stdout line: {line}")))?;
        let bound: SocketAddr = addr_str
            .trim()
            .parse()
            .map_err(|e| std::io::Error::other(format!("bad bound addr: {e}")))?;

        let credentials = WorkerCredentials {
            worker_id: voom_core::WorkerId(worker_id),
            worker_epoch,
            secret: SecretString::from(secret),
        };

        Ok(WorkerLaunch {
            child,
            bound,
            stdin: Some(stdin),
            credentials,
        })
    }

    /// Run the typed conformance suite.
    pub async fn run_typed_suite(&self, launch: &mut WorkerLaunch) -> SuiteResult {
        crate::typed_suite::run(launch).await
    }

    /// Run the raw-wire conformance suite.
    pub async fn run_raw_wire_suite(&self, launch: &mut WorkerLaunch) -> SuiteResult {
        crate::raw_wire_suite::run_active_worker(launch).await
    }

    /// Convenience wrapper that runs both suites and merges results.
    pub async fn run_all(&self, launch: &mut WorkerLaunch) -> SuiteResult {
        let mut combined = self.run_typed_suite(launch).await;
        combined.extend(self.run_raw_wire_suite(launch).await);
        combined.fail_if_empty_for(
            self.worker_binary
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("worker"),
        );
        combined
    }
}

pub struct WorkerLaunch {
    pub child: Child,
    pub bound: SocketAddr,
    pub stdin: Option<ChildStdin>,
    pub credentials: WorkerCredentials,
}

impl std::fmt::Debug for WorkerLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerLaunch")
            .field("bound", &self.bound)
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "harness_test.rs"]
mod tests;

impl WorkerLaunch {
    /// Drop the stdin pipe (worker's parent-death watchdog sees EOF
    /// and self-exits), then await child exit within `grace`. On
    /// timeout, kill the child and report.
    pub async fn shutdown(mut self, grace: Duration) -> std::io::Result<std::process::ExitStatus> {
        drop(self.stdin.take());
        if let Ok(status) = timeout(grace, self.child.wait()).await {
            return status;
        }
        self.child.kill().await?;
        self.child.wait().await
    }
}
