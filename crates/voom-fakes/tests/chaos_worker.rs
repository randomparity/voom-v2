#![expect(
    clippy::unwrap_used,
    reason = "integration tests use direct process assertions"
)]

use std::process::Stdio;
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use voom_worker_protocol::{
    ClientHandle, HttpClient, OperationKind, OperationRequest, WorkerCredentials,
};

#[tokio::test]
async fn baseline_launches_and_returns_ordered_frames() {
    let mut launch = spawn_chaos().await;
    let client = HttpClient::new(launch.bound);
    let req = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(101),
        payload: serde_json::json!({"path": "/library/example.mkv"}),
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    };
    let mut stream = client
        .dispatch(&launch.credentials, "baseline-launch", req)
        .await
        .unwrap();
    let first = stream.frames.next_frame().await.unwrap();
    assert!(matches!(
        first,
        voom_worker_protocol::NdjsonOutcome::Frame(_)
    ));
    let terminal = stream.frames.next_frame().await.unwrap();
    assert!(matches!(
        terminal,
        voom_worker_protocol::NdjsonOutcome::Terminated(_)
    ));
    launch.shutdown().await;
}

#[tokio::test]
async fn invalid_payload_returns_protocol_error_and_worker_stays_alive() {
    let mut launch = spawn_chaos().await;
    let client = HttpClient::new(launch.bound);
    let req = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(102),
        payload: serde_json::json!({}),
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    };
    let err = client
        .dispatch(&launch.credentials, "invalid-payload", req)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::InvalidPayload { .. }
    ));
    assert!(launch.child.try_wait().unwrap().is_none());
    launch.shutdown().await;
}

#[tokio::test]
async fn crash_mode_exits_non_zero() {
    let mut launch = spawn_chaos().await;
    let body = operation_body(
        201,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "crash"
        }),
    );
    let _ = send_raw_operation_and_read_some(launch.bound, body, "crash-mode").await;
    let status = tokio::time::timeout(Duration::from_secs(5), launch.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
}

#[tokio::test]
async fn malformed_result_is_rejected_by_reader() {
    let mut launch = spawn_chaos().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(
        202,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "malformed_result"
        }),
    );
    let mut stream = client
        .dispatch(&launch.credentials, "malformed-result", req)
        .await
        .unwrap();
    let err = stream.frames.next_frame().await.unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::MalformedFrame { .. }
    ));
    launch.shutdown().await;
}

#[tokio::test]
async fn stall_mode_keeps_response_body_pending() {
    let mut launch = spawn_chaos().await;
    let body = operation_body(
        203,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "stall",
            "stall_ms": 1000
        }),
    );
    let mut stream = tokio::net::TcpStream::connect(launch.bound).await.unwrap();
    write_raw_operation(&mut stream, launch.bound, body, "stall-mode").await;
    let mut bytes = read_until_headers(&mut stream).await;
    assert_text_contains(&bytes, "HTTP/1.1 200 OK");
    read_until_contains(&mut stream, &mut bytes, "\"lease_id\"").await;
    assert_no_more_bytes_within(&mut stream, Duration::from_millis(250)).await;
    launch.shutdown().await;
}

#[tokio::test]
async fn non_converging_progress_yields_progress_without_terminal() {
    let mut launch = spawn_chaos().await;
    let body = operation_body(
        204,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "non_converging_progress",
            "progress_count": 2
        }),
    );
    let mut stream = tokio::net::TcpStream::connect(launch.bound).await.unwrap();
    write_raw_operation(&mut stream, launch.bound, body, "non-converging").await;
    let mut bytes = read_until_headers(&mut stream).await;
    assert_text_contains(&bytes, "HTTP/1.1 200 OK");
    read_until_contains(&mut stream, &mut bytes, "\"lease_id\"").await;
    read_until_contains(&mut stream, &mut bytes, "\"kind\":\"progress\"").await;
    assert_text_does_not_contain(&bytes, "\"kind\":\"result\"");
    assert_no_more_bytes_within(&mut stream, Duration::from_millis(250)).await;
    launch.shutdown().await;
}

#[tokio::test]
async fn deadline_exceeded_delays_progress_past_short_timeout() {
    let mut launch = spawn_chaos().await;
    let body = operation_body(
        205,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "deadline_exceeded",
            "progress_interval_ms": 1000,
            "progress_count": 1
        }),
    );
    let mut stream = tokio::net::TcpStream::connect(launch.bound).await.unwrap();
    write_raw_operation(&mut stream, launch.bound, body, "deadline-exceeded").await;
    let mut bytes = read_until_headers(&mut stream).await;
    assert_text_contains(&bytes, "HTTP/1.1 200 OK");
    read_until_contains(&mut stream, &mut bytes, "\"lease_id\"").await;
    assert_text_does_not_contain(&bytes, "\"kind\":\"progress\"");
    assert_text_does_not_contain(&bytes, "\"kind\":\"result\"");
    assert_no_more_bytes_within(&mut stream, Duration::from_millis(250)).await;
    launch.shutdown().await;
}

fn operation_request(lease_id: u64, payload: serde_json::Value) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(lease_id),
        payload,
        heartbeat_deadline_ms: 100,
        progress_idle_deadline_ms: 100,
    }
}

fn operation_body(lease_id: u64, payload: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&operation_request(lease_id, payload)).unwrap()
}

async fn send_raw_operation_and_read_some(
    bound: std::net::SocketAddr,
    body: Vec<u8>,
    idempotency_key: &str,
) -> Vec<u8> {
    let mut stream = tokio::net::TcpStream::connect(bound).await.unwrap();
    write_raw_operation(&mut stream, bound, body, idempotency_key).await;
    let mut out = vec![0; 4096];
    let n = stream.read(&mut out).await.unwrap();
    out.truncate(n);
    out
}

async fn read_until_headers(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    read_until_contains_bytes(stream, b"\r\n\r\n", Duration::from_secs(1)).await
}

async fn read_until_contains(stream: &mut tokio::net::TcpStream, out: &mut Vec<u8>, needle: &str) {
    let needle = needle.as_bytes();
    loop {
        if out.windows(needle.len()).any(|window| window == needle) {
            return;
        }
        read_more(stream, out, Duration::from_secs(1)).await;
    }
}

async fn read_until_contains_bytes(
    stream: &mut tokio::net::TcpStream,
    needle: &[u8],
    timeout: Duration,
) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        if out.windows(needle.len()).any(|window| window == needle) {
            return out;
        }
        read_more(stream, &mut out, timeout).await;
    }
}

async fn read_more(stream: &mut tokio::net::TcpStream, out: &mut Vec<u8>, timeout: Duration) {
    let mut buf = [0_u8; 1024];
    let n = tokio::time::timeout(timeout, stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0, "connection closed before expected bytes arrived");
    out.extend_from_slice(&buf[..n]);
}

async fn assert_no_more_bytes_within(stream: &mut tokio::net::TcpStream, duration: Duration) {
    let mut buf = [0_u8; 1];
    let outcome = tokio::time::timeout(duration, stream.read(&mut buf)).await;
    assert!(outcome.is_err(), "stream produced bytes before timeout");
}

fn assert_text_contains(bytes: &[u8], needle: &str) {
    let text = String::from_utf8_lossy(bytes);
    assert!(text.contains(needle), "missing {needle:?} in {text:?}");
}

fn assert_text_does_not_contain(bytes: &[u8], needle: &str) {
    let text = String::from_utf8_lossy(bytes);
    assert!(!text.contains(needle), "unexpected {needle:?} in {text:?}");
}

async fn write_raw_operation(
    stream: &mut tokio::net::TcpStream,
    bound: std::net::SocketAddr,
    body: Vec<u8>,
    idempotency_key: &str,
) {
    let req = format!(
        "POST /v1/operations HTTP/1.1\r\n\
         Host: {bound}\r\n\
         Content-Length: {}\r\n\
         X-Voom-Protocol-Version: {}\r\n\
         Authorization: Bearer phase1-bootstrap-secret\r\n\
         X-Voom-Worker-Id: 1\r\n\
         X-Voom-Worker-Epoch: 0\r\n\
         X-Voom-Idempotency-Key: {idempotency_key}\r\n\
         \r\n",
        body.len(),
        voom_core::PROTOCOL_VERSION
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
}

struct TestLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
    bound: std::net::SocketAddr,
    credentials: WorkerCredentials,
}

impl TestLaunch {
    async fn shutdown(&mut self) {
        drop(self.stdin.take());
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "status={status}");
    }
}

async fn spawn_chaos() -> TestLaunch {
    let worker_id = voom_core::WorkerId(1);
    let worker_epoch = 0;
    let secret = "phase1-bootstrap-secret";
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_chaos-worker"))
        .env("VOOM_WORKER_SECRET", secret)
        .env("VOOM_WORKER_ID", worker_id.0.to_string())
        .env("VOOM_WORKER_EPOCH", worker_epoch.to_string())
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let bound = line
        .strip_prefix("BOUND addr=")
        .unwrap()
        .parse::<std::net::SocketAddr>()
        .unwrap();
    TestLaunch {
        child,
        stdin,
        bound,
        credentials: WorkerCredentials {
            worker_id,
            worker_epoch,
            secret: SecretString::from(secret),
        },
    }
}
