use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    OperationKind, OperationRequest, ProgressFrame, ProtocolError, VerifyArtifactExpectedFacts,
    VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

use super::*;

#[tokio::test]
async fn success_emits_progress_then_verify_artifact_result() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("artifact.bin");
    tokio::fs::write(&path, b"artifact bytes").await.unwrap();
    let request = verify_request(&path).await;

    let frames = dispatch_frames(handle_operation(request).await.unwrap());

    assert!(matches!(
        frames.first(),
        Some(ProgressFrame::Progress {
            seq: 0,
            message: Some(message),
            ..
        }) if message == "artifact verification started"
    ));
    let payload = match frames.get(1) {
        Some(ProgressFrame::Result {
            seq: 1, payload, ..
        }) => payload,
        other => panic!("expected terminal result frame, got {other:?}"),
    };
    let result: VerifyArtifactResult = serde_json::from_value(payload.clone()).unwrap();
    assert_eq!(result.status, VerifyArtifactStatus::Verified);
    assert_eq!(result.provider, "voom-verify-artifact-worker");
    assert_eq!(result.observed.size_bytes, 14);
}

#[tokio::test]
async fn missing_file_emits_terminal_artifact_unavailable_error() {
    let dir = tempfile::tempdir().unwrap();
    let request = OperationRequest {
        operation: OperationKind::VerifyArtifact,
        lease_id: LeaseId(42),
        payload: serde_json::to_value(VerifyArtifactRequest {
            path: dir
                .path()
                .join("missing.bin")
                .to_string_lossy()
                .into_owned(),
            expected: VerifyArtifactExpectedFacts {
                size_bytes: 1,
                content_hash: format!("blake3:{}", blake3::hash(b"x").to_hex()),
                modified_at: None,
                local_file_key: None,
            },
        })
        .unwrap(),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let frames = dispatch_frames(handle_operation(request).await.unwrap());

    assert_terminal_error(
        frames.last().unwrap(),
        FailureClass::ArtifactUnavailable,
        ErrorCode::ArtifactUnavailable,
    );
}

#[tokio::test]
async fn size_mismatch_emits_terminal_checksum_mismatch_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("artifact.bin");
    tokio::fs::write(&path, b"artifact bytes").await.unwrap();
    let mut request = verify_request(&path).await;
    request.payload["expected"]["size_bytes"] = serde_json::json!(15);

    let frames = dispatch_frames(handle_operation(request).await.unwrap());

    assert_terminal_error(
        frames.last().unwrap(),
        FailureClass::ArtifactChecksumMismatch,
        ErrorCode::ArtifactChecksumMismatch,
    );
}

#[tokio::test]
async fn hash_mismatch_emits_terminal_checksum_mismatch_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("artifact.bin");
    tokio::fs::write(&path, b"artifact bytes").await.unwrap();
    let mut request = verify_request(&path).await;
    request.payload["expected"]["content_hash"] = serde_json::json!("blake3:bad");

    let frames = dispatch_frames(handle_operation(request).await.unwrap());

    assert_terminal_error(
        frames.last().unwrap(),
        FailureClass::ArtifactChecksumMismatch,
        ErrorCode::ArtifactChecksumMismatch,
    );
}

#[tokio::test]
async fn malformed_request_payload_is_accepted_then_terminal_malformed_worker_result() {
    let request = OperationRequest {
        operation: OperationKind::VerifyArtifact,
        lease_id: LeaseId(42),
        payload: serde_json::json!({"path": 12}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let frames = dispatch_frames(handle_operation(request).await.unwrap());

    assert_eq!(frames.len(), 1);
    assert_terminal_error(
        frames.last().unwrap(),
        FailureClass::MalformedWorkerResult,
        ErrorCode::MalformedWorkerResult,
    );
}

#[tokio::test]
async fn unsupported_operation_returns_unknown_operation_protocol_error() {
    let request = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: LeaseId(42),
        payload: serde_json::Value::Null,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let err = handle_operation(request).await.unwrap_err();

    assert!(matches!(err, ProtocolError::UnknownOperation { .. }));
}

fn dispatch_frames(dispatch: voom_worker_protocol::OperationDispatch) -> Vec<ProgressFrame> {
    let voom_worker_protocol::http::OperationBody::Buffered(body) = dispatch.body else {
        panic!("verify artifact worker should buffer test responses");
    };
    body.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

async fn verify_request(path: &std::path::Path) -> OperationRequest {
    let observed = observe_file_facts(path).await.unwrap();
    let payload = serde_json::to_value(VerifyArtifactRequest {
        path: path.to_string_lossy().into_owned(),
        expected: VerifyArtifactExpectedFacts {
            size_bytes: observed.size_bytes,
            content_hash: observed.content_hash,
            modified_at: observed.modified_at,
            local_file_key: observed.local_file_key,
        },
    })
    .unwrap();
    OperationRequest {
        operation: OperationKind::VerifyArtifact,
        lease_id: LeaseId(42),
        payload,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

fn assert_terminal_error(frame: &ProgressFrame, class: FailureClass, code: ErrorCode) {
    let ProgressFrame::Error {
        class: actual_class,
        code: actual_code,
        message,
        payload,
        ..
    } = frame
    else {
        panic!("expected terminal error frame, got {frame:?}");
    };
    assert_eq!(*actual_class, class);
    assert_eq!(*actual_code, code);
    assert!(!message.trim().is_empty());
    assert!(payload.is_some());
}
