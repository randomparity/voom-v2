#![allow(
    dead_code,
    reason = "E2E support helpers are shared across ignored cases"
)]

use std::io;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use voom_control_plane::ControlPlane;
use voom_core::{ProviderLocator, StorageProviderKind, StorageRootId};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryRootUpdate, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_test_support::TempDatabase;
use voom_test_support::worker::{TestWorkerConfig, TestWorkerLaunch, target_debug_binary};

pub struct VoomTestDb {
    _file: TempDatabase,
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
    ) -> Result<StorageRootId, Box<dyn std::error::Error>> {
        let pool = voom_store::connect(&self.url).await?;
        let root_id = voom_store::test_support::seed_test_storage_root(&pool).await?;
        voom_store::test_support::set_test_storage_root_path(&pool, path).await?;
        Ok(root_id)
    }

    /// Register `path` as an active output root of `source_root_id`'s library
    /// and make it that root's default output root.
    ///
    /// ADR 0055 requires an artifact commit target to sit inside the source
    /// root's configured output root, and inside the source root itself when
    /// none is configured. An operator therefore registers the output
    /// directory as its own root instead of writing artifacts back into the
    /// library that was scanned.
    pub async fn configure_output_root(
        &self,
        source_root_id: StorageRootId,
        path: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::create_dir_all(path)?;
        let cp = self.control_plane().await?;
        let source = cp
            .get_library_root(source_root_id)
            .await?
            .ok_or_else(|| format!("source root {source_root_id} does not exist"))?;
        let owner_node_id = source
            .owner_node_id
            .ok_or_else(|| format!("source root {source_root_id} has no owner node"))?;
        let locator = path
            .to_str()
            .ok_or_else(|| format!("output root path is not UTF-8: {}", path.display()))?
            .to_owned();
        let output = cp
            .create_library_root(NewLibraryRoot {
                library_id: source.library_id,
                owner_node_id,
                provider_kind: StorageProviderKind::LocalFilesystem,
                provider_locator: ProviderLocator::new(locator.clone())?,
                display_locator: locator,
                include_globs: Vec::new(),
                exclude_globs: Vec::new(),
                extension_allowlist: Vec::new(),
                scan_mode: LibraryScanMode::ManualRecursive,
                symlink_policy: SymlinkPolicy::Reject,
                hidden_file_policy: HiddenFilePolicy::Ignore,
                max_depth: None,
                stability_seconds: 0,
                debounce_seconds: 0,
                default_output_root_id: None,
                default_staging_root_id: None,
                default_backup_root_id: None,
                enabled: true,
            })
            .await?;
        cp.activate_library_root(output.id, "chaos-librarian-output".to_owned())
            .await?;
        cp.update_library_root(
            source_root_id,
            LibraryRootUpdate {
                default_output_root_id: Some(Some(output.id)),
                ..LibraryRootUpdate::default()
            },
        )
        .await?;
        Ok(())
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
