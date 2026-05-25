#![allow(
    dead_code,
    reason = "E2E support helpers are shared across ignored cases"
)]

use std::io;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_test_support::worker::{TestWorkerConfig, TestWorkerLaunch, target_debug_binary};

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
    inner: TestWorkerLaunch,
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
            target_debug_binary("voom-ffprobe-worker"),
        )
        .env(
            "VOOM_FFMPEG_WORKER_BIN",
            target_debug_binary("voom-ffmpeg-worker"),
        )
        .env(
            "VOOM_VERIFY_ARTIFACT_WORKER_BIN",
            target_debug_binary("voom-verify-artifact-worker"),
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

impl TranscodeWorkerLaunch {
    pub async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            inner: TestWorkerLaunch::start(
                cp,
                TestWorkerConfig::synthetic(
                    target_debug_binary("voom-ffmpeg-worker"),
                    "chaos-librarian-ffmpeg",
                    "chaos-librarian-transcode-e2e-secret",
                    "transcode_video",
                ),
            )
            .await?,
        })
    }

    pub fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.shutdown()
    }
}
