use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use hyper::StatusCode;
use secrecy::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use voom_core::{LeaseId, WorkerId};

use super::*;
use crate::ndjson::NdjsonOutcome;
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

fn fixed_time() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 0).unwrap()
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
            Ok(OperationDispatch {
                response: OperationResponse {
                    lease_id: response_lease,
                    accepted_at: fixed_time(),
                },
                body: ndjson_bytes(&[progress(lease_id, 0), result_frame(lease_id, 1)]),
            })
        })
    })
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
async fn dispatch_returns_after_response_line_before_progress_stream_finishes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
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

        tokio::time::sleep(Duration::from_millis(250)).await;
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
        Duration::from_millis(100),
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
    let dispatch = timed.unwrap().unwrap();

    assert_eq!(dispatch.response.lease_id, LeaseId(1));
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
    assert!(
        cache
            .record_miss("a".to_owned(), [1; 32], response.clone())
            .is_ok()
    );
    assert!(
        cache
            .record_miss("b".to_owned(), [2; 32], response.clone())
            .is_ok()
    );
    assert!(cache.record_miss("c".to_owned(), [3; 32], response).is_ok());
    assert!(matches!(
        cache.lookup("a", [1; 32]),
        IdempotencyLookup::Miss
    ));
}

#[test]
fn status_code_dependency_stays_used() {
    assert_eq!(StatusCode::OK.as_u16(), 200);
}
