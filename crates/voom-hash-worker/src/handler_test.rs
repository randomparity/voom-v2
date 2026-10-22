use std::io::BufRead;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;
use voom_core::{FailureClass, LeaseId};
use voom_worker_protocol::{
    ClientHandle, HttpClient, HttpServer, NdjsonOutcome, ServerHandle, WorkerCredentials,
};
use voom_worker_protocol::{OperationKind, OperationRequest, ProgressFrame, http::OperationBody};

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

    let dispatch = run(request(OperationKind::HashFile, payload.clone()))
        .await
        .unwrap();

    assert!(
        matches!(&dispatch.body, OperationBody::Streaming(_)),
        "valid file work must use a live response, not wait for a buffered completion"
    );
    let credentials = WorkerCredentials {
        worker_id: voom_core::WorkerId(7),
        worker_epoch: 3,
        secret: secrecy::SecretString::from("test-secret"),
    };
    let server = HttpServer::new(credentials.clone(), operation_handler())
        .serve(([127, 0, 0, 1], 0).into())
        .await
        .unwrap();
    let mut dispatch = HttpClient::new(server.bound)
        .dispatch(
            &credentials,
            "real-hash",
            request(OperationKind::HashFile, payload),
        )
        .await
        .unwrap();
    let result = loop {
        match dispatch.frames.next_frame().await.unwrap() {
            NdjsonOutcome::Frame(_) => {}
            NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => break payload,
            other => panic!("expected hash result, got {other:?}"),
        }
    };
    let decoded: HashFileResult = serde_json::from_value(result).unwrap();
    assert_eq!(
        decoded.content_hash,
        format!("blake3:{}", blake3::hash(bytes).to_hex())
    );
    assert_eq!(decoded.size_bytes, u64::try_from(bytes.len()).unwrap());
    server.shutdown.send(()).unwrap();
    server.joined.await.unwrap();
}

// The pending future is injected at the file-work boundary: this verifies real HTTP
// streaming and replay, while real-file integrity is covered by the worker tests.
#[tokio::test]
async fn pending_work_acknowledges_and_progresses_then_replays_terminal() {
    for fail in [false, true] {
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = held_operation_handler(release.clone(), calls.clone(), fail);
        let credentials = WorkerCredentials {
            worker_id: voom_core::WorkerId(7),
            worker_epoch: 3,
            secret: secrecy::SecretString::from("test-secret"),
        };
        let server = HttpServer::new(credentials.clone(), handler)
            .serve(([127, 0, 0, 1], 0).into())
            .await
            .unwrap();
        let client =
            HttpClient::with_timeouts(server.bound, Duration::from_secs(5), Duration::from_secs(5));
        let req = OperationRequest {
            operation: OperationKind::HashFile,
            lease_id: voom_core::LeaseId(42),
            payload: serde_json::json!({}),
            heartbeat_deadline_ms: 2_000,
            progress_idle_deadline_ms: 2_000,
        };
        let mut dispatch = client
            .dispatch(&credentials, "held-work", req.clone())
            .await
            .unwrap();
        let mut observed = Vec::new();
        for seq in 0..3 {
            let outcome =
                tokio::time::timeout(Duration::from_secs(2), dispatch.frames.next_frame())
                    .await
                    .expect("progress must arrive while work remains pending")
                    .unwrap();
            let NdjsonOutcome::Frame(frame) = outcome else {
                panic!("premature terminal")
            };
            assert_eq!(frame.seq(), seq);
            observed.push(frame);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.notify_one();
        loop {
            let outcome =
                tokio::time::timeout(Duration::from_secs(2), dispatch.frames.next_frame())
                    .await
                    .unwrap()
                    .unwrap();
            match outcome {
                NdjsonOutcome::Frame(frame) => observed.push(frame),
                NdjsonOutcome::Terminated(frame) => {
                    assert_eq!(matches!(&frame, ProgressFrame::Error { .. }), fail);
                    assert_eq!(frame.seq(), u64::try_from(observed.len()).unwrap());
                    observed.push(frame);
                    break;
                }
                NdjsonOutcome::StreamEnd => panic!("missing terminal"),
            }
        }
        let mut replay = client
            .dispatch(&credentials, "held-work", req)
            .await
            .unwrap();
        for expected in observed {
            let outcome = replay.frames.next_frame().await.unwrap();
            let frame = match outcome {
                NdjsonOutcome::Frame(frame) | NdjsonOutcome::Terminated(frame) => frame,
                NdjsonOutcome::StreamEnd => panic!("truncated replay"),
            };
            assert_eq!(frame, expected);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "completed replay must not repeat work"
        );
        server.shutdown.send(()).unwrap();
        server.joined.await.unwrap();
    }
}

fn sample_result() -> HashFileResult {
    HashFileResult {
        content_hash: format!("blake3:{}", blake3::hash(b"fixture").to_hex()),
        size_bytes: 7,
        modified_at: "2026-10-22T00:00:00Z".into(),
        file_key: None,
        stability_started_at: "2026-10-22T00:00:00Z".into(),
        stability_confirmed_at: "2026-10-22T00:00:00Z".into(),
        sidecars: vec![],
    }
}

fn held_operation_handler(
    release: Arc<Notify>,
    calls: Arc<AtomicUsize>,
    fail: bool,
) -> OperationHandler {
    Arc::new(move |req| {
        let release = release.clone();
        let calls = calls.clone();
        Box::pin(async move {
            stream_operation(
                req.lease_id,
                OffsetDateTime::now_utc(),
                req.progress_idle_deadline_ms,
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    release.notified().await;
                    if fail {
                        Err(malformed_worker_result(
                            "controlled_error",
                            "fixture failure".into(),
                        ))
                    } else {
                        Ok(sample_result())
                    }
                },
            )
        })
    })
}
