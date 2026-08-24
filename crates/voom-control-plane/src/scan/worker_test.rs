use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use voom_core::{ErrorCode, FailureClass, WorkerId};

use super::*;

#[tokio::test]
async fn readiness_authenticates_then_shuts_down_bundled_worker() {
    let dir = tempfile::tempdir().unwrap();
    let ffprobe = write_fake_ffprobe(
        dir.path(),
        "printf '{\"format\":{\"format_name\":\"matroska\"},\"streams\":[]}\\n'\n",
    );
    let command = ffprobe_worker_command().env("VOOM_FFPROBE_BIN", ffprobe.as_os_str());

    verify_ffprobe_readiness(command).await.unwrap();
}

#[tokio::test]
async fn readiness_reaps_worker_after_post_bound_handshake_failure() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("worker.pid");
    let script = format!(
        "printf '%s' $$ > '{}'; printf 'BOUND addr=127.0.0.1:9\\n'; read ignored",
        pid_file.display()
    );
    let command = WorkerCommand::new("/bin/sh").arg("-c").arg(script);

    let err = verify_ffprobe_readiness(command).await.unwrap_err();
    let pid = wait_for_pid_file(&pid_file).await;

    assert!(err.to_string().contains("handshake failed"));
    assert_process_exited(pid.trim());
}

#[test]
fn ffprobe_exit_terminal_error_is_continuable_probe_failure() {
    let err = ScanWorkerError::terminal_error_for_test(
        FailureClass::ExternalSystemUnavailable,
        ErrorCode::ExternalSystemUnavailable,
        "external system unavailable: ffprobe exited with status 1",
        Some(serde_json::json!({"stage": "exit"})),
    );

    assert!(err.is_unprobeable_media());
}

#[test]
fn malformed_media_terminal_error_is_continuable_probe_failure() {
    // A structurally-corrupt source (#248/#287) is a per-file fault the
    // directory scan survives, regardless of the terminal payload stage.
    let err = ScanWorkerError::terminal_error_for_test(
        FailureClass::MalformedMedia,
        ErrorCode::MalformedMedia,
        "malformed media: ffprobe exited with status 1",
        Some(serde_json::json!({"stage": "exit"})),
    );

    assert!(err.is_unprobeable_media());
}

#[test]
fn worker_crash_terminal_error_is_not_continuable() {
    // A worker-level fault (crash / protocol error) aborts the group rather
    // than being recorded as a per-file skip.
    let err = ScanWorkerError::terminal_error_for_test(
        FailureClass::WorkerCrash,
        ErrorCode::WorkerCrash,
        "worker crash",
        None,
    );

    assert!(!err.is_unprobeable_media());
}

#[tokio::test]
async fn launch_timeout_reaps_child_that_never_prints_bound_address() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("worker.pid");
    let script = format!("printf '%s' $$ > '{}'; read ignored", pid_file.display());
    let command = WorkerCommand::new("/bin/sh").arg("-c").arg(script);

    let launch = BundledWorkerProcess::launch_with_startup_timeout(
        WorkerId(46),
        command,
        Duration::from_secs(5),
    );
    tokio::pin!(launch);
    tokio::select! {
        err = &mut launch => panic!("worker launch finished before startup timeout: {err:?}"),
        () = tokio::task::yield_now() => {}
    }
    let pid = wait_for_pid_file(&pid_file).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(5)).await;
    let err = launch.await.unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::WorkerCrash);
    assert_process_exited(pid.trim());
}

#[test]
fn default_ffprobe_worker_command_prefers_current_exe_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let current_exe = dir.path().join("voom");
    let worker = dir.path().join("voom-ffprobe-worker");
    std::fs::write(&worker, b"").unwrap();

    let command = bundled_ffprobe_command_from(None, Ok(current_exe));

    assert_eq!(command.program, worker.as_os_str());
    assert!(command.env.is_empty());
}

#[test]
fn default_ffprobe_worker_command_ignores_sibling_ffprobe() {
    let dir = tempfile::tempdir().unwrap();
    let current_exe = dir.path().join("voom");
    let worker = dir.path().join("voom-ffprobe-worker");
    let ffprobe = dir.path().join("ffprobe");
    std::fs::write(&worker, b"").unwrap();
    std::fs::write(&ffprobe, b"").unwrap();

    let command = bundled_ffprobe_command_from(None, Ok(current_exe));

    assert_eq!(command.program, worker.as_os_str());
    assert!(command.env.is_empty());
}

#[test]
fn default_ffprobe_worker_command_searches_profile_dir_from_test_deps_dir() {
    let dir = tempfile::tempdir().unwrap();
    let deps_dir = dir.path().join("deps");
    std::fs::create_dir(&deps_dir).unwrap();
    let current_exe = deps_dir.join("scan_worker_test");
    let worker = dir.path().join("voom-ffprobe-worker");
    let ffprobe = dir.path().join("ffprobe");
    std::fs::write(&worker, b"").unwrap();
    std::fs::write(&ffprobe, b"").unwrap();

    let command = bundled_ffprobe_command_from(None, Ok(current_exe));

    assert_eq!(command.program, worker.as_os_str());
    assert!(command.env.is_empty());
}

#[test]
fn default_ffprobe_worker_command_falls_back_to_path_when_sibling_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let current_exe = dir.path().join("voom");

    let command = bundled_ffprobe_command_from(None, Ok(current_exe));

    assert_eq!(command.program, OsStr::new("voom-ffprobe-worker"));
}

fn ffprobe_worker_command() -> WorkerCommand {
    if let Some(binary) = std::env::var_os("CARGO_BIN_EXE_voom-ffprobe-worker") {
        return WorkerCommand::new(binary);
    }
    WorkerCommand::new(build_ffprobe_worker_binary())
}

fn build_ffprobe_worker_binary() -> PathBuf {
    let status = Command::new("cargo")
        .args([
            "build",
            "-q",
            "-p",
            "voom-ffprobe-worker",
            "--bin",
            "voom-ffprobe-worker",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "failed to build voom-ffprobe-worker");
    target_debug_dir().join("voom-ffprobe-worker")
}

fn target_debug_dir() -> PathBuf {
    let current_exe = std::env::current_exe().unwrap();
    let exe_dir = current_exe.parent().unwrap();
    if exe_dir.file_name() == Some(OsStr::new("deps")) {
        return exe_dir.parent().unwrap().to_path_buf();
    }
    exe_dir.to_path_buf()
}

fn write_fake_ffprobe(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("ffprobe");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version test-helper Copyright\\n'; exit 0; fi\n\
             {body}"
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn assert_process_exited(pid: &str) {
    let status = std::process::Command::new("kill")
        .arg("-0")
        .arg(pid)
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "child process {pid} still exists");
}

async fn wait_for_pid_file(path: &Path) -> String {
    for _ in 0..1_000 {
        if let Ok(pid) = std::fs::read_to_string(path) {
            return pid;
        }
        tokio::task::yield_now().await;
    }
    panic!("worker did not create pid file {}", path.display());
}
