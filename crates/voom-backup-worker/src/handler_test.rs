use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    BackUpFileRequest, BackUpFileResult, BackUpFileStatus, OperationKind, OperationRequest,
    ProgressFrame, ProtocolError,
};

use super::*;

fn request_for(source: &std::path::Path, destination: &std::path::Path) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::BackUpFile,
        lease_id: LeaseId(42),
        payload: serde_json::to_value(BackUpFileRequest {
            source_path: source.to_string_lossy().into_owned(),
            destination_path: destination.to_string_lossy().into_owned(),
        })
        .unwrap(),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

#[tokio::test]
async fn success_emits_progress_then_backed_up_result() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mkv");
    tokio::fs::write(&source, b"backup me").await.unwrap();
    let destination = dir.path().join("backups/1/movie.mkv");

    let frames = dispatch_frames(
        handle_operation(request_for(&source, &destination))
            .await
            .unwrap(),
    );

    assert!(matches!(
        frames.first(),
        Some(ProgressFrame::Progress {
            seq: 0,
            message: Some(message),
            ..
        }) if message == "backup started"
    ));
    let payload = match frames.get(1) {
        Some(ProgressFrame::Result {
            seq: 1, payload, ..
        }) => payload,
        other => panic!("expected terminal result frame, got {other:?}"),
    };
    let result: BackUpFileResult = serde_json::from_value(payload.clone()).unwrap();
    assert_eq!(result.status, BackUpFileStatus::BackedUp);
    assert_eq!(result.provider, "voom-backup-worker");
    assert_eq!(result.size_bytes, 9);
    assert_eq!(result.destination_path, destination.to_string_lossy());
}

#[tokio::test]
async fn missing_source_emits_terminal_artifact_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("missing.mkv");
    let destination = dir.path().join("backups/1/missing.mkv");

    let frames = dispatch_frames(
        handle_operation(request_for(&source, &destination))
            .await
            .unwrap(),
    );

    assert_terminal_error(
        frames.last().unwrap(),
        FailureClass::ArtifactUnavailable,
        ErrorCode::ArtifactUnavailable,
    );
}

#[tokio::test]
async fn mismatched_existing_destination_emits_terminal_backup_failure() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mkv");
    tokio::fs::write(&source, b"fresh").await.unwrap();
    let destination = dir.path().join("backups/1/movie.mkv");
    tokio::fs::create_dir_all(destination.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&destination, b"stale").await.unwrap();

    let frames = dispatch_frames(
        handle_operation(request_for(&source, &destination))
            .await
            .unwrap(),
    );

    assert_terminal_error(
        frames.last().unwrap(),
        FailureClass::BackupFailure,
        ErrorCode::BackupFailure,
    );
}

#[tokio::test]
async fn malformed_request_payload_is_terminal_malformed_worker_result() {
    let request = OperationRequest {
        operation: OperationKind::BackUpFile,
        lease_id: LeaseId(42),
        payload: serde_json::json!({ "source_path": 12 }),
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
        panic!("backup worker should buffer test responses");
    };
    body.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
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
