#![allow(
    dead_code,
    reason = "E2E support helpers are shared across ignored cases"
)]

use std::io;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use voom_control_plane::ControlPlane;
use voom_test_support::TempDatabase;
use voom_test_support::worker::target_debug_binary;

pub struct VoomTestDb {
    _file: TempDatabase,
    pub url: String,
}

pub struct VoomOutput {
    pub status_code: Option<i32>,
    pub json: Value,
    pub stderr: String,
}

impl VoomTestDb {
    pub async fn init() -> Result<Self, Box<dyn std::error::Error>> {
        let file = TempDatabase::new()?;
        let url = voom_store::test_support::sqlite_url_for(file.path());
        voom_store::init(&url).await?;
        Ok(Self { _file: file, url })
    }

    pub async fn control_plane(&self) -> Result<ControlPlane, Box<dyn std::error::Error>> {
        Ok(ControlPlane::open(&self.url).await?)
    }

    pub async fn configure_local_root(
        &self,
        path: &Path,
    ) -> Result<voom_core::StorageRootId, Box<dyn std::error::Error>> {
        let pool = voom_store::connect(&self.url).await?;
        let root_id = voom_store::test_support::seed_test_storage_root(&pool).await?;
        voom_store::test_support::set_test_storage_root_path(&pool, path).await?;
        // Make the root its own staging and backup default so envelope
        // destinations resolve inside the library tree rather than escaping the
        // storage root during commit.
        sqlx::query(
            "UPDATE library_roots SET default_staging_root_id = id, \
             default_backup_root_id = id WHERE id = ?",
        )
        .bind(i64::try_from(root_id.0)?)
        .execute(&pool)
        .await?;
        Ok(root_id)
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
        .env(
            "VOOM_LOCAL_NODE_ID",
            voom_store::test_support::TEST_STORAGE_ROOT_ID.0.to_string(),
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
