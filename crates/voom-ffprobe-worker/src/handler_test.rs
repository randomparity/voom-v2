#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests use direct assertions and bounded fixture setup"
)]
use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;
use voom_worker_protocol::{
    ClientHandle, HttpClient, HttpServer, NdjsonOutcome, ServerHandle, WorkerCredentials,
};

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
            operation: OperationKind::ProbeFile,
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

fn sample_result() -> ProbeFileResult {
    let facts = voom_worker_protocol::ObservedFileFacts {
        size_bytes: 7,
        content_hash: format!("blake3:{}", blake3::hash(b"fixture").to_hex()),
        modified_at: None,
        local_file_key: None,
    };
    ProbeFileResult {
        status: ProbeFileStatus::Probed,
        provider: "ffprobe".into(),
        provider_version: "test".into(),
        pre_probe: facts.clone(),
        post_probe: facts,
        snapshot: serde_json::json!({}),
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
