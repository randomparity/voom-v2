#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "chaos-worker tests use direct fixture assertions"
    )
)]
#![expect(
    clippy::print_stdout,
    reason = "chaos-worker advertises readiness with BOUND addr=..."
)]
#![expect(
    clippy::exit,
    reason = "chaos-worker crash mode intentionally terminates the worker process"
)]

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use voom_worker_protocol::{
    HandshakeRequest, OperationKind, OperationRequest, OperationResponse, ProgressFrame,
    ProtocolError, WorkerCredentials, WorkerStartupError, load_worker_bind_addr_from_env,
    load_worker_credentials_from_env,
};

const MAX_DURATION_MS: u64 = 30_000;
const PROTOCOL_VERSION_HEADER: &str = "x-voom-protocol-version";
const WORKER_ID_HEADER: &str = "x-voom-worker-id";
const WORKER_EPOCH_HEADER: &str = "x-voom-worker-epoch";
const IDEMPOTENCY_KEY_HEADER: &str = "x-voom-idempotency-key";
const MAX_BODY_BYTES: usize = 1 << 20;
const IDEMPOTENCY_CACHE_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChaosMode {
    Baseline,
    Crash,
    Stall,
    MalformedResult,
    NonConvergingProgress,
    DeadlineExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChaosPayload {
    path: String,
    mode: ChaosMode,
    progress_count: usize,
    progress_interval: Duration,
    stall: Duration,
}

#[derive(Debug, Deserialize)]
struct RawChaosPayload {
    path: Option<String>,
    mode: Option<String>,
    progress_count: Option<u64>,
    progress_interval_ms: Option<u64>,
    stall_ms: Option<u64>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), WorkerStartupError> {
    let credentials = load_worker_credentials_from_env()?;
    let bind = load_worker_bind_addr_from_env()?;
    let listener = TcpListener::bind(bind)
        .await
        .map_err(|source| WorkerStartupError::bind(bind, source))?;
    let bound = listener
        .local_addr()
        .map_err(|source| WorkerStartupError::io("read bound worker address", source))?;
    println!("BOUND addr={bound}");
    let cache = std::sync::Arc::new(tokio::sync::Mutex::new(IdempotencyCache::new(
        IDEMPOTENCY_CACHE_CAPACITY,
    )));
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let watchdog = tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(_)) = stdin.next_line().await {}
        let _ = shutdown_tx.send(());
    });
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let credentials = credentials.clone();
                let cache = cache.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, credentials, cache).await;
                });
            }
        }
    }
    let _ = watchdog.await;
    Ok(())
}

async fn handle_connection(
    mut stream: TcpStream,
    credentials: WorkerCredentials,
    cache: std::sync::Arc<tokio::sync::Mutex<IdempotencyCache>>,
) -> Result<(), ProtocolError> {
    let request = read_http_request(&mut stream).await?;
    let response = route_request(&request, &credentials, cache).await?;
    write_response(&mut stream, response).await
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Debug)]
enum ChaosResponse {
    Fixed {
        status: &'static str,
        content_type: &'static str,
        body: Vec<u8>,
    },
    Open {
        prefix: Vec<u8>,
        chunks: Vec<(Vec<u8>, Duration)>,
        hold: Duration,
    },
    ExitProcess(i32),
}

async fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, ProtocolError> {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 1024];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("read headers: {e}"),
            })?;
        if n == 0 {
            return Err(ProtocolError::InvalidPayload {
                detail: "connection closed before headers".to_owned(),
            });
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_BODY_BYTES {
            return Err(ProtocolError::FrameTooLarge {
                bytes: buf.len() as u64,
                max: MAX_BODY_BYTES as u64,
            });
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.lines();
    let request_line = lines.next().ok_or_else(|| ProtocolError::InvalidPayload {
        detail: "missing request line".to_owned(),
    })?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<Vec<_>>();
    let content_length = header(&headers, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            bytes: content_length as u64,
            max: MAX_BODY_BYTES as u64,
        });
    }
    while buf.len() < header_end + content_length {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("read body: {e}"),
            })?;
        if n == 0 {
            return Err(ProtocolError::InvalidPayload {
                detail: "connection closed before full body".to_owned(),
            });
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: buf[header_end..buf.len().min(header_end + content_length)].to_vec(),
    })
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

async fn route_request(
    req: &HttpRequest,
    credentials: &WorkerCredentials,
    cache: std::sync::Arc<tokio::sync::Mutex<IdempotencyCache>>,
) -> Result<ChaosResponse, ProtocolError> {
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/v1/handshake") => Ok(handle_handshake(&req.body)),
        ("POST", "/v1/operations") => handle_operation(req, credentials, cache).await,
        _ => Ok(ChaosResponse::Fixed {
            status: "404 Not Found",
            content_type: "text/plain",
            body: b"not found".to_vec(),
        }),
    }
}

fn handle_handshake(body: &[u8]) -> ChaosResponse {
    let parsed = serde_json::from_slice::<HandshakeRequest>(body).map_err(|e| {
        ProtocolError::InvalidPayload {
            detail: format!("json decode: {e}"),
        }
    });
    match parsed.and_then(|req| voom_worker_protocol::negotiate(req.offered)) {
        Ok(resp) => json_response("200 OK", &resp),
        Err(err) => json_response("400 Bad Request", &err),
    }
}

async fn handle_operation(
    http: &HttpRequest,
    credentials: &WorkerCredentials,
    cache: std::sync::Arc<tokio::sync::Mutex<IdempotencyCache>>,
) -> Result<ChaosResponse, ProtocolError> {
    if let Err(err) = enforce_version(&http.headers) {
        return Ok(json_response("400 Bad Request", &err));
    }
    if let Err(err) = enforce_credentials(&http.headers, credentials) {
        return Ok(json_response("401 Unauthorized", &err));
    }
    let Some(idempotency_key) = header(&http.headers, IDEMPOTENCY_KEY_HEADER).map(str::to_owned)
    else {
        return Ok(json_response(
            "400 Bad Request",
            &ProtocolError::InvalidPayload {
                detail: format!("missing {IDEMPOTENCY_KEY_HEADER}"),
            },
        ));
    };
    if let Err(err) = reject_body_idempotency_key(&http.body) {
        return Ok(json_response("400 Bad Request", &err));
    }
    let body_hash = *blake3::hash(&http.body).as_bytes();
    if let Some(cached) = cache.lock().await.lookup(&idempotency_key, body_hash) {
        return Ok(cached);
    }
    if cache.lock().await.is_conflict(&idempotency_key, body_hash) {
        return Ok(json_response(
            "400 Bad Request",
            &ProtocolError::DuplicateIdempotencyKey {
                key: idempotency_key,
                original_status: "completed".to_owned(),
            },
        ));
    }
    let request = match serde_json::from_slice::<OperationRequest>(&http.body) {
        Ok(request) => request,
        Err(e) => {
            return Ok(json_response(
                "400 Bad Request",
                &ProtocolError::InvalidPayload {
                    detail: format!("json decode: {e}"),
                },
            ));
        }
    };
    let cacheable = request
        .payload
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|mode| mode == "baseline");
    let response = dispatch_operation(&request)?;
    if cacheable && matches!(response, ChaosResponse::Fixed { .. }) {
        cache
            .lock()
            .await
            .record(idempotency_key, body_hash, response.clone());
    }
    Ok(response)
}

fn dispatch_operation(req: &OperationRequest) -> Result<ChaosResponse, ProtocolError> {
    if req.operation != OperationKind::ProbeFile {
        return Ok(json_response(
            "400 Bad Request",
            &ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            },
        ));
    }
    let payload = match parse_payload(req.payload.clone()) {
        Ok(payload) => payload,
        Err(err) => return Ok(json_response("400 Bad Request", &err)),
    };
    match payload.mode {
        ChaosMode::Baseline => fixed_operation_response(req, &baseline_body(req, &payload)?),
        mode => streaming_or_fault_response(req, &payload, mode),
    }
}

fn parse_payload(value: serde_json::Value) -> Result<ChaosPayload, ProtocolError> {
    let raw: RawChaosPayload =
        serde_json::from_value(value).map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("chaos payload decode: {e}"),
        })?;
    let path = raw
        .path
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| ProtocolError::InvalidPayload {
            detail: "payload missing path".to_owned(),
        })?;
    let mode = match raw.mode.as_deref().unwrap_or("baseline") {
        "baseline" => ChaosMode::Baseline,
        "crash" => ChaosMode::Crash,
        "stall" => ChaosMode::Stall,
        "malformed_result" => ChaosMode::MalformedResult,
        "non_converging_progress" => ChaosMode::NonConvergingProgress,
        "deadline_exceeded" => ChaosMode::DeadlineExceeded,
        other => {
            return Err(ProtocolError::InvalidPayload {
                detail: format!("unknown chaos mode {other}"),
            });
        }
    };
    let progress_count = raw.progress_count.unwrap_or(3);
    if progress_count > 128 {
        return Err(ProtocolError::InvalidPayload {
            detail: "progress_count > 128".to_owned(),
        });
    }
    let progress_count =
        usize::try_from(progress_count).map_err(|_| ProtocolError::InvalidPayload {
            detail: "progress_count cannot fit usize".to_owned(),
        })?;
    let progress_interval = checked_duration("progress_interval_ms", raw.progress_interval_ms, 50)?;
    let stall = checked_duration("stall_ms", raw.stall_ms, 500)?;
    Ok(ChaosPayload {
        path,
        mode,
        progress_count,
        progress_interval,
        stall,
    })
}

fn checked_duration(
    field: &'static str,
    value: Option<u64>,
    default_ms: u64,
) -> Result<Duration, ProtocolError> {
    let ms = value.unwrap_or(default_ms);
    if ms > MAX_DURATION_MS {
        return Err(ProtocolError::InvalidPayload {
            detail: format!("{field} > {MAX_DURATION_MS}"),
        });
    }
    Ok(Duration::from_millis(ms))
}

impl Clone for ChaosResponse {
    fn clone(&self) -> Self {
        match self {
            Self::Fixed {
                status,
                content_type,
                body,
            } => Self::Fixed {
                status,
                content_type,
                body: body.clone(),
            },
            Self::Open {
                prefix,
                chunks,
                hold,
            } => Self::Open {
                prefix: prefix.clone(),
                chunks: chunks.clone(),
                hold: *hold,
            },
            Self::ExitProcess(code) => Self::ExitProcess(*code),
        }
    }
}

#[derive(Debug)]
struct CacheEntry {
    hash: [u8; 32],
    response: ChaosResponse,
}

#[derive(Debug)]
struct IdempotencyCache {
    capacity: usize,
    order: VecDeque<String>,
    entries: HashMap<String, CacheEntry>,
}

impl IdempotencyCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    fn lookup(&self, key: &str, hash: [u8; 32]) -> Option<ChaosResponse> {
        self.entries
            .get(key)
            .filter(|entry| entry.hash == hash)
            .map(|entry| entry.response.clone())
    }

    fn is_conflict(&self, key: &str, hash: [u8; 32]) -> bool {
        self.entries
            .get(key)
            .is_some_and(|entry| entry.hash != hash)
    }

    fn record(&mut self, key: String, hash: [u8; 32], response: ChaosResponse) {
        if self.capacity == 0 || self.entries.contains_key(&key) {
            return;
        }
        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, CacheEntry { hash, response });
    }
}

fn enforce_version(headers: &[(String, String)]) -> Result<(), ProtocolError> {
    let offered = header(headers, PROTOCOL_VERSION_HEADER)
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| ProtocolError::InvalidPayload {
            detail: format!("missing {PROTOCOL_VERSION_HEADER}"),
        })?;
    if offered == voom_core::PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::UnsupportedProtocolVersion {
            offered,
            supported_min: voom_core::PROTOCOL_VERSION_SUPPORTED_MIN,
            supported_max: voom_core::PROTOCOL_VERSION_SUPPORTED_MAX,
        })
    }
}

fn enforce_credentials(
    headers: &[(String, String)],
    credentials: &WorkerCredentials,
) -> Result<(), ProtocolError> {
    let bearer = header(headers, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(ProtocolError::UnauthorizedBearer)?
        .to_owned();
    let worker_id = header(headers, WORKER_ID_HEADER)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(ProtocolError::UnauthorizedBearer)?;
    let worker_epoch = header(headers, WORKER_EPOCH_HEADER)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(ProtocolError::UnauthorizedBearer)?;
    voom_worker_protocol::validate_credentials(
        &voom_worker_protocol::PresentedCredentials {
            worker_id: voom_core::WorkerId(worker_id),
            worker_epoch,
            secret: SecretString::from(bearer),
        },
        credentials,
    )
}

fn reject_body_idempotency_key(body: &[u8]) -> Result<(), ProtocolError> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("json decode: {e}"),
        })?;
    if contains_idempotency_key(&value) {
        Err(ProtocolError::HeaderBodyKeyMismatch)
    } else {
        Ok(())
    }
}

fn contains_idempotency_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(k, v)| k == "idempotency_key" || contains_idempotency_key(v)),
        serde_json::Value::Array(values) => values.iter().any(contains_idempotency_key),
        _ => false,
    }
}

fn json_response<T: serde::Serialize>(status: &'static str, value: &T) -> ChaosResponse {
    let body = serde_json::to_vec(value).unwrap_or_default();
    ChaosResponse::Fixed {
        status,
        content_type: "application/json",
        body,
    }
}

fn baseline_body(req: &OperationRequest, payload: &ChaosPayload) -> Result<Vec<u8>, ProtocolError> {
    let now = OffsetDateTime::now_utc();
    let progress = ProgressFrame::Progress {
        lease_id: req.lease_id,
        seq: 0,
        emitted_at: now,
        percent: None,
        message: Some(format!("chaos baseline {}", payload.path)),
        payload: Some(serde_json::json!({"mode": "baseline", "path": payload.path})),
    };
    let result = ProgressFrame::Result {
        lease_id: req.lease_id,
        seq: 1,
        emitted_at: now,
        payload: serde_json::json!({"mode": "baseline", "path": payload.path}),
    };
    let mut body = Vec::new();
    push_frame(&mut body, &progress)?;
    push_frame(&mut body, &result)?;
    Ok(body)
}

fn malformed_body() -> Vec<u8> {
    b"{not-json\n".to_vec()
}

fn progress_body(req: &OperationRequest, count: usize) -> Result<Vec<u8>, ProtocolError> {
    let mut body = Vec::new();
    for seq in 0..count {
        let frame = ProgressFrame::Progress {
            lease_id: req.lease_id,
            seq: seq as u64,
            emitted_at: OffsetDateTime::now_utc(),
            percent: None,
            message: Some("chaos progress".to_owned()),
            payload: Some(serde_json::json!({"mode": "progress"})),
        };
        push_frame(&mut body, &frame)?;
    }
    Ok(body)
}

fn fixed_operation_response(
    req: &OperationRequest,
    body: &[u8],
) -> Result<ChaosResponse, ProtocolError> {
    let mut framed = operation_response_line(req)?;
    framed.extend_from_slice(body);
    Ok(ChaosResponse::Fixed {
        status: "200 OK",
        content_type: "application/x-ndjson",
        body: framed,
    })
}

fn operation_response_line(req: &OperationRequest) -> Result<Vec<u8>, ProtocolError> {
    let response = OperationResponse {
        lease_id: req.lease_id,
        accepted_at: OffsetDateTime::now_utc(),
    };
    let mut out = serde_json::to_vec(&response).map_err(|e| ProtocolError::InvalidPayload {
        detail: format!("response encode: {e}"),
    })?;
    out.push(b'\n');
    Ok(out)
}

async fn write_response(
    stream: &mut TcpStream,
    response: ChaosResponse,
) -> Result<(), ProtocolError> {
    match response {
        ChaosResponse::Fixed {
            status,
            content_type,
            body,
        } => {
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(head.as_bytes())
                .await
                .map_err(|e| write_err(&e))?;
            stream.write_all(&body).await.map_err(|e| write_err(&e))?;
        }
        ChaosResponse::Open {
            prefix,
            chunks,
            hold,
        } => {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: keep-alive\r\n\r\n")
                .await
                .map_err(|e| write_err(&e))?;
            stream.write_all(&prefix).await.map_err(|e| write_err(&e))?;
            for (chunk, delay) in chunks {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                stream.write_all(&chunk).await.map_err(|e| write_err(&e))?;
            }
            tokio::time::sleep(hold).await;
        }
        ChaosResponse::ExitProcess(code) => std::process::exit(code),
    }
    Ok(())
}

fn write_err(e: &std::io::Error) -> ProtocolError {
    ProtocolError::MalformedFrame {
        detail: format!("write: {e}"),
    }
}

fn streaming_or_fault_response(
    req: &OperationRequest,
    payload: &ChaosPayload,
    mode: ChaosMode,
) -> Result<ChaosResponse, ProtocolError> {
    match mode {
        ChaosMode::Crash => Ok(ChaosResponse::ExitProcess(101)),
        ChaosMode::MalformedResult => {
            let mut body = operation_response_line(req)?;
            body.extend_from_slice(&malformed_body());
            Ok(ChaosResponse::Fixed {
                status: "200 OK",
                content_type: "application/x-ndjson",
                body,
            })
        }
        ChaosMode::Stall => Ok(ChaosResponse::Open {
            prefix: operation_response_line(req)?,
            chunks: Vec::new(),
            hold: payload.stall,
        }),
        ChaosMode::NonConvergingProgress => Ok(ChaosResponse::Open {
            prefix: operation_response_line(req)?,
            chunks: vec![(progress_body(req, payload.progress_count)?, Duration::ZERO)],
            hold: payload.stall,
        }),
        ChaosMode::DeadlineExceeded => Ok(ChaosResponse::Open {
            prefix: operation_response_line(req)?,
            chunks: vec![(
                progress_body(req, payload.progress_count)?,
                payload.progress_interval,
            )],
            hold: payload.stall,
        }),
        ChaosMode::Baseline => fixed_operation_response(req, &baseline_body(req, payload)?),
    }
}

fn push_frame(out: &mut Vec<u8>, frame: &ProgressFrame) -> Result<(), ProtocolError> {
    out.extend_from_slice(&serde_json::to_vec(frame).map_err(|e| {
        ProtocolError::InvalidPayload {
            detail: format!("frame encode: {e}"),
        }
    })?);
    out.push(b'\n');
    Ok(())
}

#[cfg(test)]
#[path = "chaos_worker_test.rs"]
mod tests;
