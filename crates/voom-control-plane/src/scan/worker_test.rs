use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use secrecy::SecretString;
use voom_core::{ErrorCode, FailureClass, LeaseId, WorkerId};
use voom_worker_protocol::{
    ClientHandle, ExpectedFileFacts, HttpClient, HttpServer, OperationDispatch, OperationFuture,
    OperationHandler, OperationKind, OperationRequest, OperationResponse, ProbeFileRequest,
    ProgressFrame, ProtocolError, ServerHandle, WorkerCredentials,
};

use super::*;

#[tokio::test]
async fn launch_uses_caller_supplied_worker_id_and_dispatches_probe_file() {
    let dir = tempfile::tempdir().unwrap();
    let media_path = write_media_file(dir.path());
    let ffprobe = write_fake_ffprobe(
        dir.path(),
        "printf '{\"format\":{\"format_name\":\"matroska\"},\"streams\":[]}\\n'\n",
    );
    let worker_id = WorkerId(44);
    let command = ffprobe_worker_command().env("VOOM_FFPROBE_BIN", ffprobe.as_os_str());

    let mut worker = BundledWorkerProcess::launch(worker_id, command)
        .await
        .unwrap();

    assert_eq!(worker.worker_id, worker_id);
    assert_eq!(worker.credentials.worker_id, worker_id);
    let handshake = worker.client.handshake(voom_core::PROTOCOL_VERSION).await;
    assert!(handshake.is_ok());
    assert_worker_rejects_different_presented_id(&worker).await;
    let result = worker
        .dispatch_probe_file(probe_file_request(&media_path))
        .await
        .unwrap();

    assert_eq!(result.provider, "ffprobe");
    assert_eq!(result.status, voom_worker_protocol::ProbeFileStatus::Probed);
    assert_eq!(
        result.pre_probe.content_hash,
        result.post_probe.content_hash
    );
    worker.shutdown(Duration::from_secs(5)).await.unwrap();
}

async fn assert_worker_rejects_different_presented_id(worker: &BundledWorkerProcess) {
    let mut wrong_credentials = worker.credentials.clone();
    wrong_credentials.worker_id = WorkerId(worker.worker_id.0 + 1);
    let err = worker
        .client
        .dispatch(
            &wrong_credentials,
            "wrong-presented-worker-id",
            OperationRequest {
                operation: OperationKind::ProbeFile,
                lease_id: LeaseId(1),
                payload: serde_json::json!({"path": "/tmp/movie.mkv"}),
                heartbeat_deadline_ms: 1_000,
                progress_idle_deadline_ms: 1_000,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::UnknownWorkerId { presented } if presented == wrong_credentials.worker_id
    ));
}

#[tokio::test]
async fn worker_terminal_error_becomes_scan_worker_error() {
    let dir = tempfile::tempdir().unwrap();
    let media_path = write_media_file(dir.path());
    let missing_ffprobe = dir.path().join("missing-ffprobe");
    let command = ffprobe_worker_command().env("VOOM_FFPROBE_BIN", missing_ffprobe.as_os_str());
    let mut worker = BundledWorkerProcess::launch(WorkerId(45), command)
        .await
        .unwrap();

    let err = worker
        .dispatch_probe_file(probe_file_request(&media_path))
        .await
        .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::ExternalSystemUnavailable);
    assert_eq!(err.error_code(), ErrorCode::ExternalSystemUnavailable);
    worker.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn consecutive_dispatches_use_distinct_nonzero_protocol_lease_ids() {
    let credentials = WorkerCredentials {
        worker_id: WorkerId(9),
        worker_epoch: 0,
        secret: SecretString::from("lease-secret"),
    };
    let seen = Arc::new(Mutex::new(Vec::<LeaseId>::new()));
    let server = HttpServer::new(credentials.clone(), capture_lease_handler(seen.clone()));
    let running = server
        .serve(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = HttpClient::new(running.bound);
    let request = probe_file_request(Path::new("/tmp/movie.mkv"));

    dispatch_probe_file_with_client(&client, &credentials, request.clone())
        .await
        .unwrap();
    dispatch_probe_file_with_client(&client, &credentials, request)
        .await
        .unwrap();

    let leases = seen.lock().unwrap().clone();
    assert_eq!(leases.len(), 2);
    assert_ne!(leases[0], LeaseId(0));
    assert_ne!(leases[1], LeaseId(0));
    assert_ne!(leases[0], leases[1]);
    let _send = running.shutdown.send(());
    running.joined.await.unwrap();
}

#[tokio::test]
async fn launch_timeout_reaps_child_that_never_prints_bound_address() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("worker.pid");
    let script = format!("printf '%s' $$ > '{}'; exec sleep 60", pid_file.display());
    let command = WorkerCommand::new("/bin/sh").arg("-c").arg(script);

    let started = std::time::Instant::now();
    let err = BundledWorkerProcess::launch(WorkerId(46), command)
        .await
        .unwrap_err();

    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(err.failure_class(), FailureClass::WorkerCrash);
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    assert_process_exited(pid.trim());
}

#[test]
fn dispatch_setup_protocol_failures_are_worker_crashes() {
    for detail in [
        "missing response/body separator",
        "response read: unexpected end of file",
        "response decode: expected value at line 1 column 1",
    ] {
        let err = map_dispatch_protocol_error(&ProtocolError::MalformedFrame {
            detail: detail.to_owned(),
        });

        assert_eq!(err.failure_class(), FailureClass::WorkerCrash);
        assert_eq!(err.error_code(), ErrorCode::WorkerCrash);
    }
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
fn default_ffprobe_worker_command_uses_sibling_ffprobe_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let current_exe = dir.path().join("voom");
    let worker = dir.path().join("voom-ffprobe-worker");
    let ffprobe = dir.path().join("ffprobe");
    std::fs::write(&worker, b"").unwrap();
    std::fs::write(&ffprobe, b"").unwrap();

    let command = bundled_ffprobe_command_from(None, Ok(current_exe));

    assert_eq!(command.program, worker.as_os_str());
    assert_eq!(
        command.env,
        vec![(OsString::from("VOOM_FFPROBE_BIN"), ffprobe.into_os_string())]
    );
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
    assert_eq!(
        command.env,
        vec![(OsString::from("VOOM_FFPROBE_BIN"), ffprobe.into_os_string())]
    );
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

fn capture_lease_handler(seen: Arc<Mutex<Vec<LeaseId>>>) -> OperationHandler {
    Arc::new(move |req: OperationRequest| {
        let seen = seen.clone();
        Box::pin(async move {
            seen.lock().unwrap().push(req.lease_id);
            let now = Utc::now();
            let result = ProgressFrame::Result {
                lease_id: req.lease_id,
                seq: 0,
                emitted_at: now,
                payload: serde_json::to_value(probe_file_result()).unwrap(),
            };
            Ok(OperationDispatch::buffered(
                OperationResponse {
                    lease_id: req.lease_id,
                    accepted_at: now,
                },
                frame_body(&[result]),
            ))
        }) as OperationFuture
    })
}

fn probe_file_request(path: &Path) -> ProbeFileRequest {
    let expected = if path.exists() {
        let bytes = std::fs::read(path).unwrap();
        ExpectedFileFacts {
            size_bytes: u64::try_from(bytes.len()).unwrap(),
            content_hash: format!("blake3:{}", blake3::hash(&bytes).to_hex()),
            modified_at: None,
            local_file_key: None,
        }
    } else {
        ExpectedFileFacts {
            size_bytes: 0,
            content_hash: "blake3:test".to_owned(),
            modified_at: None,
            local_file_key: None,
        }
    };
    ProbeFileRequest {
        path: path.to_string_lossy().into_owned(),
        expected,
    }
}

fn probe_file_result() -> voom_worker_protocol::ProbeFileResult {
    let facts = voom_worker_protocol::ObservedFileFacts {
        size_bytes: 0,
        content_hash: "blake3:test".to_owned(),
        modified_at: None,
        local_file_key: None,
    };
    voom_worker_protocol::ProbeFileResult {
        status: voom_worker_protocol::ProbeFileStatus::Probed,
        provider: "test-worker".to_owned(),
        provider_version: "test".to_owned(),
        pre_probe: facts.clone(),
        post_probe: facts,
        snapshot: serde_json::json!({"ok": true}),
    }
}

fn frame_body(frames: &[ProgressFrame]) -> Vec<u8> {
    let mut body = Vec::new();
    for frame in frames {
        body.extend_from_slice(&serde_json::to_vec(frame).unwrap());
        body.push(b'\n');
    }
    body
}

fn write_media_file(dir: &Path) -> PathBuf {
    let path = dir.join("movie.mkv");
    std::fs::write(&path, b"not real media, fake ffprobe ignores it").unwrap();
    path
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
