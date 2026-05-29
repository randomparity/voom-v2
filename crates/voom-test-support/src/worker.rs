use std::io::{self, BufRead};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use voom_control_plane::ControlPlane;
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};

#[derive(Debug, Clone)]
pub struct TestWorkerConfig {
    pub binary_path: PathBuf,
    pub worker_name: String,
    pub worker_kind: WorkerKind,
    pub secret: String,
    pub operation: String,
    pub max_parallel: u64,
}

#[derive(Debug)]
pub struct TestWorkerLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl TestWorkerConfig {
    #[must_use]
    pub fn synthetic(
        binary_path: impl Into<PathBuf>,
        worker_name: impl Into<String>,
        secret: impl Into<String>,
        operation: impl Into<String>,
    ) -> Self {
        Self {
            binary_path: binary_path.into(),
            worker_name: worker_name.into(),
            worker_kind: WorkerKind::Synthetic,
            secret: secret.into(),
            operation: operation.into(),
            max_parallel: 1,
        }
    }
}

impl TestWorkerLaunch {
    pub async fn start(
        cp: &ControlPlane,
        config: TestWorkerConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let worker = cp
            .register_worker(NewWorker {
                name: config.worker_name,
                kind: config.worker_kind,
                registered_at: cp.clock().now(),
                node_id: None,
            })
            .await?;
        let mut child = Command::new(&config.binary_path)
            .env("VOOM_WORKER_SECRET", &config.secret)
            .env("VOOM_WORKER_ID", worker.id.0.to_string())
            .env("VOOM_WORKER_EPOCH", "0")
            .env("VOOM_WORKER_BIND", "127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let bound = read_bound_addr(&mut child, &config.binary_path)?;
        cp.record_capability(NewCapability {
            worker_id: worker.id,
            operation: config.operation.clone(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: serde_json::json!({
                "endpoint": bound.to_string(),
                "secret": config.secret,
            }),
        })
        .await?;
        cp.record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec![config.operation.clone()],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: serde_json::json!({ config.operation: config.max_parallel }),
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
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                if status.success() {
                    return Ok(());
                }
                return Err(io::Error::other(format!("worker exited with {status}")).into());
            }
            if started.elapsed() > timeout {
                let _ = self.child.kill();
                return Err(io::Error::other("worker cleanup timed out").into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for TestWorkerLaunch {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.shutdown_with_timeout(Duration::from_secs(1));
        }
    }
}

#[must_use]
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Returns the cargo profile output directory that holds built binaries.
///
/// This is the directory the bundled control-plane worker derives from
/// `std::env::current_exe()` when it auto-wires sibling binaries, so test
/// helpers that touch those siblings must agree on it. It is robust across
/// cargo target dirs (`target/debug`, `target/llvm-cov-target/debug`, a custom
/// `CARGO_TARGET_DIR`, or a `--target <triple>` profile dir) because it walks up
/// from the running test binary rather than assuming `target/debug`.
#[must_use]
pub fn target_debug_dir() -> PathBuf {
    let Ok(current_exe) = std::env::current_exe() else {
        return workspace_root().join("target").join("debug");
    };
    let Some(exe_dir) = current_exe.parent() else {
        return workspace_root().join("target").join("debug");
    };
    if exe_dir.file_name().is_some_and(|name| name == "deps") {
        return exe_dir
            .parent()
            .map_or_else(|| exe_dir.to_path_buf(), std::path::Path::to_path_buf);
    }
    exe_dir.to_path_buf()
}

#[must_use]
pub fn target_debug_binary(name: &str) -> PathBuf {
    target_debug_dir().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Returns the cargo target root that owns the active profile directory.
///
/// The nested `cargo build` invocations below must emit binaries into the same
/// profile dir that [`target_debug_binary`] looks them up in. A nested `cargo
/// build` does not inherit the outer `--target-dir` (e.g. llvm-cov's
/// `target/llvm-cov-target`), so it would otherwise build into the default
/// `target/debug` and the spawn-path lookup would miss. We derive the target
/// root as the parent of the profile dir returned by [`target_debug_dir`] and
/// pass it back via an explicit `--target-dir`, keeping build output and lookup
/// in agreement across normal, coverage, and custom target dirs.
fn target_root_dir() -> PathBuf {
    let profile_dir = target_debug_dir();
    profile_dir
        .parent()
        .map_or(profile_dir.clone(), std::path::Path::to_path_buf)
}

pub fn cargo_build_package(package: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("cargo")
        .args(["build", "-p", package])
        .arg("--target-dir")
        .arg(target_root_dir())
        .current_dir(workspace_root())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("failed to build {package}: {status}")).into())
    }
}

pub fn cargo_bin_or_build(
    package: &str,
    binary: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let env_name = format!("CARGO_BIN_EXE_{binary}");
    if let Some(path) = std::env::var_os(env_name) {
        return Ok(PathBuf::from(path));
    }
    let status = Command::new("cargo")
        .args(["build", "-p", package, "--bin", binary])
        .arg("--target-dir")
        .arg(target_root_dir())
        .current_dir(workspace_root())
        .status()?;
    if !status.success() {
        return Err(
            io::Error::other(format!("failed to build {package}/{binary}: {status}")).into(),
        );
    }
    Ok(target_debug_binary(binary))
}

fn read_bound_addr(
    child: &mut Child,
    binary_path: &std::path::Path,
) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other(format!("{} stdout missing", binary_path.display())))?;
    let mut lines = std::io::BufReader::new(stdout).lines();
    let line = lines.next().transpose()?.ok_or_else(|| {
        io::Error::other(format!("{} exited before bind line", binary_path.display()))
    })?;
    Ok(line
        .strip_prefix("BOUND addr=")
        .ok_or_else(|| io::Error::other(format!("malformed bind line: {line}")))?
        .parse::<std::net::SocketAddr>()?)
}
