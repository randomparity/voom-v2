#![expect(
    clippy::expect_used,
    reason = "integration tests use expect for direct setup and process assertions"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use voom_core::{ErrorCode, FailureClass, LeaseId, WorkerId};
use voom_ffprobe_worker::{
    FFPROBE_BIN_ENV, FfprobeConfig, observe_file_facts, operation_handler_with_config,
};
use voom_worker_protocol::{
    ClientHandle, ExpectedFileFacts, HttpClient, HttpServer, NdjsonOutcome, OperationKind,
    OperationRequest, ProbeFileRequest, ProgressFrame, ProtocolError, ServerHandle, ServerRunning,
    WorkerCredentials,
};

const BASIC_FFPROBE_JSON: &str = include_str!("../fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn missing_ffprobe_returns_terminal_domain_error_over_http_success() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let media_path = write_wav(dir.path());
    let missing_ffprobe = dir.path().join("missing-ffprobe");
    let config = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, missing_ffprobe.as_os_str())]);

    let terminal = Box::pin(dispatch_terminal_frame(
        &media_path,
        config,
        "missing-ffprobe",
    ))
    .await;

    assert_terminal_error(
        terminal,
        FailureClass::ExternalSystemUnavailable,
        ErrorCode::ExternalSystemUnavailable,
    );
}

#[tokio::test]
async fn nonzero_ffprobe_returns_terminal_domain_error_over_http_success() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let media_path = write_wav(dir.path());
    let fake_ffprobe = write_fake_ffprobe(dir.path(), "printf 'boom\\n' >&2\nexit 42\n");
    let config = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, fake_ffprobe.as_os_str())]);

    let terminal = Box::pin(dispatch_terminal_frame(
        &media_path,
        config,
        "nonzero-ffprobe",
    ))
    .await;

    assert_terminal_error(
        terminal,
        FailureClass::ExternalSystemUnavailable,
        ErrorCode::ExternalSystemUnavailable,
    );
}

#[tokio::test]
async fn invalid_ffprobe_json_returns_terminal_domain_error_over_http_success() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let media_path = write_wav(dir.path());
    let fake_ffprobe = write_fake_ffprobe(dir.path(), "printf 'not-json\\n'\nexit 0\n");
    let config = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, fake_ffprobe.as_os_str())]);

    let terminal = Box::pin(dispatch_terminal_frame(&media_path, config, "invalid-json")).await;

    assert_terminal_error(
        terminal,
        FailureClass::MalformedWorkerResult,
        ErrorCode::MalformedWorkerResult,
    );
}

#[tokio::test]
async fn content_drift_returns_terminal_domain_error_over_http_success() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let media_path = write_wav(dir.path());
    let fake_ffprobe = write_fake_ffprobe(
        dir.path(),
        "last=''\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf drift >> \"$last\"\nprintf '{\"format\":{},\"streams\":[]}\\n'\nexit 0\n",
    );
    let config = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, fake_ffprobe.as_os_str())]);

    let terminal = Box::pin(dispatch_terminal_frame(
        &media_path,
        config,
        "content-drift",
    ))
    .await;

    assert_terminal_error(
        terminal,
        FailureClass::ArtifactChecksumMismatch,
        ErrorCode::ArtifactChecksumMismatch,
    );
}

#[tokio::test]
async fn ffprobe_success_returns_progress_and_probe_result() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let media_path = write_wav(dir.path());
    let fake_ffprobe = write_fake_ffprobe(
        dir.path(),
        &format!("cat <<'JSON'\n{BASIC_FFPROBE_JSON}\nJSON\nexit 0\n"),
    );
    let config = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, fake_ffprobe.as_os_str())]);
    let running_result = running_server(config).await;
    assert!(running_result.is_ok());
    let Ok((addr, running)) = running_result else {
        return;
    };
    let client = HttpClient::new(addr);
    let request = Box::pin(probe_request(&media_path, LeaseId(42))).await;

    let dispatch_result = client
        .dispatch(&credentials(), "ffprobe-success", request)
        .await;

    assert!(
        dispatch_result.is_ok(),
        "worker-domain success must be an HTTP-success dispatch"
    );
    let Ok(mut dispatch) = dispatch_result else {
        stop_server(running).await;
        return;
    };
    let first_result = dispatch.frames.next_frame().await;
    assert!(first_result.is_ok());
    let Ok(first) = first_result else {
        stop_server(running).await;
        return;
    };
    assert!(matches!(
        first,
        NdjsonOutcome::Frame(ProgressFrame::Progress { .. })
    ));
    let terminal_result = dispatch.frames.next_frame().await;
    assert!(terminal_result.is_ok());
    let Ok(terminal) = terminal_result else {
        stop_server(running).await;
        return;
    };
    let payload = match terminal {
        NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => payload,
        other => {
            assert!(
                matches!(
                    other,
                    NdjsonOutcome::Terminated(ProgressFrame::Result { .. })
                ),
                "expected terminal result frame"
            );
            stop_server(running).await;
            return;
        }
    };
    let parsed = serde_json::from_value::<voom_worker_protocol::ProbeFileResult>(payload);
    assert!(parsed.is_ok());
    let Ok(result) = parsed else {
        stop_server(running).await;
        return;
    };
    assert_eq!(result.provider, "ffprobe");
    assert_eq!(result.status, voom_worker_protocol::ProbeFileStatus::Probed);
    assert_eq!(result.pre_probe.size_bytes, result.post_probe.size_bytes);
    assert_eq!(
        result.pre_probe.content_hash,
        result.post_probe.content_hash
    );
    assert_eq!(result.snapshot["format"], "sprint10-v1");
    stop_server(running).await;
}

#[tokio::test]
async fn binary_prints_bound_address_and_stops_on_stdin_close() {
    let binary = env!("CARGO_BIN_EXE_voom-ffprobe-worker");
    let mut child = tokio::process::Command::new(binary)
        .env("VOOM_WORKER_ID", "7")
        .env("VOOM_WORKER_EPOCH", "3")
        .env("VOOM_WORKER_SECRET", "secret")
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("worker binary should spawn");

    let stdout = child.stdout.take().expect("worker stdout should be piped");
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .expect("worker should print bound address before timeout")
        .expect("worker stdout read should succeed")
        .expect("worker should print one stdout line");
    assert!(
        line.strip_prefix("BOUND addr=")
            .and_then(|addr| addr.parse::<std::net::SocketAddr>().ok())
            .is_some(),
        "unexpected worker bound line: {line}"
    );

    drop(child.stdin.take());
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("worker should stop after stdin closes")
        .expect("worker wait should succeed");

    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _read = pipe.read_to_string(&mut stderr).await;
        }
        assert!(status.success(), "worker exited {status}: {stderr}");
    }
}

#[tokio::test]
async fn malformed_payload_returns_terminal_domain_error_over_http_success() {
    let config =
        FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, std::ffi::OsStr::new("/nonexistent"))]);
    let (addr, running) = running_server(config).await.expect("server starts");
    let client = HttpClient::new(addr);

    // `path` must be a string; an integer makes ProbeFileRequest decoding
    // fail. The worker must answer HTTP 200 with a terminal
    // MalformedWorkerResult frame, not an HTTP 400 transport error.
    let request = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: LeaseId(42),
        payload: serde_json::json!({"path": 12}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };
    let dispatch_result = client
        .dispatch(&credentials(), "malformed-payload", request)
        .await;
    assert!(
        dispatch_result.is_ok(),
        "malformed payload must not become a transport error: {dispatch_result:?}"
    );
    let mut dispatch = dispatch_result.expect("dispatch ok");
    let terminal = dispatch.frames.next_frame().await.expect("terminal frame");
    stop_server(running).await;
    match terminal {
        NdjsonOutcome::Terminated(frame) => assert_terminal_error(
            frame,
            FailureClass::MalformedWorkerResult,
            ErrorCode::MalformedWorkerResult,
        ),
        other => assert!(
            matches!(other, NdjsonOutcome::Terminated(_)),
            "expected terminal error frame, got {other:?}"
        ),
    }
}

async fn dispatch_terminal_frame(
    media_path: &Path,
    config: FfprobeConfig,
    idempotency_key: &str,
) -> ProgressFrame {
    let running_result = running_server(config).await;
    assert!(running_result.is_ok());
    let Ok((addr, running)) = running_result else {
        return fallback_error_frame();
    };
    let client = HttpClient::new(addr);
    let request = Box::pin(probe_request(media_path, LeaseId(42))).await;
    let dispatch_result = client
        .dispatch(&credentials(), idempotency_key, request)
        .await;
    assert!(
        dispatch_result.is_ok(),
        "worker-domain failures must not become transport errors"
    );
    let Ok(mut dispatch) = dispatch_result else {
        stop_server(running).await;
        return fallback_error_frame();
    };
    let terminal_result = dispatch.frames.next_frame().await;
    assert!(terminal_result.is_ok());
    stop_server(running).await;
    let Ok(terminal) = terminal_result else {
        return fallback_error_frame();
    };
    match terminal {
        NdjsonOutcome::Terminated(frame) => frame,
        other => {
            assert!(
                matches!(other, NdjsonOutcome::Terminated(_)),
                "expected terminal frame"
            );
            fallback_error_frame()
        }
    }
}

fn assert_terminal_error(frame: ProgressFrame, class: FailureClass, code: ErrorCode) {
    let (actual_class, actual_code, message, payload) = match frame {
        ProgressFrame::Error {
            class,
            code,
            message,
            payload,
            ..
        } => (class, code, message, payload),
        other => {
            assert!(
                matches!(other, ProgressFrame::Error { .. }),
                "expected terminal error frame"
            );
            return;
        }
    };
    assert_eq!(actual_class, class);
    assert_eq!(actual_code, code);
    assert!(!message.trim().is_empty());
    assert!(payload.is_some());
}

async fn running_server(
    config: FfprobeConfig,
) -> Result<(std::net::SocketAddr, ServerRunning), ProtocolError> {
    let server = HttpServer::new(credentials(), operation_handler_with_config(config));
    let running = server
        .serve(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .await?;
    Ok((running.bound, running))
}

async fn stop_server(running: ServerRunning) {
    let _send_result = running.shutdown.send(());
    let _join_result = running.joined.await;
}

fn credentials() -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(7),
        worker_epoch: 3,
        secret: SecretString::from("secret"),
    }
}

async fn probe_request(path: &Path, lease_id: LeaseId) -> OperationRequest {
    let observed_result = Box::pin(observe_file_facts(path)).await;
    assert!(observed_result.is_ok());
    let Ok(observed) = observed_result else {
        return OperationRequest {
            operation: OperationKind::ProbeFile,
            lease_id,
            payload: serde_json::Value::Null,
            heartbeat_deadline_ms: 1_000,
            progress_idle_deadline_ms: 1_000,
        };
    };
    let request = ProbeFileRequest {
        path: path.to_string_lossy().into_owned(),
        expected: ExpectedFileFacts {
            size_bytes: observed.size_bytes,
            content_hash: observed.content_hash,
            modified_at: observed.modified_at,
            local_file_key: observed.local_file_key,
        },
    };
    let payload_result = serde_json::to_value(request);
    assert!(payload_result.is_ok());
    let Ok(payload) = payload_result else {
        return OperationRequest {
            operation: OperationKind::ProbeFile,
            lease_id,
            payload: serde_json::Value::Null,
            heartbeat_deadline_ms: 1_000,
            progress_idle_deadline_ms: 1_000,
        };
    };
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id,
        payload,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

fn write_wav(dir: &Path) -> PathBuf {
    let path = dir.join("tone.wav");
    let bytes = tiny_wav_bytes();
    let write_result = std::fs::write(&path, bytes);
    assert!(write_result.is_ok());
    path
}

fn tiny_wav_bytes() -> Vec<u8> {
    let samples: [i16; 8] = [0, 6000, 0, -6000, 0, 6000, 0, -6000];
    let data_len = u32::try_from(samples.len() * 2).unwrap_or(0);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8_000_u32.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn write_fake_ffprobe(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("ffprobe");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version test-helper Copyright\\n'; exit 0; fi\n\
         {body}"
    );
    let write_result = std::fs::write(&path, script);
    assert!(write_result.is_ok());
    let metadata_result = std::fs::metadata(&path);
    assert!(metadata_result.is_ok());
    let Ok(metadata) = metadata_result else {
        return path;
    };
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    let chmod_result = std::fs::set_permissions(&path, permissions);
    assert!(chmod_result.is_ok());
    path
}

fn fallback_error_frame() -> ProgressFrame {
    ProgressFrame::Error {
        lease_id: LeaseId(42),
        seq: 0,
        emitted_at: chrono::Utc::now(),
        class: FailureClass::WorkerCrash,
        code: ErrorCode::WorkerCrash,
        message: "test fallback".to_owned(),
        payload: None,
    }
}
