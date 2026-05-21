use bytes::Bytes;
use secrecy::ExposeSecret;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use voom_worker_protocol::low_level::raw_post_request;
use voom_worker_protocol::{OperationKind, OperationRequest, ProtocolError, WorkerCredentials};

pub async fn run_active_worker(launch: &mut crate::WorkerLaunch) -> crate::SuiteResult {
    let mut result = crate::SuiteResult::default();
    record_raw(
        &mut result,
        "golden_handshake_request_round_trips",
        golden_handshake_request_round_trips(launch),
    )
    .await;
    record_raw(
        &mut result,
        "golden_operation_request_round_trips",
        golden_operation_request_round_trips(launch),
    )
    .await;
    record_raw(
        &mut result,
        "missing_auth_headers_rejected",
        missing_auth_headers_rejected(launch),
    )
    .await;
    record_raw(
        &mut result,
        "wrong_bearer_header_rejected",
        wrong_bearer_header_rejected(launch),
    )
    .await;
    record_raw(
        &mut result,
        "wrong_worker_epoch_header_rejected",
        wrong_worker_epoch_header_rejected(launch),
    )
    .await;
    record_raw(
        &mut result,
        "malformed_json_rejected",
        malformed_json_rejected(launch),
    )
    .await;
    record_raw(
        &mut result,
        "wrong_content_length_rejected",
        wrong_content_length_rejected(launch),
    )
    .await;
    record_raw(
        &mut result,
        "unknown_route_returns_404",
        unknown_route_returns_404(launch),
    )
    .await;
    record_raw(
        &mut result,
        "handshake_version_skew_returns_structured_error",
        handshake_version_skew_returns_structured_error(launch),
    )
    .await;
    record_raw(
        &mut result,
        "idempotency_exact_byte_replay_returns_cached_response",
        idempotency_exact_byte_replay_returns_cached_response(launch),
    )
    .await;
    record_raw(
        &mut result,
        "idempotency_same_key_different_body_rejected",
        idempotency_same_key_different_body_rejected(launch),
    )
    .await;
    result
}

pub async fn run_protocol_negative_fixture() -> crate::SuiteResult {
    let mut result = crate::SuiteResult::default();
    record_fixture(
        &mut result,
        "frame_with_wrong_lease_id_rejected",
        crate::negative_fixture::FixtureMode::WrongLeaseId,
    )
    .await;
    record_fixture(
        &mut result,
        "frame_after_terminal_rejected",
        crate::negative_fixture::FixtureMode::FrameAfterTerminal,
    )
    .await;
    record_fixture(
        &mut result,
        "partial_response_body_classified",
        crate::negative_fixture::FixtureMode::TruncatedBody,
    )
    .await;
    result
}

async fn record_fixture(
    result: &mut crate::SuiteResult,
    name: &'static str,
    mode: crate::negative_fixture::FixtureMode,
) {
    match crate::negative_fixture::classify_fixture(mode).await {
        Err(_) => result.pass(name),
        Ok(()) => result.fail(name, "fixture was accepted"),
    }
}

async fn golden_handshake_request_round_trips(
    launch: &crate::WorkerLaunch,
) -> Result<(), String> {
    let body = serde_json::to_vec(&serde_json::json!({"offered": 1}))
        .map_err(|e| format!("handshake encode: {e}"))?;
    let response = send_raw(
        launch.bound,
        raw_post_request(&launch.bound.to_string(), "/v1/handshake", &body, &[]),
    )
    .await?;
    let parsed = RawHttpResponse::parse(&response)?;
    require_status_prefix(&parsed, "HTTP/1.1 200")
}

async fn golden_operation_request_round_trips(
    launch: &crate::WorkerLaunch,
) -> Result<(), String> {
    let response = send_operation(launch, "raw-valid", operation_body(30, "/library/raw.mkv")?)
        .await?;
    let parsed = RawHttpResponse::parse(&response)?;
    require_status_prefix(&parsed, "HTTP/1.1 200")?;
    if !parsed.is_success() {
        return Err("handshake response was not successful".to_owned());
    }
    if parsed.body.windows(b"\"lease_id\":".len()).any(|w| w == b"\"lease_id\":") {
        Ok(())
    } else {
        Err("operation response body missing lease_id".to_owned())
    }
}

async fn missing_auth_headers_rejected(launch: &crate::WorkerLaunch) -> Result<(), String> {
    let body = operation_body(31, "/library/missing-auth.mkv")?;
    let response = send_raw(
        launch.bound,
        raw_post_request(&launch.bound.to_string(), "/v1/operations", &body, &[]),
    )
    .await?;
    let err = RawHttpResponse::parse(&response)?.protocol_error()?;
    match err {
        ProtocolError::InvalidPayload { .. } | ProtocolError::UnauthorizedBearer => Ok(()),
        other => Err(format!("expected auth/version rejection, got {other:?}")),
    }
}

async fn wrong_bearer_header_rejected(launch: &crate::WorkerLaunch) -> Result<(), String> {
    let mut creds = launch.credentials.clone();
    creds.secret = secrecy::SecretString::from("wrong");
    let response = send_operation_with_creds(
        launch,
        &creds,
        "raw-wrong-bearer",
        operation_body(32, "/library/wrong-bearer.mkv")?,
    )
    .await?;
    match RawHttpResponse::parse(&response)?.protocol_error()? {
        ProtocolError::UnauthorizedBearer => Ok(()),
        other => Err(format!("expected UnauthorizedBearer, got {other:?}")),
    }
}

async fn wrong_worker_epoch_header_rejected(launch: &crate::WorkerLaunch) -> Result<(), String> {
    let mut creds = launch.credentials.clone();
    creds.worker_epoch += 1;
    let response = send_operation_with_creds(
        launch,
        &creds,
        "raw-wrong-worker-epoch",
        operation_body(33, "/library/wrong-epoch.mkv")?,
    )
    .await?;
    match RawHttpResponse::parse(&response)?.protocol_error()? {
        ProtocolError::StaleWorkerEpoch { .. } => Ok(()),
        other => Err(format!("expected StaleWorkerEpoch, got {other:?}")),
    }
}

async fn malformed_json_rejected(launch: &crate::WorkerLaunch) -> Result<(), String> {
    let response = send_operation(launch, "raw-malformed-json", malformed_json_body().to_vec())
        .await?;
    match RawHttpResponse::parse(&response)?.protocol_error()? {
        ProtocolError::InvalidPayload { .. } => Ok(()),
        other => Err(format!("expected InvalidPayload, got {other:?}")),
    }
}

async fn wrong_content_length_rejected(launch: &crate::WorkerLaunch) -> Result<(), String> {
    let body = operation_body(34, "/library/wrong-length.mkv")?;
    let header_values = auth_headers(&launch.credentials, "raw-wrong-length");
    let headers = header_values
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("POST /v1/operations HTTP/1.1\r\nHost: {}\r\n", launch.bound).as_bytes());
    bytes.extend_from_slice(format!("Content-Length: {}\r\n", body.len() + 64).as_bytes());
    for (k, v) in headers {
        bytes.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(&body);

    match send_raw(launch.bound, Bytes::from(bytes)).await {
        Ok(response) if response.starts_with(b"HTTP/1.1 200") => {
            Err("wrong content-length was accepted".to_owned())
        }
        Ok(_) | Err(_) => Ok(()),
    }
}

async fn unknown_route_returns_404(launch: &crate::WorkerLaunch) -> Result<(), String> {
    let body = serde_json::to_vec(&serde_json::json!({})).map_err(|e| e.to_string())?;
    let response = send_raw(
        launch.bound,
        raw_post_request(&launch.bound.to_string(), "/v1/unknown", &body, &[]),
    )
    .await?;
    let parsed = RawHttpResponse::parse(&response)?;
    require_status_prefix(&parsed, "HTTP/1.1 404")
}

async fn handshake_version_skew_returns_structured_error(
    launch: &crate::WorkerLaunch,
) -> Result<(), String> {
    let body = serde_json::to_vec(&serde_json::json!({"offered": 0}))
        .map_err(|e| format!("handshake encode: {e}"))?;
    let response = send_raw(
        launch.bound,
        raw_post_request(&launch.bound.to_string(), "/v1/handshake", &body, &[]),
    )
    .await?;
    match RawHttpResponse::parse(&response)?.protocol_error()? {
        ProtocolError::UnsupportedProtocolVersion { .. } => Ok(()),
        other => Err(format!("expected UnsupportedProtocolVersion, got {other:?}")),
    }
}

async fn idempotency_exact_byte_replay_returns_cached_response(
    launch: &crate::WorkerLaunch,
) -> Result<(), String> {
    let request = operation_request(
        35,
        "/library/raw-replay.mkv",
        Some("raw-replay".to_owned()),
    )?;
    let first = send_raw(launch.bound, request.clone()).await?;
    let second = send_raw(launch.bound, request).await?;
    let first = RawHttpResponse::parse(&first)?;
    let second = RawHttpResponse::parse(&second)?;
    require_status_prefix(&first, "HTTP/1.1 200")?;
    require_status_prefix(&second, "HTTP/1.1 200")?;
    if first.body == second.body {
        Ok(())
    } else {
        Err("cached replay body differed".to_owned())
    }
}

async fn idempotency_same_key_different_body_rejected(
    launch: &crate::WorkerLaunch,
) -> Result<(), String> {
    let first = send_operation(
        launch,
        "raw-replay-conflict",
        operation_body(36, "/library/raw-one.mkv")?,
    )
    .await?;
    require_status_prefix(&RawHttpResponse::parse(&first)?, "HTTP/1.1 200")?;
    let second = send_operation(
        launch,
        "raw-replay-conflict",
        operation_body(36, "/library/raw-two.mkv")?,
    )
    .await?;
    match RawHttpResponse::parse(&second)?.protocol_error()? {
        ProtocolError::DuplicateIdempotencyKey { .. } => Ok(()),
        other => Err(format!("expected DuplicateIdempotencyKey, got {other:?}")),
    }
}

async fn send_operation(
    launch: &crate::WorkerLaunch,
    idempotency_key: &str,
    body: Vec<u8>,
) -> Result<Vec<u8>, String> {
    send_operation_with_creds(launch, &launch.credentials, idempotency_key, body).await
}

async fn send_operation_with_creds(
    launch: &crate::WorkerLaunch,
    creds: &WorkerCredentials,
    idempotency_key: &str,
    body: Vec<u8>,
) -> Result<Vec<u8>, String> {
    send_raw(
        launch.bound,
        operation_request_with_creds(launch, creds, idempotency_key, &body),
    )
    .await
}

fn operation_request(
    lease_id: u64,
    path: &str,
    idempotency_key: Option<String>,
) -> Result<Bytes, String> {
    let body = operation_body(lease_id, path)?;
    let creds = WorkerCredentials {
        worker_id: voom_core::WorkerId(1),
        worker_epoch: 0,
        secret: secrecy::SecretString::from("phase1-bootstrap-secret"),
    };
    let headers = auth_headers(&creds, idempotency_key.as_deref().unwrap_or("raw"));
    let header_refs = headers
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .collect::<Vec<_>>();
    Ok(raw_post_request(
        "127.0.0.1",
        "/v1/operations",
        &body,
        &header_refs,
    ))
}

fn operation_request_with_creds(
    launch: &crate::WorkerLaunch,
    creds: &WorkerCredentials,
    idempotency_key: &str,
    body: &[u8],
) -> Bytes {
    let headers = auth_headers(creds, idempotency_key);
    let header_refs = headers
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .collect::<Vec<_>>();
    raw_post_request(
        &launch.bound.to_string(),
        "/v1/operations",
        body,
        &header_refs,
    )
}

fn operation_body(lease_id: u64, path: &str) -> Result<Vec<u8>, String> {
    let request = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(lease_id),
        payload: serde_json::json!({ "path": path }),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };
    serde_json::to_vec(&request).map_err(|e| format!("operation encode: {e}"))
}

fn auth_headers(creds: &WorkerCredentials, idempotency_key: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "X-Voom-Protocol-Version",
            voom_core::PROTOCOL_VERSION.to_string(),
        ),
        (
            "Authorization",
            format!("Bearer {}", creds.secret.expose_secret()),
        ),
        ("X-Voom-Worker-Id", creds.worker_id.0.to_string()),
        ("X-Voom-Worker-Epoch", creds.worker_epoch.to_string()),
        ("X-Voom-Idempotency-Key", idempotency_key.to_owned()),
    ]
}

fn malformed_json_body() -> &'static [u8] {
    b"{not-json"
}

async fn record_raw<F>(result: &mut crate::SuiteResult, name: &'static str, fut: F)
where
    F: std::future::Future<Output = Result<(), String>>,
{
    match fut.await {
        Ok(()) => result.pass(name),
        Err(e) => result.fail(name, e),
    }
}

async fn send_raw(addr: std::net::SocketAddr, bytes: Bytes) -> Result<Vec<u8>, String> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async move {
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        stream
            .write_all(&bytes)
            .await
            .map_err(|e| format!("write: {e}"))?;
        let mut out = Vec::new();
        let mut buf = [0_u8; 1024];
        let header_len = loop {
            if let Some(split) = out.windows(4).position(|w| w == b"\r\n\r\n") {
                break split + 4;
            }
            let n = stream
                .read(&mut buf)
                .await
                .map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&buf[..n]);
        };
        let body_len = content_length(&out[..header_len])?;
        while out.len() < header_len + body_len {
            let n = stream
                .read(&mut buf)
                .await
                .map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        Ok(out)
    })
    .await
    .map_err(|_| "raw HTTP response timed out".to_owned())?
}

fn content_length(headers: &[u8]) -> Result<usize, String> {
    let headers = String::from_utf8_lossy(headers);
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse::<usize>()
                .map_err(|e| format!("content-length parse: {e}"));
        }
    }
    Err("response missing content-length".to_owned())
}

fn require_status_prefix(response: &RawHttpResponse, prefix: &str) -> Result<(), String> {
    if response.status_line.starts_with(prefix) {
        Ok(())
    } else {
        Err(format!("expected status {prefix}, got {}", response.status_line))
    }
}

#[derive(Debug)]
struct RawHttpResponse {
    status_line: String,
    body: Vec<u8>,
}

impl RawHttpResponse {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let split = bytes
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "missing header/body split".to_owned())?;
        let headers = String::from_utf8_lossy(&bytes[..split]);
        let status_line = headers.lines().next().unwrap_or_default().to_owned();
        Ok(Self {
            status_line,
            body: bytes[split + 4..].to_vec(),
        })
    }

    fn is_success(&self) -> bool {
        self.status_line.starts_with("HTTP/1.1 2")
    }

    fn protocol_error(&self) -> Result<ProtocolError, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("protocol error decode: {e}"))
    }
}

#[cfg(test)]
#[path = "raw_wire_suite_test.rs"]
mod tests;
