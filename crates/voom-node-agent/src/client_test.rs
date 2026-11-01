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
use voom_core::{ArtifactAccessMode, ErrorCode, NodeId, OperationKind, VoomError};

use super::{
    ControlPlaneClient, DEFAULT_MAXIMUM_ATTEMPTS, INITIAL_RETRY_DELAY, MAX_RETRY_DELAY,
    PROTOCOL_VERSION_HEADER, RetryBackoff, RetryRequest, RetrySettings,
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

const SUCCESS_BODY: &str = r#"{"schema_version":"0","command":"test","status":"ok","data":{"accepted":true},"warnings":[],"error":null}"#;

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
    let client = test_client(address, Some(2), Duration::ZERO, REQUEST_TIMEOUT);
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
    assert_eq!(error.error_code(), ErrorCode::ExternalSystemUnavailable);
    let VoomError::ExternalSystemUnavailable(message) = error else {
        return;
    };
    assert!(message.contains("after 2 attempts"));
    assert!(message.contains("500 Internal Server Error"));
    server.join().unwrap();
}

#[tokio::test]
async fn unlimited_retries_continue_past_default_attempt_limit() {
    let responses: [RawResponse; DEFAULT_MAXIMUM_ATTEMPTS + 1] = std::array::from_fn(|index| {
        if index < DEFAULT_MAXIMUM_ATTEMPTS {
            RawResponse::Status(500)
        } else {
            RawResponse::Success
        }
    });
    let (address, received, server) = raw_server(responses);
    let client = test_client(address, None, Duration::ZERO, REQUEST_TIMEOUT);
    let request = RetryRequest::new(
        "unbounded-key".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();

    let response: TestResponse = client.send("/test", &request).await.unwrap();

    assert_eq!(response, TestResponse { accepted: true });
    assert_eq!(received.iter().count(), DEFAULT_MAXIMUM_ATTEMPTS + 1);
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
        Some(responses.len() + 1),
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
    let client = test_client(address, Some(3), Duration::ZERO, REQUEST_TIMEOUT);
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

fn protocol_test_request() -> RetryRequest<TestBody> {
    RetryRequest::new(
        "protocol-key".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap()
}

/// ADR 0123: the control plane refuses a request that does not name this build's version.
#[tokio::test]
async fn requests_offer_the_agent_protocol_version() {
    let (address, received, server) = raw_server([RawResponse::Success]);
    let client = test_client(address, Some(1), Duration::ZERO, REQUEST_TIMEOUT);

    let response: TestResponse = client
        .send("/test", &protocol_test_request())
        .await
        .unwrap();

    assert_eq!(response, TestResponse { accepted: true });
    let attempt = received.recv().unwrap();
    assert_eq!(
        attempt.protocol_version,
        Some(voom_core::PROTOCOL_VERSION.to_string())
    );
    server.join().unwrap();
}

/// ADR 0123: an answer from a control plane on another protocol version (or one that predates
/// the header) is refused before its body is used, and never retried: a lease must not follow.
#[tokio::test]
async fn skewed_success_is_refused_without_retry() {
    for response in [
        RawResponse::SuccessWithVersion(Some(voom_core::PROTOCOL_VERSION + 1)),
        RawResponse::SuccessWithVersion(Some(voom_core::PROTOCOL_VERSION - 1)),
        RawResponse::SuccessWithVersion(None),
        RawResponse::SuccessMalformedVersion,
    ] {
        let (address, received, server) = raw_server([response]);
        let client = test_client(address, None, Duration::ZERO, REQUEST_TIMEOUT);

        let error = client
            .send::<TestResponse, _>("/test", &protocol_test_request())
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::ConfigInvalid);
        assert!(error.to_string().contains("(ADR 0123)"), "{error}");
        assert_eq!(received.iter().count(), 1);
        server.join().unwrap();
    }
}

/// The control plane's protocol refusal is terminal even when retries are unbounded: a
/// skewed agent must stop, not retry activation forever.
#[tokio::test]
async fn protocol_refusal_is_terminal() {
    let (address, received, server) = raw_server([RawResponse::ProtocolRefusal]);
    let client = test_client(address, None, Duration::ZERO, REQUEST_TIMEOUT);

    let error = client
        .send::<TestResponse, _>("/test", &protocol_test_request())
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        error.to_string().contains(&format!(
            "offered={}, expected {}",
            voom_core::PROTOCOL_VERSION + 1,
            voom_core::PROTOCOL_VERSION
        )),
        "{error}"
    );
    assert_eq!(received.iter().count(), 1);
    server.join().unwrap();
}

/// Error answers are not version-checked: outer layers and proxies may produce them
/// without the header, and they never lead to execution.
#[tokio::test]
async fn error_answers_are_not_version_checked() {
    let (address, _received, server) = raw_server([RawResponse::BadRequest]);
    let client = test_client(address, Some(1), Duration::ZERO, REQUEST_TIMEOUT);
    let error = client
        .send::<TestResponse, _>("/test", &protocol_test_request())
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Config(ref message) if message == "bad input"));
    server.join().unwrap();
}

#[tokio::test]
async fn success_envelopes_are_strict_and_bounded() {
    for response in [RawResponse::UnknownEnvelope, RawResponse::Oversized] {
        let (address, received, server) = raw_server([response]);
        let client = test_client(address, Some(1), Duration::ZERO, REQUEST_TIMEOUT);
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
            storage_roots: Vec::new(),
            node_token: TokenSource::Env {
                name: "VOOM_NODE_TOKEN".to_owned(),
            },
            probe_hash_reuse: false,
            workers: vec![WorkerConfig {
                name: "echo".to_owned(),
                program: PathBuf::from("/bin/echo"),
                args: Vec::new(),
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                dependencies: crate::config::WorkerDependencyPaths::default(),
                accelerator: None,
                max_parallel: 1,
            }],
        },
        node_token: SecretString::from("secret-token"),
    }
}

fn test_client(
    address: SocketAddr,
    maximum_attempts: Option<usize>,
    retry_delay: Duration,
    timeout: Duration,
) -> ControlPlaneClient {
    let loaded = loaded_config(&format!("http://{address}"));
    ControlPlaneClient::from_config_with_settings(
        &loaded,
        RetrySettings {
            initial_delay: retry_delay,
            maximum_delay: retry_delay,
            maximum_attempts,
        },
        timeout,
    )
    .unwrap()
}

#[derive(Clone, Copy)]
enum RawResponse {
    CompletionWindow,
    Status(u16),
    Timeout,
    Disconnect,
    Success,
    BadRequest,
    UnknownEnvelope,
    Oversized,
    /// A success whose protocol header is this version, or absent for `None`.
    SuccessWithVersion(Option<u32>),
    SuccessMalformedVersion,
    /// The control plane's 400 refusal of a request from another protocol version.
    ProtocolRefusal,
}

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    idempotency_key: String,
    authorization: String,
    protocol_version: Option<String>,
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
                    RawResponse::CompletionWindow => write_response(
                        &mut stream,
                        408,
                        r#"{"schema_version":"0","command":"execution.complete","status":"error","data":null,"warnings":[],"error":{"code":"REQUEST_TIMEOUT","message":"completion response window elapsed"}}"#,
                    ),
                    RawResponse::Status(status) => write_response(&mut stream, status, "{}"),
                    RawResponse::Timeout => thread::sleep(SERVER_STALL),
                    RawResponse::Disconnect => {}
                    RawResponse::Success => write_response(&mut stream, 200, SUCCESS_BODY),
                    // Unstamped, as an outer layer or proxy may answer.
                    RawResponse::BadRequest => write_response_with_version(
                        &mut stream,
                        400,
                        r#"{"schema_version":"0","command":"test","status":"error","data":null,"warnings":[],"error":{"code":"BAD_ARGS","message":"bad input"}}"#,
                        None,
                    ),
                    RawResponse::UnknownEnvelope => write_response(
                        &mut stream,
                        200,
                        r#"{"schema_version":"0","command":"test","status":"ok","data":{"accepted":true},"warnings":[],"error":null,"unknown":true}"#,
                    ),
                    RawResponse::Oversized => {
                        let _ = stream.write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{PROTOCOL_VERSION_HEADER}: {}\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n",
                                voom_core::PROTOCOL_VERSION
                            )
                            .as_bytes(),
                        );
                    }
                    RawResponse::SuccessWithVersion(version) => write_response_with_version(
                        &mut stream,
                        200,
                        SUCCESS_BODY,
                        version.map(|version| version.to_string()).as_deref(),
                    ),
                    RawResponse::SuccessMalformedVersion => {
                        write_response_with_version(&mut stream, 200, SUCCESS_BODY, Some("x"));
                    }
                    RawResponse::ProtocolRefusal => {
                        let expected = voom_core::PROTOCOL_VERSION;
                        let offered = expected + 1;
                        let body = format!(
                            r#"{{"schema_version":"0","command":"api.request","status":"error","data":null,"warnings":[],"error":{{"code":"BAD_ARGS","message":"unsupported protocol version: offered={offered}, expected {expected}","hint":"mixed protocol versions are refused (ADR 0123)"}}}}"#
                        );
                        write_response(&mut stream, 400, &body);
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
        protocol_version: optional_header_value(&headers, PROTOCOL_VERSION_HEADER)
            .map(str::to_owned),
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}

fn header_value<'a>(headers: &'a str, name: &str) -> &'a str {
    optional_header_value(headers, name).unwrap()
}

fn optional_header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

/// Answer as a control plane on this build's protocol version.
fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let version = voom_core::PROTOCOL_VERSION.to_string();
    write_response_with_version(stream, status, body, Some(&version));
}

fn write_response_with_version(
    stream: &mut TcpStream,
    status: u16,
    body: &str,
    version: Option<&str>,
) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let version_header = version
        .map(|version| format!("{PROTOCOL_VERSION_HEADER}: {version}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{version_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// Issue #479: the replayed acquire response is byte-identical to the fresh
/// one, so the agent's mirror decodes both identically and stays strict about
/// unknown fields. Store-level corruption (e.g. a zero lease id) decodes here
/// by contract — proving it is the control plane's replay validator's job.
#[test]
fn replayed_acquire_wire_decodes_identically_and_rejects_unknown_fields() {
    use crate::client::AcquireOutcome;
    use voom_core::{LeaseId, TicketId, WorkerId};

    let leased = serde_json::json!({
        "outcome": "leased",
        "lease_id": 11,
        "scheduler_decision_id": 7,
        "ticket_id": 3,
        "worker_id": 4,
        "operation": "transcode_video",
        "dispatch_payload": {"operation": "transcode_video"},
        "lease_ttl_seconds": 60,
        "heartbeat_after_seconds": 30,
        "artifact_access_plan": {
            "id": 5,
            "owner_node_id": 1,
            "access_evidence": {
                "declaration": [
                    {"target": {"kind": "storage_root", "storage_root_id": 2},
                     "rights": ["write"]}
                ],
                "root_epochs": [{"storage_root_id": 2, "root_epoch": 0}]
            }
        }
    });
    let first: AcquireOutcome = serde_json::from_value(leased.clone()).unwrap();
    let second: AcquireOutcome = serde_json::from_value(leased).unwrap();
    assert_eq!(first, second);
    let AcquireOutcome::Leased(dispatch) = first else {
        unreachable!("the leased mirror decoded above");
    };
    assert_eq!(dispatch.lease_id, LeaseId(11));
    assert_eq!(dispatch.ticket_id, TicketId(3));
    assert_eq!(dispatch.worker_id, WorkerId(4));

    let mut unknown = serde_json::json!({
        "outcome": "leased",
        "lease_id": 11,
        "scheduler_decision_id": 7,
        "ticket_id": 3,
        "worker_id": 4,
        "operation": "transcode_video",
        "dispatch_payload": {},
        "lease_ttl_seconds": 60,
        "heartbeat_after_seconds": 30,
        "artifact_access_plan": {"id": 5, "owner_node_id": null, "access_evidence": null},
        "legacy_mode": "shared_mount"
    });
    assert!(serde_json::from_value::<AcquireOutcome>(unknown.clone()).is_err());
    unknown.as_object_mut().unwrap().remove("legacy_mode");
    unknown["artifact_access_plan"]["mode"] = serde_json::json!("shared_mount");
    assert!(serde_json::from_value::<AcquireOutcome>(unknown).is_err());
}
#[test]
fn commit_dtos_debug_redact_fence_hex() {
    let outcome = super::CommitAuthorizeOutcome {
        intent_id: voom_core::ids::ArtifactCommitIntentId(1),
        commit_record_id: voom_core::ids::ArtifactCommitRecordId(1),
        staging_storage_root_id: voom_core::StorageRootId(1),
        staging_provider_relative_locator: "staging/a.bin".to_owned(),
        target_storage_root_id: voom_core::StorageRootId(1),
        target_provider_relative_locator: "committed/a.bin".to_owned(),
        source_storage_root_id: voom_core::StorageRootId(1),
        source_provider_relative_locator: "library/a.bin".to_owned(),
        expected_size_bytes: 1,
        expected_content_hash: "blake3:x".to_owned(),
        fence_hex: "deadbeef".to_owned(),
    };
    let request = super::CommitCompleteRequest {
        node_id: NodeId(1),
        incarnation_id: "0123456789abcdef0123456789abcdef".parse().unwrap(),
        fence_hex: "deadbeef".to_owned(),
    };
    // The one-time fence is capability material: its Debug rendering must
    // never leak it into a log or telemetry surface.
    for rendered in [format!("{outcome:?}"), format!("{request:?}")] {
        assert!(!rendered.contains("deadbeef"), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
    }
}

#[tokio::test]
async fn completion_windows_do_not_exhaust_the_network_failure_budget() {
    let (address, received, server) = raw_server([
        RawResponse::CompletionWindow,
        RawResponse::CompletionWindow,
        RawResponse::CompletionWindow,
        RawResponse::CompletionWindow,
        RawResponse::CompletionWindow,
        RawResponse::CompletionWindow,
        RawResponse::Success,
    ]);
    let client = test_client(address, Some(2), Duration::ZERO, REQUEST_TIMEOUT);
    let request = RetryRequest::new(
        "same-completion".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();
    let response: TestResponse = client
        .send_completion_windows("/complete", &request)
        .await
        .unwrap();
    assert!(response.accepted);
    let captured: Vec<_> = received.iter().collect();
    assert_eq!(
        captured.len(),
        7,
        "healthy publication can span more than five bounded requests"
    );
    assert!(captured.iter().all(
        |sent| sent.idempotency_key == request.idempotency_key() && sent.body == request.body()
    ));
    server.join().unwrap();
}

#[tokio::test]
async fn completion_windows_preserve_accumulated_network_failures() {
    let (address, received, server) = raw_server([
        RawResponse::Status(500),
        RawResponse::CompletionWindow,
        RawResponse::Status(500),
    ]);
    let client = test_client(address, Some(2), Duration::ZERO, REQUEST_TIMEOUT);
    let request = RetryRequest::new(
        "failed-completion".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();
    let error = client
        .send_completion_windows::<TestResponse, _>("/complete", &request)
        .await
        .unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::ExternalSystemUnavailable);
    assert_eq!(
        received.iter().count(),
        3,
        "a healthy response window must not reset the two-error limit"
    );
    server.join().unwrap();
}

#[tokio::test]
async fn completion_windows_reject_malformed_timeout_envelopes() {
    let (address, received, server) = raw_server([RawResponse::Status(408)]);
    let client = test_client(address, Some(2), Duration::ZERO, REQUEST_TIMEOUT);
    let request = RetryRequest::new(
        "malformed-completion".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();
    assert!(
        client
            .send_completion_windows::<TestResponse, _>("/complete", &request)
            .await
            .is_err()
    );
    assert_eq!(
        received.iter().count(),
        1,
        "unvalidated 408 is not a completion continuation"
    );
    server.join().unwrap();
}

#[tokio::test]
async fn generic_timeout_still_spends_the_ordinary_retry_budget() {
    let (address, received, server) =
        raw_server([RawResponse::CompletionWindow, RawResponse::CompletionWindow]);
    let client = test_client(address, Some(2), Duration::ZERO, REQUEST_TIMEOUT);
    let request = RetryRequest::new(
        "generic-timeout".to_owned(),
        &TestBody {
            incarnation_id: "0123456789abcdef0123456789abcdef",
        },
    )
    .unwrap();
    assert!(
        client
            .send::<TestResponse, _>("/other", &request)
            .await
            .is_err()
    );
    assert_eq!(received.iter().count(), 2);
    server.join().unwrap();
}

#[test]
fn completion_window_rejects_mismatched_and_unknown_envelope_fields() {
    let valid = serde_json::json!({"schema_version":"0","command":"execution.complete","status":"error","data":null,"warnings":[],"error":{"code":"REQUEST_TIMEOUT","message":"window elapsed"}});
    assert!(super::validate_completion_window(&serde_json::to_vec(&valid).unwrap()).is_ok());
    for (field, value) in [
        ("schema_version", serde_json::json!("1")),
        ("command", serde_json::json!("other")),
        ("status", serde_json::json!("ok")),
        ("data", serde_json::json!({"completed":true})),
        ("unexpected", serde_json::json!(true)),
    ] {
        let mut changed = valid.clone();
        changed[field] = value;
        assert!(
            super::validate_completion_window(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "mismatched {field} must never continue completion"
        );
    }
    for code in ["CONFLICT", "UNKNOWN_CODE"] {
        let mut changed = valid.clone();
        changed["error"]["code"] = serde_json::json!(code);
        assert!(super::validate_completion_window(&serde_json::to_vec(&changed).unwrap()).is_err());
    }
    let mut changed = valid;
    changed["error"]["unexpected"] = serde_json::json!(true);
    assert!(super::validate_completion_window(&serde_json::to_vec(&changed).unwrap()).is_err());
}
