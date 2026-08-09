use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rand::SeedableRng;
use rand::rngs::StdRng;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use voom_core::{ArtifactAccessMode, NodeId, OperationKind, VoomError};

use super::{
    ControlPlaneClient, DEFAULT_MAXIMUM_ATTEMPTS, INITIAL_RETRY_DELAY, MAX_RETRY_DELAY,
    RetryBackoff, RetryRequest, RetrySettings,
};
use crate::config::{AgentConfig, LoadedAgentConfig, TokenSource, WorkerConfig};

#[derive(Debug, Serialize)]
struct TestBody {
    incarnation_id: &'static str,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct TestResponse {
    accepted: bool,
}

/// How long `RawResponse::Timeout` withholds a response so the client's request timeout fires.
const SERVER_STALL: Duration = Duration::from_millis(400);

/// Shorter than [`SERVER_STALL`] so a stall is classified as a timeout rather than a disconnect.
const REQUEST_TIMEOUT: Duration = Duration::from_millis(100);

#[test]
fn production_retry_policy_is_bounded_and_unlimited_is_explicit() {
    let loaded = loaded_config("http://127.0.0.1:7443");
    let bounded = ControlPlaneClient::from_config(&loaded).unwrap();
    let unlimited = ControlPlaneClient::from_config_with_unbounded_retries(&loaded).unwrap();

    assert_eq!(
        bounded.retry.maximum_attempts,
        Some(DEFAULT_MAXIMUM_ATTEMPTS)
    );
    assert_eq!(unlimited.retry.maximum_attempts, None);
}

#[tokio::test]
async fn retryable_failures_stop_at_the_attempt_limit() {
    let responses = [RawResponse::Status(500), RawResponse::Status(500)];
    let (address, received, server) = raw_server(responses);
    let client = test_client(address, 2, Duration::ZERO, REQUEST_TIMEOUT);
    let request = RetryRequest::new(
        "bounded-key".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();

    let error = client
        .send::<TestResponse, _>("/test", &request)
        .await
        .unwrap_err();

    assert_eq!(received.iter().count(), 2);
    let message = match error {
        VoomError::ExternalSystemUnavailable(message) => message,
        error => format!("unexpected retry exhaustion error: {error}"),
    };
    assert!(message.contains("after 2 attempts"));
    assert!(message.contains("500 Internal Server Error"));
    server.join().unwrap();
}

#[test]
fn retry_backoff_jitters_each_sleep_and_doubles_only_the_ceiling() {
    let mut rng = StdRng::seed_from_u64(448);
    let mut backoff = RetryBackoff::new(INITIAL_RETRY_DELAY, MAX_RETRY_DELAY);
    let ceilings = [
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(4),
        Duration::from_secs(8),
        Duration::from_secs(16),
        Duration::from_secs(30),
        Duration::from_secs(30),
    ];

    let mut delays = Vec::new();
    for ceiling in ceilings {
        assert_eq!(backoff.ceiling, ceiling);
        let delay = backoff.next_delay(&mut rng);
        assert!(delay <= ceiling);
        delays.push(delay);
    }
    assert_eq!(backoff.ceiling, MAX_RETRY_DELAY);
    assert!(delays.windows(2).any(|pair| pair[0] != pair[1]));
    let capped_delays = (0..8)
        .map(|_| backoff.next_delay(&mut rng))
        .collect::<Vec<_>>();
    assert!(capped_delays.iter().any(|delay| *delay < MAX_RETRY_DELAY));
    assert!(capped_delays.windows(2).any(|pair| pair[0] != pair[1]));
}

#[test]
fn retry_request_freezes_body_key_and_hash() {
    let request = RetryRequest::new(
        "same-key".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();

    assert_eq!(request.idempotency_key(), "same-key");
    assert_eq!(
        request.body(),
        br#"{"incarnation_id":"0123456789abcdef0123456789abcdef"}"#
    );
    assert_eq!(request.request_hash().len(), 64);
}

#[tokio::test]
async fn client_builds_for_explicit_loopback_http() {
    let loaded = loaded_config("http://127.0.0.1:7443");
    let client = ControlPlaneClient::from_config_with_settings(
        &loaded,
        RetrySettings {
            initial_delay: Duration::ZERO,
            maximum_delay: Duration::ZERO,
            maximum_attempts: Some(1),
        },
        Duration::from_millis(50),
    )
    .unwrap();
    let debug = format!("{client:?}");
    assert!(!debug.contains("secret-token"));

    let request = RetryRequest::new(
        "key".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();
    let result: Result<serde_json::Value, _> = client.send("/test", &request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn retries_reuse_identical_body_and_key_across_retry_classes() {
    let responses = [
        RawResponse::Status(408),
        RawResponse::Status(429),
        RawResponse::Status(500),
        RawResponse::Timeout,
        RawResponse::Disconnect,
        RawResponse::Success,
    ];
    let (address, received, server) = raw_server(responses);
    let client = test_client(
        address,
        responses.len() + 1,
        Duration::ZERO,
        REQUEST_TIMEOUT,
    );
    let request = RetryRequest::new(
        "replay-key".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();

    let response: TestResponse = client.send("/test", &request).await.unwrap();
    assert_eq!(response, TestResponse { accepted: true });
    let attempts = received.iter().take(responses.len()).collect::<Vec<_>>();
    assert_eq!(attempts.len(), responses.len());
    for attempt in attempts {
        assert_eq!(attempt.path, "/test");
        assert_eq!(attempt.idempotency_key, "replay-key");
        assert_eq!(attempt.authorization, "Bearer secret-token");
        assert_eq!(attempt.body, request.body());
    }
    server.join().unwrap();
}

#[tokio::test]
async fn other_client_errors_are_terminal() {
    let responses = [RawResponse::BadRequest];
    let (address, received, server) = raw_server(responses);
    let client = test_client(address, 3, Duration::ZERO, REQUEST_TIMEOUT);
    let request = RetryRequest::new(
        "terminal-key".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();

    let result: Result<TestResponse, _> = client.send("/test", &request).await;
    assert!(matches!(result, Err(voom_core::VoomError::Config(_))));
    assert_eq!(received.iter().count(), 1);
    server.join().unwrap();
}

#[tokio::test]
async fn success_envelopes_are_strict_and_bounded() {
    for response in [RawResponse::UnknownEnvelope, RawResponse::Oversized] {
        let (address, received, server) = raw_server([response]);
        let client = test_client(address, 1, Duration::ZERO, REQUEST_TIMEOUT);
        let request = RetryRequest::new(
            "strict-key".to_owned(),
            &TestBody {
                incarnation_id: "0123456789abcdef0123456789abcdef",
            },
        )
        .unwrap();

        let result: Result<TestResponse, _> = client.send("/test", &request).await;
        assert!(result.is_err());
        assert_eq!(received.iter().count(), 1);
        server.join().unwrap();
    }
}

#[test]
fn malformed_custom_ca_is_rejected_at_construction() {
    let temp = tempfile::TempDir::new().unwrap();
    let ca = temp.path().join("bad-ca.pem");
    std::fs::write(&ca, "not a certificate").unwrap();
    let mut loaded = loaded_config("https://localhost:7443");
    loaded.config.ca_cert = Some(ca);

    assert!(ControlPlaneClient::from_config(&loaded).is_err());
}

fn loaded_config(url: &str) -> LoadedAgentConfig {
    LoadedAgentConfig {
        config: AgentConfig {
            control_plane_url: url.to_owned(),
            ca_cert: None,
            node_id: NodeId(7),
            poll_interval_ms: 1_000,
            lease_ttl_seconds: 30,
            progress_idle_timeout_seconds: 300,
            shutdown_grace_seconds: 10,
            node_token: TokenSource::Env {
                name: "VOOM_NODE_TOKEN".to_owned(),
            },
            workers: vec![WorkerConfig {
                name: "echo".to_owned(),
                program: PathBuf::from("/bin/echo"),
                args: Vec::new(),
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                max_parallel: 1,
            }],
        },
        node_token: SecretString::from("secret-token"),
    }
}

fn test_client(
    address: SocketAddr,
    maximum_attempts: usize,
    retry_delay: Duration,
    timeout: Duration,
) -> ControlPlaneClient {
    let loaded = loaded_config(&format!("http://{address}"));
    ControlPlaneClient::from_config_with_settings(
        &loaded,
        RetrySettings {
            initial_delay: retry_delay,
            maximum_delay: retry_delay,
            maximum_attempts: Some(maximum_attempts),
        },
        timeout,
    )
    .unwrap()
}

#[derive(Clone, Copy)]
enum RawResponse {
    Status(u16),
    Timeout,
    Disconnect,
    Success,
    BadRequest,
    UnknownEnvelope,
    Oversized,
}

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    idempotency_key: String,
    authorization: String,
    body: Vec<u8>,
}

fn raw_server<const N: usize>(
    responses: [RawResponse; N],
) -> (
    SocketAddr,
    mpsc::Receiver<CapturedRequest>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut handlers = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let sender = sender.clone();
            handlers.push(thread::spawn(move || {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                sender.send(read_request(&mut stream)).unwrap();
                match response {
                    RawResponse::Status(status) => write_response(&mut stream, status, "{}"),
                    RawResponse::Timeout => thread::sleep(SERVER_STALL),
                    RawResponse::Disconnect => {}
                    RawResponse::Success => write_response(
                        &mut stream,
                        200,
                        r#"{"schema_version":"0","command":"test","status":"ok","data":{"accepted":true},"warnings":[],"error":null}"#,
                    ),
                    RawResponse::BadRequest => write_response(
                        &mut stream,
                        400,
                        r#"{"schema_version":"0","command":"test","status":"error","data":null,"warnings":[],"error":{"code":"BAD_ARGS","message":"bad input"}}"#,
                    ),
                    RawResponse::UnknownEnvelope => write_response(
                        &mut stream,
                        200,
                        r#"{"schema_version":"0","command":"test","status":"ok","data":{"accepted":true},"warnings":[],"error":null,"unknown":true}"#,
                    ),
                    RawResponse::Oversized => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            }));
        }
        for handler in handlers {
            handler.join().unwrap();
        }
    });
    (address, receiver, handle)
}

fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end])
        .unwrap()
        .to_owned();
    let content_length = header_value(&headers, "content-length")
        .parse::<usize>()
        .unwrap();
    while bytes.len() < header_end + content_length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    let request_line = headers.lines().next().unwrap();
    CapturedRequest {
        path: request_line
            .split_ascii_whitespace()
            .nth(1)
            .unwrap()
            .to_owned(),
        idempotency_key: header_value(&headers, "x-voom-idempotency-key").to_owned(),
        authorization: header_value(&headers, "authorization").to_owned(),
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}

fn header_value<'a>(headers: &'a str, name: &str) -> &'a str {
    headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
        .unwrap()
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}
