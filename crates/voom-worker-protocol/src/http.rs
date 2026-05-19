//! HTTP/1.1 loopback transport over `hyper` 1.x. Phase 1 design §3.8.
//!
//! Scope this commit covers:
//! - `POST /v1/handshake` (exempt from version/auth/idempotency)
//! - `POST /v1/operations` (gated on version + auth; idempotency
//!   cache + body-scan deferred to a follow-up commit per the
//!   plan's scope discipline)
//! - 404 on unknown routes
//! - `OperationResponse` envelope as the first NDJSON line on the
//!   response body, followed by the actual progress frames
//!
//! Per the design, lease-callback routes (heartbeat/progress/cancel)
//! are Phase 2 supervisor-side work and are NOT in this commit.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderName};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as HttpAutoBuilder;
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use tokio::io::AsyncRead;
use tokio::net::TcpListener;

use crate::credentials::{PresentedCredentials, WorkerCredentials, validate_credentials};
use crate::envelope::{OperationRequest, OperationResponse, ProtocolError};
use crate::handshake::{HandshakeRequest, HandshakeResponse, negotiate};
use crate::ndjson::NdjsonReader;
use crate::transport::{ClientHandle, DispatchStream, ServerHandle, ServerRunning};

const PROTOCOL_VERSION_HEADER: &str = "x-voom-protocol-version";
const WORKER_ID_HEADER: &str = "x-voom-worker-id";
const WORKER_EPOCH_HEADER: &str = "x-voom-worker-epoch";
const IDEMPOTENCY_KEY_HEADER: &str = "x-voom-idempotency-key";

const MAX_BODY_BYTES: usize = 1 << 20; // 1 MiB

pub(crate) type ResponseBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

/// What the worker's handler returns from one `/v1/operations` call.
pub struct OperationDispatch {
    pub response: OperationResponse,
    /// NDJSON body bytes (concatenated frames, each terminated by `\n`).
    pub body: Vec<u8>,
}

impl std::fmt::Debug for OperationDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationDispatch")
            .field("response", &self.response)
            .field("body_len", &self.body.len())
            .finish()
    }
}

pub type OperationFuture =
    Pin<Box<dyn std::future::Future<Output = Result<OperationDispatch, ProtocolError>> + Send>>;

pub type OperationHandler = Arc<dyn Fn(OperationRequest) -> OperationFuture + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutePolicy {
    pub version: bool,
    pub auth: bool,
}

#[must_use]
pub fn route_policy(method: &Method, path: &str) -> Option<RoutePolicy> {
    match (method, path) {
        (&Method::POST, "/v1/handshake") => Some(RoutePolicy {
            version: false,
            auth: false,
        }),
        (&Method::POST, "/v1/operations") => Some(RoutePolicy {
            version: true,
            auth: true,
        }),
        _ => None,
    }
}

pub struct HttpServer {
    pub credentials: WorkerCredentials,
    pub operation_handler: OperationHandler,
}

impl std::fmt::Debug for HttpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpServer")
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

impl HttpServer {
    #[must_use]
    pub fn new(credentials: WorkerCredentials, operation_handler: OperationHandler) -> Self {
        Self {
            credentials,
            operation_handler,
        }
    }
}

#[async_trait]
impl ServerHandle for HttpServer {
    async fn serve(&self, addr: SocketAddr) -> Result<ServerRunning, ProtocolError> {
        let listener =
            TcpListener::bind(addr)
                .await
                .map_err(|e| ProtocolError::InvalidPayload {
                    detail: format!("bind failed: {e}"),
                })?;
        let bound = listener
            .local_addr()
            .map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("local_addr failed: {e}"),
            })?;

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let creds = self.credentials.clone();
        let handler = self.operation_handler.clone();

        let joined = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let Ok((stream, _peer)) = accept else { continue };
                        let creds = creds.clone();
                        let handler = handler.clone();
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let service = service_fn(move |req: Request<Incoming>| {
                                let creds = creds.clone();
                                let handler = handler.clone();
                                async move { handle_request(req, &creds, &handler).await }
                            });
                            let _ = HttpAutoBuilder::new(TokioExecutor::new())
                                .serve_connection(io, service)
                                .await;
                        });
                    }
                }
            }
        });

        Ok(ServerRunning {
            bound,
            shutdown: shutdown_tx,
            joined,
        })
    }
}

async fn handle_request(
    req: Request<Incoming>,
    creds: &WorkerCredentials,
    handler: &OperationHandler,
) -> Result<Response<ResponseBody>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();

    let Some(policy) = route_policy(&method, &path) else {
        return Ok(plain_status(StatusCode::NOT_FOUND, "not found"));
    };

    let (parts, body) = req.into_parts();

    if policy.version
        && let Err(e) = enforce_version(&parts.headers)
    {
        return Ok(json_error(StatusCode::BAD_REQUEST, &e));
    }

    if policy.auth {
        match parse_credentials(&parts.headers) {
            Ok(p) => match validate_credentials(&p, creds) {
                Ok(()) => {}
                Err(e) => return Ok(json_error(StatusCode::UNAUTHORIZED, &e)),
            },
            Err(e) => return Ok(json_error(StatusCode::UNAUTHORIZED, &e)),
        }
    }

    let body_bytes = match read_body(body).await {
        Ok(b) => b,
        Err(e) => return Ok(json_error(StatusCode::BAD_REQUEST, &e)),
    };

    Ok(match path.as_str() {
        "/v1/handshake" => handle_handshake(&body_bytes),
        "/v1/operations" => handle_operations(&body_bytes, handler).await,
        _ => plain_status(StatusCode::NOT_FOUND, "not found"),
    })
}

fn enforce_version(headers: &hyper::HeaderMap) -> Result<(), ProtocolError> {
    let v = headers
        .get(HeaderName::from_static(PROTOCOL_VERSION_HEADER))
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());
    match v {
        Some(n) if n == voom_core::PROTOCOL_VERSION => Ok(()),
        Some(n) => Err(ProtocolError::UnsupportedProtocolVersion {
            offered: n,
            supported_min: voom_core::PROTOCOL_VERSION_SUPPORTED_MIN,
            supported_max: voom_core::PROTOCOL_VERSION_SUPPORTED_MAX,
        }),
        None => Err(ProtocolError::InvalidPayload {
            detail: format!("missing {PROTOCOL_VERSION_HEADER}"),
        }),
    }
}

fn handle_handshake(body: &[u8]) -> Response<ResponseBody> {
    let req: HandshakeRequest = match decode_body(body) {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    match negotiate(req.offered) {
        Ok(resp) => json_ok(StatusCode::OK, &resp),
        Err(e) => json_error(StatusCode::BAD_REQUEST, &e),
    }
}

async fn handle_operations(body: &[u8], handler: &OperationHandler) -> Response<ResponseBody> {
    let req: OperationRequest = match decode_body(body) {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    let dispatched = match (handler)(req).await {
        Ok(d) => d,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    let Ok(resp_bytes) = serde_json::to_vec(&dispatched.response) else {
        return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "encode failed");
    };

    let mut combined = resp_bytes;
    combined.push(b'\n');
    combined.extend_from_slice(&dispatched.body);

    let body = Full::new(Bytes::from(combined))
        .map_err(|never: Infallible| match never {})
        .boxed();
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/x-ndjson")
        .body(body)
        .unwrap_or_else(|_| plain_status(StatusCode::INTERNAL_SERVER_ERROR, "build failed"))
}

fn parse_credentials(headers: &hyper::HeaderMap) -> Result<PresentedCredentials, ProtocolError> {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(ProtocolError::UnauthorizedBearer)?
        .to_owned();
    let worker_id = headers
        .get(HeaderName::from_static(WORKER_ID_HEADER))
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(ProtocolError::UnauthorizedBearer)?;
    let epoch = headers
        .get(HeaderName::from_static(WORKER_EPOCH_HEADER))
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(ProtocolError::UnauthorizedBearer)?;
    Ok(PresentedCredentials {
        worker_id: voom_core::WorkerId(worker_id),
        worker_epoch: epoch,
        secret: SecretString::from(bearer),
    })
}

async fn read_body(body: Incoming) -> Result<Bytes, ProtocolError> {
    let collected = body
        .collect()
        .await
        .map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("body collect: {e}"),
        })?;
    let bytes = collected.to_bytes();
    if bytes.len() > MAX_BODY_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            bytes: bytes.len() as u64,
            max: MAX_BODY_BYTES as u64,
        });
    }
    Ok(bytes)
}

fn decode_body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProtocolError> {
    serde_json::from_slice(bytes).map_err(|e| ProtocolError::InvalidPayload {
        detail: format!("json decode: {e}"),
    })
}

fn plain_status(status: StatusCode, msg: &'static str) -> Response<ResponseBody> {
    let body = Full::new(Bytes::from_static(msg.as_bytes()))
        .map_err(|never: Infallible| match never {})
        .boxed();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain")
        .body(body)
        .unwrap_or_else(|_| {
            let fallback = Full::new(Bytes::from_static(b"internal"))
                .map_err(|never: Infallible| match never {})
                .boxed();
            Response::new(fallback)
        })
}

fn json_ok<T: serde::Serialize>(status: StatusCode, v: &T) -> Response<ResponseBody> {
    let bytes = serde_json::to_vec(v).unwrap_or_default();
    let body = Full::new(Bytes::from(bytes))
        .map_err(|never: Infallible| match never {})
        .boxed();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap_or_else(|_| plain_status(StatusCode::INTERNAL_SERVER_ERROR, "build failed"))
}

fn json_error(status: StatusCode, e: &ProtocolError) -> Response<ResponseBody> {
    json_ok(status, e)
}

// =============================================================
// HttpClient — supervisor-side ClientHandle implementation.
// =============================================================

pub struct HttpClient {
    base: String,
    client: hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        Full<Bytes>,
    >,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    #[must_use]
    pub fn new(base: SocketAddr) -> Self {
        let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
            .build(hyper_util::client::legacy::connect::HttpConnector::new());
        Self {
            base: format!("http://{base}"),
            client,
        }
    }
}

#[async_trait]
impl ClientHandle for HttpClient {
    async fn handshake(&self, offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        let body = serde_json::to_vec(&HandshakeRequest { offered }).map_err(|e| {
            ProtocolError::InvalidPayload {
                detail: format!("encode: {e}"),
            }
        })?;
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("{}/v1/handshake", self.base))
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("build: {e}"),
            })?;
        let resp = self
            .client
            .request(req)
            .await
            .map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("request: {e}"),
            })?;
        let status = resp.status();
        let body = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("body: {e}"),
            })?
            .to_bytes();
        if status.is_success() {
            return serde_json::from_slice::<HandshakeResponse>(&body).map_err(|e| {
                ProtocolError::InvalidPayload {
                    detail: format!("decode: {e}"),
                }
            });
        }
        let perr = serde_json::from_slice::<ProtocolError>(&body).unwrap_or_else(|_| {
            ProtocolError::InvalidPayload {
                detail: format!("handshake failed status={status}"),
            }
        });
        Err(perr)
    }

    async fn dispatch(
        &self,
        creds: &WorkerCredentials,
        idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        let body_bytes =
            serde_json::to_vec(&request).map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("encode: {e}"),
            })?;
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("{}/v1/operations", self.base))
            .header(CONTENT_TYPE, "application/json")
            .header(PROTOCOL_VERSION_HEADER, voom_core::PROTOCOL_VERSION)
            .header(
                AUTHORIZATION,
                format!("Bearer {}", creds.secret.expose_secret()),
            )
            .header(WORKER_ID_HEADER, creds.worker_id.0.to_string())
            .header(WORKER_EPOCH_HEADER, creds.worker_epoch.to_string())
            .header(IDEMPOTENCY_KEY_HEADER, idempotency_key)
            .body(Full::new(Bytes::from(body_bytes)))
            .map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("build: {e}"),
            })?;
        let resp = self
            .client
            .request(req)
            .await
            .map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("request: {e}"),
            })?;
        let status = resp.status();
        let collected = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| ProtocolError::InvalidPayload {
                detail: format!("body: {e}"),
            })?
            .to_bytes();
        if !status.is_success() {
            let perr = serde_json::from_slice::<ProtocolError>(&collected).unwrap_or_else(|_| {
                ProtocolError::InvalidPayload {
                    detail: format!("dispatch failed status={status}"),
                }
            });
            return Err(perr);
        }
        let split =
            collected
                .iter()
                .position(|&b| b == b'\n')
                .ok_or(ProtocolError::MalformedFrame {
                    detail: "missing response/body separator".to_owned(),
                })?;
        let (resp_line, ndjson_tail) = collected.split_at(split);
        let response: OperationResponse =
            serde_json::from_slice(resp_line).map_err(|e| ProtocolError::MalformedFrame {
                detail: format!("response decode: {e}"),
            })?;
        let ndjson_bytes = ndjson_tail.get(1..).unwrap_or(&[]).to_vec();
        let lease_id = response.lease_id;
        let reader: Pin<Box<dyn AsyncRead + Send + Unpin>> = Box::pin(CursorAsync {
            inner: std::io::Cursor::new(ndjson_bytes),
        });
        let frames = NdjsonReader::new(reader, lease_id);
        Ok(DispatchStream { response, frames })
    }
}

struct CursorAsync {
    inner: std::io::Cursor<Vec<u8>>,
}

impl std::fmt::Debug for CursorAsync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorAsync").finish_non_exhaustive()
    }
}

impl AsyncRead for CursorAsync {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        use std::io::Read;
        let n = self.inner.read(buf.initialize_unfilled()).unwrap_or(0);
        buf.advance(n);
        Poll::Ready(Ok(()))
    }
}
