use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use hyper::StatusCode;
use secrecy::SecretString;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use voom_core::{LeaseId, WorkerId};

use super::*;
use crate::NdjsonOutcome;
use crate::{OperationKind, ProgressFrame};

fn creds() -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(7),
        worker_epoch: 3,
        secret: SecretString::from("secret"),
    }
}

fn request(lease_id: LeaseId, payload: serde_json::Value) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id,
        payload,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

fn fixed_time() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_779_192_000).unwrap()
}

fn progress(lease_id: LeaseId, seq: u64) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq,
        emitted_at: fixed_time(),
        percent: None,
        message: None,
        payload: None,
    }
}

fn result_frame(lease_id: LeaseId, seq: u64) -> ProgressFrame {
    ProgressFrame::Result {
        lease_id,
        seq,
        emitted_at: fixed_time(),
        payload: serde_json::json!({"ok": true}),
    }
}

#[test]
fn streaming_writer_rejects_second_terminal_frame() {
    let (mut writer, _dispatch) = OperationDispatch::streaming(OperationResponse {
        lease_id: LeaseId(1),
        accepted_at: fixed_time(),
    });
    // First terminal frame is accepted.
    writer.write_frame(&result_frame(LeaseId(1), 0)).unwrap();
    // A second terminal frame must be rejected, not appended — appending it
    // would concatenate two terminal frames into the buffered body and
    // corrupt the idempotency-cache entry on replay.
    let err = writer
        .write_frame(&result_frame(LeaseId(1), 1))
        .unwrap_err();
    assert!(
        matches!(err, ProtocolError::MalformedFrame { .. }),
        "expected MalformedFrame for a second terminal frame, got {err:?}"
    );
}

fn ndjson_bytes(frames: &[ProgressFrame]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(&serde_json::to_vec(frame).unwrap());
        bytes.push(b'\n');
    }
    bytes
}

fn operation_handler(calls: Arc<AtomicUsize>, response_lease: Option<LeaseId>) -> OperationHandler {
    Arc::new(move |req: OperationRequest| {
        let calls = calls.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let lease_id = req.lease_id;
            let response_lease = response_lease.unwrap_or(lease_id);
            Ok(OperationDispatch::buffered(
                OperationResponse {
                    lease_id: response_lease,
                    accepted_at: fixed_time(),
                },
                ndjson_bytes(&[progress(lease_id, 0), result_frame(lease_id, 1)]),
            ))
        })
    })
}

fn streaming_handler(calls: Arc<AtomicUsize>) -> OperationHandler {
    Arc::new(move |req: OperationRequest| {
        let calls = calls.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let (mut writer, dispatch) = OperationDispatch::streaming(OperationResponse {
                lease_id: req.lease_id,
                accepted_at: fixed_time(),
            });
            tokio::spawn(async move {
                writer.write_frame(&progress(req.lease_id, 0)).unwrap();
                writer.write_frame(&result_frame(req.lease_id, 1)).unwrap();
                writer.finish().unwrap();
            });
            Ok(dispatch)
        })
    })
}

fn slow_streaming_handler(calls: Arc<AtomicUsize>, gate: Arc<Notify>) -> OperationHandler {
    Arc::new(move |req: OperationRequest| {
        let calls = calls.clone();
        let gate = gate.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let (mut writer, dispatch) = OperationDispatch::streaming(OperationResponse {
                lease_id: req.lease_id,
                accepted_at: fixed_time(),
            });
            tokio::spawn(async move {
                writer.write_frame(&progress(req.lease_id, 0)).unwrap();
                gate.notified().await;
                writer.write_frame(&result_frame(req.lease_id, 1)).unwrap();
                writer.finish().unwrap();
            });
            Ok(dispatch)
        })
    })
}

#[derive(Debug, Default)]
struct WorkerCompletionGate {
    release: Notify,
    finished: Notify,
    is_finished: AtomicBool,
}

impl WorkerCompletionGate {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn release(&self) {
        self.release.notify_one();
    }

    fn mark_finished(&self) {
        self.is_finished.store(true, Ordering::SeqCst);
        self.finished.notify_waiters();
    }

    fn is_finished(&self) -> bool {
        self.is_finished.load(Ordering::SeqCst)
    }

    async fn wait_finished(&self) {
        while !self.is_finished() {
            self.finished.notified().await;
        }
    }
}

fn client_aborts_before_worker_finishes_handler(
    calls: Arc<AtomicUsize>,
    worker: Arc<WorkerCompletionGate>,
) -> OperationHandler {
    Arc::new(move |req: OperationRequest| {
        let calls = calls.clone();
        let worker = worker.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let (mut writer, dispatch) = OperationDispatch::streaming(OperationResponse {
                lease_id: req.lease_id,
                accepted_at: fixed_time(),
            });
            tokio::spawn(async move {
                writer.write_frame(&progress(req.lease_id, 0)).unwrap();
                worker.release.notified().await;
                let _ = writer.write_frame(&result_frame(req.lease_id, 1));
                worker.mark_finished();
                let _ = writer.finish();
            });
            Ok(dispatch)
        })
    })
}

fn worker_aborts_after_response_handler(
    calls: Arc<AtomicUsize>,
    worker: Arc<WorkerCompletionGate>,
) -> OperationHandler {
    Arc::new(move |req: OperationRequest| {
        let calls = calls.clone();
        let worker = worker.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let (mut writer, dispatch) = OperationDispatch::streaming(OperationResponse {
                lease_id: req.lease_id,
                accepted_at: fixed_time(),
            });
            tokio::spawn(async move {
                writer.write_frame(&progress(req.lease_id, 0)).unwrap();
                worker.mark_finished();
            });
            Ok(dispatch)
        })
    })
}

async fn collect_body(mut dispatch: DispatchStream) -> Vec<NdjsonOutcome> {
    let mut outcomes = Vec::new();
    loop {
        let outcome = dispatch.frames.next_frame().await.unwrap();
        let done = matches!(
            outcome,
            NdjsonOutcome::Terminated(_) | NdjsonOutcome::StreamEnd
        );
        outcomes.push(outcome);
        if done {
            break;
        }
    }
    outcomes
}

async fn running_server(handler: OperationHandler) -> (SocketAddr, ServerRunning) {
    let server = HttpServer::new(creds(), handler);
    let running = server.serve("127.0.0.1:0".parse().unwrap()).await.unwrap();
    (running.bound, running)
}

async fn write_chunk(stream: &mut tokio::net::TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    stream
        .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
        .await?;
    stream.write_all(bytes).await?;
    stream.write_all(b"\r\n").await
}

#[tokio::test]
async fn server_streaming_dispatch_returns_before_terminal_frame() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = calls.clone();
    let terminal_gate = Arc::new(Notify::new());
    let terminal_gate_for_handler = terminal_gate.clone();
    let handler: OperationHandler = Arc::new(move |req| {
        let calls_for_handler = calls_for_handler.clone();
        let terminal_gate = terminal_gate_for_handler.clone();
        Box::pin(async move {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            let (mut writer, body) = OperationDispatch::streaming(OperationResponse {
                lease_id: req.lease_id,
                accepted_at: fixed_time(),
            });
            tokio::spawn(async move {
                writer.write_frame(&progress(req.lease_id, 0)).unwrap();
                terminal_gate.notified().await;
                writer.write_frame(&result_frame(req.lease_id, 1)).unwrap();
                writer.finish().unwrap();
            });
            Ok(body)
        })
    });

    let (addr, running) = running_server(handler).await;
    let client = HttpClient::new(addr);
    let mut dispatch = tokio::time::timeout(
        Duration::from_secs(1),
        client.dispatch(
            &creds(),
            "streaming-server-1",
            request(LeaseId(1), serde_json::json!({})),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        dispatch.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Frame(_)
    ));
    terminal_gate.notify_one();
    assert!(matches!(
        dispatch.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Terminated(_)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn active_same_key_same_body_rejects_without_second_handler_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Notify::new());
    let handler = slow_streaming_handler(calls.clone(), gate.clone());
    let (addr, running) = running_server(handler).await;
    let client = HttpClient::new(addr);
    let req = request(LeaseId(10), serde_json::json!({"path": "/tmp/a"}));

    let first = client
        .dispatch(&creds(), "active-dup", req.clone())
        .await
        .unwrap();
    let err = client
        .dispatch(&creds(), "active-dup", req)
        .await
        .unwrap_err();
    assert!(matches!(err, ProtocolError::DuplicateIdempotencyKey { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(first);
    gate.notify_waiters();
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn completed_stream_replays_cached_buffered_response() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, running) = running_server(streaming_handler(calls.clone())).await;
    let client = HttpClient::new(addr);
    let req = request(LeaseId(11), serde_json::json!({"path": "/tmp/a"}));

    let first = collect_body(
        client
            .dispatch(&creds(), "completed-replay", req.clone())
            .await
            .unwrap(),
    )
    .await;
    let second = collect_body(
        client
            .dispatch(&creds(), "completed-replay", req)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn handler_error_clears_active_idempotency_entry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = calls.clone();
    let handler: OperationHandler = Arc::new(move |_req| {
        let calls_for_handler = calls_for_handler.clone();
        Box::pin(async move {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            Err(ProtocolError::InvalidPayload {
                detail: "scripted failure".to_owned(),
            })
        })
    });
    let (addr, running) = running_server(handler).await;
    let client = HttpClient::new(addr);
    let req = request(LeaseId(12), serde_json::json!({"path": "/tmp/a"}));

    let first = client
        .dispatch(&creds(), "handler-error", req.clone())
        .await
        .unwrap_err();
    let second = client
        .dispatch(&creds(), "handler-error", req)
        .await
        .unwrap_err();

    assert!(matches!(first, ProtocolError::InvalidPayload { .. }));
    assert!(matches!(second, ProtocolError::InvalidPayload { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn aborted_client_stream_keeps_active_idempotency_until_worker_terminal() {
    let calls = Arc::new(AtomicUsize::new(0));
    let worker = WorkerCompletionGate::new();
    let handler = client_aborts_before_worker_finishes_handler(calls.clone(), worker.clone());
    let (addr, running) = running_server(handler).await;
    let client = HttpClient::new(addr);
    let req = request(LeaseId(13), serde_json::json!({"path": "/tmp/a"}));

    let mut first = client
        .dispatch(&creds(), "aborted-stream", req.clone())
        .await
        .unwrap();
    assert!(matches!(
        first.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Frame(_)
    ));
    drop(first);

    assert!(!worker.is_finished());
    let duplicate = client
        .dispatch(&creds(), "aborted-stream", req)
        .await
        .unwrap_err();
    assert!(matches!(
        duplicate,
        ProtocolError::DuplicateIdempotencyKey { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    worker.release();
    worker.wait_finished().await;
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn dropped_client_stream_replays_after_worker_terminal() {
    let calls = Arc::new(AtomicUsize::new(0));
    let worker = WorkerCompletionGate::new();
    let handler = client_aborts_before_worker_finishes_handler(calls.clone(), worker.clone());
    let (addr, running) = running_server(handler).await;
    let client = HttpClient::new(addr);
    let req = request(LeaseId(15), serde_json::json!({"path": "/tmp/a"}));

    let mut first = client
        .dispatch(&creds(), "dropped-then-terminal", req.clone())
        .await
        .unwrap();
    assert!(matches!(
        first.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Frame(_)
    ));
    drop(first);
    worker.release();
    worker.wait_finished().await;

    let replay = collect_body(
        client
            .dispatch(&creds(), "dropped-then-terminal", req)
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(replay.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn worker_abort_after_response_clears_active_idempotency_entry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let worker = WorkerCompletionGate::new();
    let handler = worker_aborts_after_response_handler(calls.clone(), worker.clone());
    let (addr, running) = running_server(handler).await;
    let client = HttpClient::new(addr);
    let req = request(LeaseId(14), serde_json::json!({"path": "/tmp/a"}));

    let mut first = client
        .dispatch(&creds(), "worker-abort", req.clone())
        .await
        .unwrap();
    assert!(matches!(
        first.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Frame(_)
    ));
    let _ = first.frames.next_frame().await.unwrap_err();
    worker.wait_finished().await;

    let _ = client.dispatch(&creds(), "worker-abort", req).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn dispatch_returns_after_response_line_before_progress_stream_finishes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body_gate = Arc::new(Notify::new());
    let body_gate_for_server = body_gate.clone();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0_u8; 4096];
        let _ = stream.read(&mut buf).await.unwrap();

        let response = OperationResponse {
            lease_id: LeaseId(1),
            accepted_at: fixed_time(),
        };
        let mut response_line = serde_json::to_vec(&response).unwrap();
        response_line.push(b'\n');
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ntransfer-encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
        write_chunk(&mut stream, &response_line).await.unwrap();
        stream.flush().await.unwrap();

        body_gate_for_server.notified().await;
        write_chunk(
            &mut stream,
            &ndjson_bytes(&[progress(LeaseId(1), 0), result_frame(LeaseId(1), 1)]),
        )
        .await
        .unwrap();
        stream.write_all(b"0\r\n\r\n").await.unwrap();
    });

    let client = HttpClient::new(addr);
    let timed = tokio::time::timeout(
        Duration::from_secs(1),
        client.dispatch(
            &creds(),
            "idem-1",
            request(LeaseId(1), serde_json::json!({})),
        ),
    )
    .await;
    assert!(
        timed.is_ok(),
        "dispatch must return after OperationResponse line"
    );
    let mut dispatch = timed.unwrap().unwrap();

    assert_eq!(dispatch.response.lease_id, LeaseId(1));
    body_gate.notify_one();
    assert!(matches!(
        dispatch.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Frame(_)
    ));
    assert!(matches!(
        dispatch.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Terminated(_)
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn dispatch_rejects_response_lease_mismatch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, running) = running_server(operation_handler(calls, Some(LeaseId(99)))).await;
    let client = HttpClient::new(addr);

    let err = client
        .dispatch(
            &creds(),
            "idem-2",
            request(LeaseId(1), serde_json::json!({})),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        ProtocolError::WrongLeaseId {
            expected: LeaseId(1),
            got: LeaseId(99)
        }
    ));
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn missing_idempotency_key_rejects() {
    let headers = hyper::HeaderMap::new();
    let err = require_idempotency_key(&headers).unwrap_err();
    assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
}

#[tokio::test]
async fn nested_body_idempotency_key_rejects() {
    let body = serde_json::to_vec(&request(
        LeaseId(1),
        serde_json::json!({"idempotency_key": "not allowed"}),
    ))
    .unwrap();
    let err = validate_no_body_idempotency_key(&body).unwrap_err();
    assert!(matches!(err, ProtocolError::HeaderBodyKeyMismatch));
}

#[tokio::test]
async fn same_key_same_body_replays_without_handler() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, running) = running_server(operation_handler(calls.clone(), None)).await;
    let client = HttpClient::new(addr);
    let req = request(LeaseId(1), serde_json::json!({"path": "/tmp/a"}));

    let first = client
        .dispatch(&creds(), "idem-3", req.clone())
        .await
        .unwrap();
    let second = client.dispatch(&creds(), "idem-3", req).await.unwrap();

    assert_eq!(first.response.lease_id, second.response.lease_id);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn same_key_different_body_rejects_duplicate() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, running) = running_server(operation_handler(calls.clone(), None)).await;
    let client = HttpClient::new(addr);

    client
        .dispatch(
            &creds(),
            "idem-4",
            request(LeaseId(1), serde_json::json!({"path": "/tmp/a"})),
        )
        .await
        .unwrap();
    let err = client
        .dispatch(
            &creds(),
            "idem-4",
            request(LeaseId(1), serde_json::json!({"path": "/tmp/b"})),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, ProtocolError::DuplicateIdempotencyKey { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn cached_replay_preserves_frames() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, running) = running_server(operation_handler(calls, None)).await;
    let client = HttpClient::new(addr);
    let req = request(LeaseId(1), serde_json::json!({"path": "/tmp/a"}));

    let mut first = client
        .dispatch(&creds(), "idem-5", req.clone())
        .await
        .unwrap();
    let mut second = client.dispatch(&creds(), "idem-5", req).await.unwrap();

    assert!(matches!(
        first.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Frame(_)
    ));
    assert!(matches!(
        second.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Frame(_)
    ));
    assert!(matches!(
        second.frames.next_frame().await.unwrap(),
        NdjsonOutcome::Terminated(_)
    ));
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[test]
fn idempotency_cache_evicts_oldest_after_capacity() {
    let mut cache = IdempotencyCache::new(2);
    let response = CachedResponse {
        response: OperationResponse {
            lease_id: LeaseId(1),
            accepted_at: fixed_time(),
        },
        body: vec![b'1'],
    };
    cache.complete("a", [1; 32], response.clone());
    cache.complete("b", [2; 32], response.clone());
    cache.complete("c", [3; 32], response);
    assert!(matches!(
        cache.lookup("a", [1; 32]),
        IdempotencyBegin::Started
    ));
}

#[test]
fn idempotency_cache_does_not_exceed_capacity_under_all_active() {
    let mut cache = IdempotencyCache::new(2);
    assert!(matches!(
        cache.begin("a".to_owned(), [1; 32]),
        IdempotencyBegin::Started
    ));
    assert!(matches!(
        cache.begin("b".to_owned(), [2; 32]),
        IdempotencyBegin::Started
    ));
    // Both entries are in-flight (Active); a third cannot be admitted without
    // either growing past capacity or dropping a tracked entry, so the cache
    // refuses it with backpressure rather than admitting it untracked.
    assert!(matches!(
        cache.begin("c".to_owned(), [3; 32]),
        IdempotencyBegin::AtCapacity
    ));
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn idempotency_cache_make_room_evicts_completed_behind_active() {
    let mut cache = IdempotencyCache::new(2);
    let response = CachedResponse {
        response: OperationResponse {
            lease_id: LeaseId(1),
            accepted_at: fixed_time(),
        },
        body: vec![b'1'],
    };
    // `a` stays Active (in-flight) and is the oldest; `b` completes. At
    // capacity, admitting `c` must scan past the older Active `a` to evict the
    // newer Completed `b`, never evicting the in-flight entry.
    cache.begin("a".to_owned(), [1; 32]);
    cache.begin("b".to_owned(), [2; 32]);
    cache.complete("b", [2; 32], response);

    assert!(matches!(
        cache.begin("c".to_owned(), [3; 32]),
        IdempotencyBegin::Started
    ));
    assert_eq!(cache.entries.len(), 2);
    assert!(cache.entries.contains_key("a"));
    assert!(!cache.entries.contains_key("b"));
    assert!(cache.entries.contains_key("c"));
}

/// Accept the connection, drain the request, then hold the socket open without
/// ever writing a response — simulating a worker that hangs mid-request.
async fn unresponsive_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0_u8; 4096];
        let _ = stream.read(&mut buf).await.unwrap();
        // Hold the connection; never reply. Keep `stream` alive so the client
        // sees a hung peer, not a closed connection.
        std::future::pending::<()>().await;
        drop(stream);
    });
    (addr, server)
}

#[tokio::test]
async fn handshake_times_out_when_worker_never_responds() {
    let (addr, server) = unresponsive_server().await;
    let client =
        HttpClient::with_timeouts(addr, Duration::from_millis(100), Duration::from_millis(100));

    // Outer guard: if the client fails to self-time-out, this fires first and
    // the unwrap panics — the failure points at a client that hung.
    let result = tokio::time::timeout(Duration::from_secs(5), client.handshake(1))
        .await
        .unwrap();

    assert!(
        matches!(result, Err(ProtocolError::Timeout { .. })),
        "expected Timeout, got {result:?}"
    );
    server.abort();
}

#[tokio::test]
async fn dispatch_times_out_when_worker_never_sends_response_line() {
    let (addr, server) = unresponsive_server().await;
    let client =
        HttpClient::with_timeouts(addr, Duration::from_millis(100), Duration::from_millis(100));

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        client.dispatch(
            &creds(),
            "timeout-1",
            request(LeaseId(1), serde_json::json!({})),
        ),
    )
    .await
    .unwrap();

    assert!(
        matches!(result, Err(ProtocolError::Timeout { .. })),
        "expected Timeout, got {:?}",
        result.as_ref().map(|_| "Ok(stream)")
    );
    server.abort();
}

#[tokio::test]
async fn server_closes_connection_when_request_head_never_completes() {
    // Slowloris (M13): a peer that opens a connection and sends a partial
    // request head, then stalls, must be cut loose once the header-read timeout
    // elapses — not pinned to a per-connection task forever.
    let handler: OperationHandler =
        Arc::new(|_req| Box::pin(async { Err(ProtocolError::InternalServerError) }));
    let server =
        HttpServer::new(creds(), handler).with_header_read_timeout(Duration::from_millis(150));
    let running = server.serve("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = running.bound;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    // A request line + one header, but no terminating blank line: the head is
    // never completed, so the server keeps waiting for more header bytes.
    stream
        .write_all(b"POST /v1/handshake HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .unwrap();

    // Outer guard: if the server never closes the half-open connection, the read
    // parks forever and this timeout fires, panicking on the unwrap below.
    let mut buf = [0_u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n, 0, "expected EOF once the header-read timeout elapsed");

    drop(running);
}

#[tokio::test]
async fn handshake_rejects_mismatched_agreed_version() {
    // A well-formed 200 whose `agreed` does not echo `offered` must be rejected:
    // ADR-0016 is an exact match, so the client defends the server->client
    // direction even though a conforming server never emits this.
    let offered = voom_core::PROTOCOL_VERSION;
    let agreed = offered + 1;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0_u8; 4096];
        let _ = stream.read(&mut buf).await.unwrap();
        let body = format!("{{\"agreed\":{agreed}}}");
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
    });

    let client = HttpClient::new(addr);
    let err = client.handshake(offered).await.unwrap_err();
    assert!(
        matches!(
            &err,
            ProtocolError::UnsupportedProtocolVersion { offered: o, expected: e }
                if *o == offered && *e == agreed
        ),
        "expected UnsupportedProtocolVersion, got {err:?}"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn handshake_accepts_matching_agreed_version() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, running) = running_server(operation_handler(calls, None)).await;
    let client = HttpClient::new(addr);
    let resp = client.handshake(voom_core::PROTOCOL_VERSION).await.unwrap();
    assert_eq!(resp.agreed, voom_core::PROTOCOL_VERSION);
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[test]
fn status_code_dependency_stays_used() {
    assert_eq!(StatusCode::OK.as_u16(), 200);
}
