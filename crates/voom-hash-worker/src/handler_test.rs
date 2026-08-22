use std::io::BufRead;
use voom_core::{FailureClass, LeaseId};
use voom_worker_protocol::{
    HashFileResult, OperationKind, OperationRequest, ProgressFrame, http::OperationBody,
};

use super::*;

fn request(operation: OperationKind, payload: serde_json::Value) -> OperationRequest {
    OperationRequest {
        operation,
        lease_id: LeaseId(42),
        payload,
        heartbeat_deadline_ms: 30_000,
        progress_idle_deadline_ms: 30_000,
    }
}

async fn run(req: OperationRequest) -> Result<OperationDispatch, ProtocolError> {
    let handler = operation_handler();
    let future = handler(req);
    Box::pin(future).await
}

fn decode_frames(body: &[u8]) -> Vec<ProgressFrame> {
    let mut frames = Vec::new();
    let cursor = std::io::Cursor::new(body);
    for line in cursor.lines() {
        frames.push(serde_json::from_str(&line.unwrap()).unwrap());
    }
    frames
}

#[tokio::test]
async fn rejects_non_hash_file_operations_as_protocol_errors() {
    // A wrong operation kind is a wire-contract violation, not work output:
    // it must surface as ProtocolError, not as a terminal frame.
    let err = run(request(OperationKind::ScanLibrary, serde_json::json!({})))
        .await
        .unwrap_err();

    assert!(
        matches!(&err, ProtocolError::UnknownOperation { name } if name.contains("ScanLibrary")),
        "expected UnknownOperation, got: {err}"
    );
}

#[tokio::test]
async fn malformed_payload_becomes_terminal_malformed_worker_result_frame() {
    // Missing provider_relative_locator. Malformed payloads must stay on
    // HTTP 200 as terminal worker results so retries replay the same
    // outcome instead of evicting the idempotency row.
    let payload = serde_json::json!({ "provider_locator": "/tmp/does-not-matter" });

    let dispatch = run(request(OperationKind::HashFile, payload))
        .await
        .unwrap();

    let OperationBody::Buffered(body) = &dispatch.body else {
        panic!("hash worker emits buffered dispatches");
    };
    let frames = decode_frames(body);
    assert_eq!(frames.len(), 1, "exactly one terminal error frame");
    assert_eq!(frames.first().unwrap().lease_id(), LeaseId(42));
    match &frames[0] {
        ProgressFrame::Error {
            class,
            code,
            message,
            payload,
            ..
        } => {
            assert_eq!(*class, FailureClass::MalformedWorkerResult);
            assert_eq!(*code, voom_core::ErrorCode::MalformedWorkerResult);
            assert!(message.contains("payload decode"), "{message}");
            assert_eq!(
                payload.as_ref(),
                Some(&serde_json::json!({ "stage": "decode_request" })),
                "stage marker rides in the payload"
            );
        }
        other => panic!("expected Error frame, got: {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn happy_path_dispatch_carries_progress_then_result_frame() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"dispatch-level fixture";
    std::fs::write(dir.path().join("f.bin"), bytes).unwrap();
    let payload = serde_json::json!({
        "provider_locator": dir.path().display().to_string(),
        "provider_relative_locator": "f.bin",
    });

    let dispatch = run(request(OperationKind::HashFile, payload))
        .await
        .unwrap();

    let OperationBody::Buffered(body) = &dispatch.body else {
        panic!("hash worker emits buffered dispatches");
    };
    let frames = decode_frames(body);
    assert_eq!(frames.len(), 2);
    assert!(matches!(
        frames.first(),
        Some(ProgressFrame::Progress { .. })
    ));
    let ProgressFrame::Result { payload, .. } = frames.last().unwrap() else {
        panic!("terminal frame must be Result");
    };
    let decoded: HashFileResult = serde_json::from_value(payload.clone()).unwrap();
    let expected_hash = blake3::hash(bytes).to_hex().to_string();
    assert_eq!(decoded.content_hash, format!("blake3:{expected_hash}"));
    assert_eq!(decoded.size_bytes, bytes.len() as u64);
}
