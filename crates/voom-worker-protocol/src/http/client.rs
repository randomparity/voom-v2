use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Buf, Bytes};
use http_body::Body;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::{Method, Request};
use hyper_util::rt::TokioExecutor;
use secrecy::ExposeSecret;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::transport::{ClientHandle, DispatchStream};
use crate::{
    HandshakeRequest, HandshakeResponse, NdjsonReader, OperationRequest, OperationResponse,
    ProtocolError, WorkerCredentials,
};

use super::{
    IDEMPOTENCY_KEY_HEADER, PROTOCOL_VERSION_HEADER, WORKER_EPOCH_HEADER, WORKER_ID_HEADER,
};

/// Bound on the full handshake round-trip. The handshake is a tiny
/// request/response, so a hung worker that never replies should be abandoned
/// quickly.
const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on a dispatch up to (and including) the first response line. The
/// NDJSON progress stream that follows is intentionally *not* bounded by this —
/// it can legitimately run for the length of a transcode and is governed by the
/// lease TTL and progress-idle timeouts instead.
const DEFAULT_DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

pub struct HttpClient {
    base: String,
    client: hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        Full<Bytes>,
    >,
    handshake_timeout: Duration,
    dispatch_timeout: Duration,
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
        Self::with_timeouts(base, DEFAULT_HANDSHAKE_TIMEOUT, DEFAULT_DISPATCH_TIMEOUT)
    }

    /// Construct a client with explicit request deadlines. Used by tests to
    /// drive the timeout paths without waiting the production defaults.
    #[must_use]
    pub fn with_timeouts(
        base: SocketAddr,
        handshake_timeout: Duration,
        dispatch_timeout: Duration,
    ) -> Self {
        let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
            .build(hyper_util::client::legacy::connect::HttpConnector::new());
        Self {
            base: format!("http://{base}"),
            client,
            handshake_timeout,
            dispatch_timeout,
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
        // Bound the whole handshake round-trip: a worker that accepts the
        // connection but never replies must not park this task indefinitely.
        let (status, body) = tokio::time::timeout(self.handshake_timeout, async {
            let resp =
                self.client
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
            Ok::<_, ProtocolError>((status, body))
        })
        .await
        .map_err(|_| ProtocolError::Timeout {
            elapsed: self.handshake_timeout,
            detail: "handshake: worker did not respond".to_owned(),
        })??;
        if status.is_success() {
            let response = serde_json::from_slice::<HandshakeResponse>(&body).map_err(|e| {
                ProtocolError::InvalidPayload {
                    detail: format!("decode: {e}"),
                }
            })?;
            // Defense in depth: the server is the authority (ADR-0016 exact
            // match), but a non-conforming 2xx that echoes a different version
            // must not be silently accepted as agreement.
            if response.agreed != offered {
                return Err(ProtocolError::UnsupportedProtocolVersion {
                    offered,
                    expected: response.agreed,
                });
            }
            return Ok(response);
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
        let requested_lease_id = request.lease_id;
        // Bound dispatch up to (and including) the first response line. A worker
        // that accepts the connection but never sends the OperationResponse line
        // would otherwise park this task forever. The NDJSON progress stream
        // built below is deliberately left outside this deadline — it can run
        // for the length of a transcode and is bounded by the lease TTL and
        // progress-idle timeouts instead.
        let (response, reader) = tokio::time::timeout(self.dispatch_timeout, async {
            let resp =
                self.client
                    .request(req)
                    .await
                    .map_err(|e| ProtocolError::InvalidPayload {
                        detail: format!("request: {e}"),
                    })?;
            let status = resp.status();
            if !status.is_success() {
                let collected = resp
                    .into_body()
                    .collect()
                    .await
                    .map_err(|e| ProtocolError::InvalidPayload {
                        detail: format!("body: {e}"),
                    })?
                    .to_bytes();
                let perr =
                    serde_json::from_slice::<ProtocolError>(&collected).unwrap_or_else(|_| {
                        ProtocolError::InvalidPayload {
                            detail: format!("dispatch failed status={status}"),
                        }
                    });
                return Err(perr);
            }
            let mut reader = IncomingAsyncRead::new(resp.into_body());
            let resp_line = read_response_line(&mut reader).await?;
            let response: OperationResponse =
                serde_json::from_slice(&resp_line).map_err(|e| ProtocolError::MalformedFrame {
                    detail: format!("response decode: {e}"),
                })?;
            Ok::<_, ProtocolError>((response, reader))
        })
        .await
        .map_err(|_| ProtocolError::Timeout {
            elapsed: self.dispatch_timeout,
            detail: "dispatch: worker did not send a response line".to_owned(),
        })??;
        if response.lease_id != requested_lease_id {
            return Err(ProtocolError::WrongLeaseId {
                expected: requested_lease_id,
                got: response.lease_id,
            });
        }
        let reader: Pin<Box<dyn AsyncRead + Send + Unpin>> = Box::pin(reader);
        let frames = NdjsonReader::new(reader, requested_lease_id);
        Ok(DispatchStream { response, frames })
    }
}

async fn read_response_line<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, ProtocolError> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        let n = reader
            .read(&mut byte)
            .await
            .map_err(|e| ProtocolError::MalformedFrame {
                detail: format!("response read: {e}"),
            })?;
        if n == 0 {
            return Err(ProtocolError::MalformedFrame {
                detail: "missing response/body separator".to_owned(),
            });
        }
        if byte[0] == b'\n' {
            return Ok(line);
        }
        line.push(byte[0]);
    }
}

struct IncomingAsyncRead {
    body: Incoming,
    current: Option<Bytes>,
}

impl IncomingAsyncRead {
    fn new(body: Incoming) -> Self {
        Self {
            body,
            current: None,
        }
    }
}

impl std::fmt::Debug for IncomingAsyncRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncomingAsyncRead").finish_non_exhaustive()
    }
}

impl AsyncRead for IncomingAsyncRead {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if let Some(current) = &mut self.current {
                if current.has_remaining() {
                    let n = current.remaining().min(buf.remaining());
                    buf.put_slice(&current.copy_to_bytes(n));
                    return Poll::Ready(Ok(()));
                }
                self.current = None;
            }

            match Pin::new(&mut self.body).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data()
                        && !data.is_empty()
                    {
                        self.current = Some(data);
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(std::io::Error::other(e)));
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
