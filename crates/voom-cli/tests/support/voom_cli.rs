#![allow(
    dead_code,
    reason = "E2E support helpers are shared across ignored cases"
)]

use std::io::{self, BufRead};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::time::Duration;

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};

use super::chaos_librarian::workspace_root;

pub struct VoomTestDb {
    _file: NamedTempFile,
    pub url: String,
}

pub struct VoomOutput {
    pub status_code: Option<i32>,
    pub json: Value,
    pub stderr: String,
}

pub struct TranscodeWorkerLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl VoomTestDb {
    pub async fn init() -> Result<Self, Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;
        let url = voom_store::test_support::sqlite_url_for(file.path());
        voom_store::init(&url).await?;
        Ok(Self { _file: file, url })
    }

    pub async fn control_plane(&self) -> Result<ControlPlane, Box<dyn std::error::Error>> {
        Ok(ControlPlane::open(&self.url).await?)
    }
}

pub fn run_voom<I, S>(database_url: &str, args: I) -> Result<VoomOutput, Box<dyn std::error::Error>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args(["--database-url", database_url])
        .args(args)
        .env(
            "VOOM_FFPROBE_WORKER_BIN",
            worker_binary("voom-ffprobe-worker"),
        )
        .env(
            "VOOM_FFMPEG_WORKER_BIN",
            worker_binary("voom-ffmpeg-worker"),
        )
        .env(
            "VOOM_VERIFY_ARTIFACT_WORKER_BIN",
            worker_binary("voom-verify-artifact-worker"),
        )
        .output()?;
    output_to_envelope(output)
}

pub fn output_to_envelope(output: Output) -> Result<VoomOutput, Box<dyn std::error::Error>> {
    let stdout = String::from_utf8(output.stdout)?;
    let json = serde_json::from_str(stdout.trim()).map_err(|err| {
        io::Error::other(format!(
            "stdout must contain exactly one JSON envelope; got {stdout:?}: {err}"
        ))
    })?;
    Ok(VoomOutput {
        status_code: output.status.code(),
        json,
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

pub fn worker_binary(name: &str) -> PathBuf {
    workspace_root()
        .join("target")
        .join("debug")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

impl TranscodeWorkerLaunch {
    pub async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        let secret = "chaos-librarian-transcode-e2e-secret";
        let worker = cp
            .register_worker(NewWorker {
                name: "chaos-librarian-ffmpeg".to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: cp.clock().now(),
                node_id: None,
            })
            .await?;
        let mut child = Command::new(worker_binary("voom-ffmpeg-worker"))
            .env("VOOM_WORKER_SECRET", secret)
            .env("VOOM_WORKER_ID", worker.id.0.to_string())
            .env("VOOM_WORKER_EPOCH", "0")
            .env("VOOM_WORKER_BIND", "127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let bound = read_bound_addr(&mut child)?;
        cp.record_capability(NewCapability {
            worker_id: worker.id,
            operation: "transcode_video".to_owned(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: serde_json::json!({
                "endpoint": bound.to_string(),
                "secret": secret,
            }),
        })
        .await?;
        cp.record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec!["transcode_video".to_owned()],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: serde_json::json!({ "transcode_video": 1 }),
        })
        .await?;
        Ok(Self { child, stdin })
    }

    pub fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.shutdown_with_timeout(Duration::from_secs(5))
    }

    fn shutdown_with_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error>> {
        drop(self.stdin.take());
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                if status.success() {
                    return Ok(());
                }
                return Err(
                    io::Error::other(format!("voom-ffmpeg-worker exited with {status}")).into(),
                );
            }
            if started.elapsed() > timeout {
                let _ = self.child.kill();
                return Err(io::Error::other("voom-ffmpeg-worker cleanup timed out").into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for TranscodeWorkerLaunch {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.shutdown_with_timeout(Duration::from_secs(1));
        }
    }
}

fn read_bound_addr(child: &mut Child) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("worker stdout missing"))?;
    let mut lines = std::io::BufReader::new(stdout).lines();
    let line = lines
        .next()
        .transpose()?
        .ok_or_else(|| io::Error::other("worker exited before bind line"))?;
    Ok(line
        .strip_prefix("BOUND addr=")
        .ok_or_else(|| io::Error::other(format!("malformed bind line: {line}")))?
        .parse::<std::net::SocketAddr>()?)
}
