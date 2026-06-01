use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderName};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as HttpAutoBuilder;
use secrecy::SecretString;
use serde::de::DeserializeOwned;
use tokio::net::TcpListener;

use crate::transport::{ServerHandle, ServerRunning};
use crate::{
    HandshakeRequest, OperationRequest, PresentedCredentials, ProtocolError, WorkerCredentials,
    negotiate, validate_credentials,
};

use super::idempotency::{
    CachedResponse, IDEMPOTENCY_CACHE_CAPACITY, IdempotencyBegin, IdempotencyCache,
};
use super::streaming::{
    OperationBody, OperationDispatch, StreamingFinalizer, operation_response,
    operation_streaming_response,
};
use super::{
    IDEMPOTENCY_KEY_HEADER, PROTOCOL_VERSION_HEADER, ResponseBody, WORKER_EPOCH_HEADER,
    WORKER_ID_HEADER,
};

const MAX_BODY_BYTES: usize = 1 << 20; // 1 MiB

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
    idempotency_cache: Arc<Mutex<IdempotencyCache>>,
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
            idempotency_cache: Arc::new(Mutex::new(IdempotencyCache::new(
                IDEMPOTENCY_CACHE_CAPACITY,
            ))),
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
        let cache = self.idempotency_cache.clone();

        let joined = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let Ok((stream, _peer)) = accept else { continue };
                        let creds = creds.clone();
                        let handler = handler.clone();
                        let cache = cache.clone();
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let service = service_fn(move |req: Request<Incoming>| {
                                let creds = creds.clone();
                                let handler = handler.clone();
                                let cache = cache.clone();
                                async move { handle_request(req, &creds, &handler, &cache).await }
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
    cache: &Arc<Mutex<IdempotencyCache>>,
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
        "/v1/operations" => handle_operations(&parts.headers, &body_bytes, handler, cache).await,
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

async fn handle_operations(
    headers: &hyper::HeaderMap,
    body: &[u8],
    handler: &OperationHandler,
    cache: &Arc<Mutex<IdempotencyCache>>,
) -> Response<ResponseBody> {
    let idempotency_key = match require_idempotency_key(headers) {
        Ok(key) => key,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    if let Err(e) = validate_no_body_idempotency_key(body) {
        return json_error(StatusCode::BAD_REQUEST, &e);
    }
    let body_hash = blake3::hash(body);
    let body_hash_bytes = *body_hash.as_bytes();

    let req: OperationRequest = match decode_body(body) {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };
    let begin = match cache.lock() {
        Ok(mut cache) => cache.begin(idempotency_key.clone(), body_hash_bytes),
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &ProtocolError::InternalServerError,
            );
        }
    };
    match begin {
        IdempotencyBegin::Replay(cached) => {
            return operation_response(&cached.response, &cached.body);
        }
        IdempotencyBegin::Duplicate {
            key,
            original_status,
        } => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &ProtocolError::DuplicateIdempotencyKey {
                    key,
                    original_status,
                },
            );
        }
        IdempotencyBegin::AtCapacity => {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                &ProtocolError::ServiceAtCapacity,
            );
        }
        IdempotencyBegin::Started => {}
    }

    let dispatched = match (handler)(req).await {
        Ok(d) => d,
        Err(e) => {
            if let Ok(mut cache) = cache.lock() {
                cache.clear_active(&idempotency_key, body_hash_bytes);
            }
            return json_error(StatusCode::BAD_REQUEST, &e);
        }
    };
    match dispatched.body {
        OperationBody::Buffered(body) => {
            let cached_response = CachedResponse {
                response: dispatched.response.clone(),
                body: body.clone(),
            };
            match cache.lock() {
                Ok(mut cache) => {
                    cache.complete(&idempotency_key, body_hash_bytes, cached_response);
                }
                Err(_) => {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &ProtocolError::InternalServerError,
                    );
                }
            }
            operation_response(&dispatched.response, &body)
        }
        OperationBody::Streaming(body) => {
            let response = dispatched.response;
            let finalizer = StreamingFinalizer {
                cache: cache.clone(),
                key: idempotency_key,
                hash: body_hash_bytes,
                response: response.clone(),
            };
            if let Err(e) = body.set_finalizer(finalizer) {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e);
            }
            operation_streaming_response(&response, body)
        }
    }
}

pub(super) fn require_idempotency_key(headers: &hyper::HeaderMap) -> Result<String, ProtocolError> {
    let key = headers
        .get(HeaderName::from_static(IDEMPOTENCY_KEY_HEADER))
        .and_then(|h| h.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProtocolError::InvalidPayload {
            detail: format!("missing {IDEMPOTENCY_KEY_HEADER}"),
        })?;
    Ok(key.to_owned())
}

pub(super) fn validate_no_body_idempotency_key(body: &[u8]) -> Result<(), ProtocolError> {
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
