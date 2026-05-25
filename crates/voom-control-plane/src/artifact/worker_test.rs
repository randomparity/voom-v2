use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use secrecy::SecretString;
use voom_core::{ErrorCode, FailureClass, LeaseId, WorkerId};
use voom_worker_protocol::{
    ClientHandle, HttpServer, OperationDispatch, OperationFuture, OperationHandler, OperationKind,
    OperationRequest, OperationResponse, ProgressFrame, ProtocolError, ServerHandle,
    VerifyArtifactExpectedFacts, VerifyArtifactRequest, VerifyArtifactStatus, WorkerCredentials,
};

use super::*;

#[tokio::test]
async fn launch_uses_caller_supplied_worker_id_and_dispatches_verify_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let artifact_path = write_artifact_file(dir.path(), b"verified bytes");
    let worker_id = WorkerId(144);

    let mut worker = BundledWorkerProcess::launch(worker_id, verify_worker_command())
        .await
        .unwrap();

    assert_eq!(worker.worker_id, worker_id);
    assert_eq!(worker.credentials.worker_id, worker_id);
    let handshake = worker.client.handshake(voom_core::PROTOCOL_VERSION).await;
    assert!(handshake.is_ok());
    assert_worker_rejects_different_presented_id(&worker).await;
    let result = worker
        .dispatch_verify_artifact(verify_request(&artifact_path, b"verified bytes"))
        .await
        .unwrap();

    assert_eq!(result.status, VerifyArtifactStatus::Verified);
    assert_eq!(
        result.observed.content_hash,
        blake3_checksum(b"verified bytes")
    );
    worker.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn worker_terminal_error_becomes_verify_worker_error() {
    let dir = tempfile::tempdir().unwrap();
    let artifact_path = write_artifact_file(dir.path(), b"changed bytes");
    let mut worker = BundledWorkerProcess::launch(WorkerId(145), verify_worker_command())
        .await
        .unwrap();

    let err = worker
        .dispatch_verify_artifact(verify_request(&artifact_path, b"expected bytes"))
        .await
        .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::ArtifactChecksumMismatch);
    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    worker.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn malformed_request_payload_is_terminal_worker_domain_error() {
    let credentials = WorkerCredentials {
        worker_id: WorkerId(146),
        worker_epoch: 0,
        secret: SecretString::from("verify-secret"),
    };
    let server = HttpServer::new(credentials.clone(), malformed_request_handler());
    let running = server
        .serve(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = voom_worker_protocol::HttpClient::new(running.bound);
    let dispatch = client
        .dispatch(
            &credentials,
            "malformed-request",
            OperationRequest {
                operation: OperationKind::VerifyArtifact,
                lease_id: LeaseId(1),
                payload: serde_json::json!({"path": "/tmp/staged.bin"}),
                heartbeat_deadline_ms: 1_000,
                progress_idle_deadline_ms: 1_000,
            },
        )
        .await
        .unwrap();

    let err = consume_verify_artifact_stream(dispatch, Duration::from_secs(5))
        .await
        .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    let _send = running.shutdown.send(());
    running.joined.await.unwrap();
}

#[tokio::test]
async fn unsupported_operation_is_protocol_error_unknown_operation() {
    let credentials = WorkerCredentials {
        worker_id: WorkerId(147),
        worker_epoch: 0,
        secret: SecretString::from("verify-secret"),
    };
    let seen = Arc::new(Mutex::new(Vec::<OperationKind>::new()));
    let server = HttpServer::new(credentials.clone(), unknown_operation_handler(seen.clone()));
    let running = server
        .serve(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = voom_worker_protocol::HttpClient::new(running.bound);

    let err = client
        .dispatch(
            &credentials,
            "unsupported-operation",
            OperationRequest {
                operation: OperationKind::ProbeFile,
                lease_id: LeaseId(1),
                payload: serde_json::json!({"path": "/tmp/staged.bin"}),
                heartbeat_deadline_ms: 1_000,
                progress_idle_deadline_ms: 1_000,
            },
        )
        .await
        .unwrap_err();

    assert!(matches!(err, ProtocolError::UnknownOperation { .. }));
    assert_eq!(seen.lock().unwrap().as_slice(), &[OperationKind::ProbeFile]);
    let _send = running.shutdown.send(());
    running.joined.await.unwrap();
}

#[tokio::test]
async fn malformed_result_payload_is_verify_worker_error() {
    let credentials = WorkerCredentials {
        worker_id: WorkerId(148),
        worker_epoch: 0,
        secret: SecretString::from("verify-secret"),
    };
    let server = HttpServer::new(credentials.clone(), malformed_result_handler());
    let running = server
        .serve(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = voom_worker_protocol::HttpClient::new(running.bound);

    let err = dispatch_verify_artifact_with_client(
        &client,
        &credentials,
        verify_request(Path::new("/tmp/staged.bin"), b"expected bytes"),
    )
    .await
    .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    let _send = running.shutdown.send(());
    running.joined.await.unwrap();
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
                operation: OperationKind::VerifyArtifact,
                lease_id: LeaseId(1),
                payload: serde_json::json!({"path": "/tmp/staged.bin"}),
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

fn malformed_request_handler() -> OperationHandler {
    Arc::new(|req: OperationRequest| {
        Box::pin(async move {
            let now = Utc::now();
            let frame = ProgressFrame::Error {
                lease_id: req.lease_id,
                seq: 0,
                emitted_at: now,
                class: FailureClass::MalformedWorkerResult,
                code: ErrorCode::MalformedWorkerResult,
                message: "malformed worker result: verify_artifact payload decode".to_owned(),
                payload: None,
            };
            Ok(OperationDispatch::buffered(
                OperationResponse {
                    lease_id: req.lease_id,
                    accepted_at: now,
                },
                frame_body(&[frame]),
            ))
        }) as OperationFuture
    })
}

fn unknown_operation_handler(seen: Arc<Mutex<Vec<OperationKind>>>) -> OperationHandler {
    Arc::new(move |req: OperationRequest| {
        let seen = seen.clone();
        Box::pin(async move {
            seen.lock().unwrap().push(req.operation);
            if req.operation != OperationKind::VerifyArtifact {
                return Err(ProtocolError::UnknownOperation {
                    name: format!("{:?}", req.operation),
                });
            }
            unreachable!("test dispatches only unsupported operation")
        }) as OperationFuture
    })
}

fn malformed_result_handler() -> OperationHandler {
    Arc::new(|req: OperationRequest| {
        Box::pin(async move {
            let now = Utc::now();
            let frame = ProgressFrame::Result {
                lease_id: req.lease_id,
                seq: 0,
                emitted_at: now,
                payload: serde_json::json!({
                    "status": "verified",
                    "provider": "bad-test-worker"
                }),
            };
            Ok(OperationDispatch::buffered(
                OperationResponse {
                    lease_id: req.lease_id,
                    accepted_at: now,
                },
                frame_body(&[frame]),
            ))
        }) as OperationFuture
    })
}

fn verify_worker_command() -> WorkerCommand {
    if let Some(binary) = std::env::var_os("CARGO_BIN_EXE_voom-verify-artifact-worker") {
        return WorkerCommand::new(binary);
    }
    WorkerCommand::new(build_verify_worker_binary())
}

fn build_verify_worker_binary() -> PathBuf {
    let status = Command::new("cargo")
        .args([
            "build",
            "-q",
            "-p",
            "voom-verify-artifact-worker",
            "--bin",
            "voom-verify-artifact-worker",
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "failed to build voom-verify-artifact-worker"
    );
    target_debug_dir().join("voom-verify-artifact-worker")
}

fn target_debug_dir() -> PathBuf {
    let current_exe = std::env::current_exe().unwrap();
    let exe_dir = current_exe.parent().unwrap();
    if exe_dir.file_name() == Some(OsStr::new("deps")) {
        return exe_dir.parent().unwrap().to_path_buf();
    }
    exe_dir.to_path_buf()
}

fn verify_request(path: &Path, expected_bytes: &[u8]) -> VerifyArtifactRequest {
    VerifyArtifactRequest {
        path: path.to_string_lossy().into_owned(),
        expected: VerifyArtifactExpectedFacts {
            size_bytes: u64::try_from(expected_bytes.len()).unwrap(),
            content_hash: blake3_checksum(expected_bytes),
            modified_at: None,
            local_file_key: None,
        },
    }
}

fn write_artifact_file(dir: &Path, bytes: &[u8]) -> PathBuf {
    let path = dir.join("staged.bin");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn frame_body(frames: &[ProgressFrame]) -> Vec<u8> {
    let mut body = Vec::new();
    for frame in frames {
        body.extend_from_slice(&serde_json::to_vec(frame).unwrap());
        body.push(b'\n');
    }
    body
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
