use std::fs;
use std::io::{self, BufRead, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use voom_control_plane::ControlPlane;
use voom_control_plane::workers::RegisterWorkerInput;
use voom_core::{TicketOperation, WorkerKind};
use voom_store::repo::execution::workers::{NewCapability, NewGrant};

const FFPROBE_SIBLING_LOCK_FILE: &str = ".voom-ffprobe-sibling.lock-owner";
const FFPROBE_TEST_HELPER_MARKER: &[u8] = b"ffprobe version test-helper";
const PREBUILT_WORKERS_ENV: &str = "VOOM_TEST_PREBUILT_WORKERS";
const FFPROBE_FIXTURE_DIR: &str = ".voom-ffprobe-sibling.fixture";

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
            .register_worker(RegisterWorkerInput {
                name: config.worker_name,
                kind: config.worker_kind,
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
        let operation = TicketOperation::new(config.operation.clone())?;
        cp.record_capability(NewCapability {
            worker_id: worker.id,
            operation: operation.clone(),
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
            can_execute: vec![operation],
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

// Keep one file in the trusted profile parent; legacy lock directories may be peer-writable.
// Never unlink it: that can split waiters across inodes.
// The kernel releases ownership when the final descriptor closes, even on death.
#[derive(Debug)]
struct TargetDebugLock {
    _file: fs::File,
}

impl TargetDebugLock {
    #[expect(
        clippy::unnecessary_debug_formatting,
        reason = "diagnostics must preserve path bytes and escape control characters"
    )]
    fn acquire_at(profile_dir: &Path, timeout: Duration) -> io::Result<Self> {
        let path = profile_dir.join(FFPROBE_SIBLING_LOCK_FILE);
        let mut options = fs::File::options();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("open lock {:?}: {err}", path.as_os_str()),
            )
        })?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= timeout {
                        return Err(io::Error::new(
                            ErrorKind::TimedOut,
                            format!(
                                "timed out waiting for {:?}; lock state may be stale; after stopping all test processes, from its profile directory, run: rm -f -- {FFPROBE_SIBLING_LOCK_FILE}",
                                path.as_os_str()
                            ),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(fs::TryLockError::Error(err)) => {
                    return Err(io::Error::new(
                        err.kind(),
                        format!("acquire lock {:?}: {err}", path.as_os_str()),
                    ));
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct FfprobeSiblingGuard {
    path: PathBuf,
    // Only non-Unix live guards need this fallback; abandoned guards cannot prove ownership.
    installed_copy: bool,
    _lock: TargetDebugLock,
}

impl Drop for FfprobeSiblingGuard {
    #[expect(
        clippy::print_stderr,
        reason = "Drop cannot return fixture recovery failures"
    )]
    fn drop(&mut self) {
        if let Err(err) = recover_ffprobe_fixture(&self.path, self.installed_copy) {
            eprintln!("ffprobe fixture cleanup failed: {err}");
        }
    }
}

pub fn hide_stale_fake_ffprobe_sibling(_label: &str) -> io::Result<FfprobeSiblingGuard> {
    ffprobe_fixture_at(None, &target_debug_dir())
}

pub fn install_fake_ffprobe_sibling(
    source: &Path,
    _label: &str,
) -> io::Result<FfprobeSiblingGuard> {
    install_fake_ffprobe_sibling_at(source, &target_debug_dir())
}

fn install_fake_ffprobe_sibling_at(
    source: &Path,
    profile: &Path,
) -> io::Result<FfprobeSiblingGuard> {
    ffprobe_fixture_at(Some(source), profile)
}

fn ffprobe_fixture_at(source: Option<&Path>, profile: &Path) -> io::Result<FfprobeSiblingGuard> {
    let lock = TargetDebugLock::acquire_at(profile, Duration::from_mins(2))?;
    let path = profile.join(format!("ffprobe{}", std::env::consts::EXE_SUFFIX));
    for entry in fs::read_dir(profile)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("ffprobe.") && name.contains(".hidden.") {
            return Err(fixture_recovery_error(
                &entry.path(),
                "legacy backup has ambiguous ownership",
            ));
        }
    }
    recover_ffprobe_fixture(&path, false)?;
    let installed_copy = source.is_some();
    if source.is_some() || ffprobe_sibling_is_test_helper(&path) {
        let journal = profile.join(FFPROBE_FIXTURE_DIR);
        let result = (|| {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&journal)?;
            if entry_exists(&path)? {
                if fs::symlink_metadata(&path)?.is_dir() {
                    return Err(io::Error::new(
                        ErrorKind::InvalidInput,
                        "ffprobe sibling is a directory",
                    ));
                }
                fs::rename(&path, journal.join("original"))?;
            }
            if let Some(source) = source {
                // A writer may outlive this owner. Retries must never reuse its pathname.
                let staging = ffprobe_staging_dir(profile)?;
                let staged = staging.path().join("replacement");
                copy_executable(source, &staged)?;
                #[cfg(unix)]
                {
                    fs::rename(&staged, journal.join("replacement"))?;
                    fs::hard_link(journal.join("replacement"), &path)?;
                }
                #[cfg(not(unix))]
                {
                    fs::File::create_new(journal.join("replacement"))?;
                    fs::rename(&staged, &path)?;
                }
            }
            Ok(())
        })();
        fixture_setup_result(result, &path)?;
    }
    Ok(FfprobeSiblingGuard {
        path,
        installed_copy,
        _lock: lock,
    })
}

fn ffprobe_staging_dir(profile: &Path) -> io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(".voom-ffprobe-write-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(profile)
}

fn fixture_setup_result(result: io::Result<()>, path: &Path) -> io::Result<()> {
    if let Err(err) = result {
        if let Err(recovery) = recover_ffprobe_fixture(path, false) {
            return Err(io::Error::new(
                err.kind(),
                format!("fixture setup failed: {err}; rollback failed: {recovery}"),
            ));
        }
        return Err(err);
    }
    Ok(())
}

#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "escape path bytes in recovery diagnostics"
)]
fn fixture_recovery_error(path: &Path, reason: &str) -> io::Error {
    io::Error::other(format!(
        "ffprobe fixture recovery at {:?}: {reason}; stop all test processes, inspect the sibling and retained backup, then manually restore the original before removing the fixture recovery directory",
        path.as_os_str()
    ))
}

fn recover_ffprobe_fixture(path: &Path, live_installed: bool) -> io::Result<()> {
    let journal = path.with_file_name(FFPROBE_FIXTURE_DIR);
    let result = (|| {
        let metadata = match fs::symlink_metadata(&journal) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        if !metadata.is_dir() {
            return Err(io::Error::other("expected a private directory"));
        }
        for entry in fs::read_dir(&journal)? {
            let entry = entry?;
            let name = entry.file_name();
            let kind = entry.file_type()?;
            if !matches!(name.to_str(), Some("original" | "replacement"))
                || kind.is_dir()
                || (name == "replacement" && !kind.is_file())
            {
                return Err(io::Error::other("unexpected recovery entry"));
            }
        }
        let original = journal.join("original");
        let replacement = journal.join("replacement");
        let original_exists = entry_exists(&original)?;
        let replacement_exists = entry_exists(&replacement)?;
        if entry_exists(path)? && (original_exists || replacement_exists) {
            #[cfg(unix)]
            let owned = {
                use std::os::unix::fs::MetadataExt;
                let _ = live_installed;
                let sibling = fs::symlink_metadata(path)?;
                replacement_exists
                    && fs::symlink_metadata(&replacement).is_ok_and(|witness| {
                        sibling.dev() == witness.dev() && sibling.ino() == witness.ino()
                    })
            };
            #[cfg(not(unix))]
            let owned = live_installed && replacement_exists;
            if !owned {
                return Err(io::Error::other("occupied sibling has unproven ownership"));
            }
            fs::remove_file(path)?;
        }
        // Retire the replacement witness before restoring the original. After restoration
        // a crash leaves an empty journal, never a foreign-looking original beside a witness.
        if replacement_exists {
            fs::remove_file(replacement)?;
        }
        if original_exists {
            fs::rename(original, path)?;
        }
        fs::remove_dir(&journal)
    })();
    result.map_err(|err| {
        io::Error::new(
            err.kind(),
            fixture_recovery_error(&journal, &err.to_string()),
        )
    })
}

fn entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

fn copy_executable(source: &Path, destination: &Path) -> io::Result<()> {
    crate::executable::write_executable(destination, fs::read(source)?)
}

fn ffprobe_sibling_is_test_helper(path: &Path) -> bool {
    fs::read(path).is_ok_and(|bytes| {
        bytes
            .windows(FFPROBE_TEST_HELPER_MARKER.len())
            .any(|window| window == FFPROBE_TEST_HELPER_MARKER)
    })
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;

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
    if use_prebuilt_worker_binaries() {
        prebuilt_worker_binary(package)?;
        return Ok(());
    }

    // Fallback for runs without `VOOM_TEST_PREBUILT_WORKERS`. This `-p` build can
    // resolve a different feature set than the workspace build and republish the
    // worker binary, racing a concurrent test that execs it (ETXTBSY/ENOENT).
    // `just test` and `just test-hook` prebuild and set the variable to avoid it.
    let status = Command::new("cargo")
        .args(["build", "-p", package, "--all-features"])
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
    if use_prebuilt_worker_binaries() {
        return prebuilt_worker_binary(binary);
    }

    let status = Command::new("cargo")
        .args(["build", "-p", package, "--bin", binary, "--all-features"])
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

fn use_prebuilt_worker_binaries() -> bool {
    std::env::var_os(PREBUILT_WORKERS_ENV).is_some()
}

fn prebuilt_worker_binary(binary: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = target_debug_binary(binary);
    if path.is_file() {
        return Ok(path);
    }
    Err(io::Error::new(
        ErrorKind::NotFound,
        format!(
            "prebuilt worker binary missing at {}; run `cargo build --workspace \
             --all-features --all-targets` or unset {PREBUILT_WORKERS_ENV}",
            path.display()
        ),
    )
    .into())
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
