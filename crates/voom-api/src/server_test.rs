use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::header::CONTENT_TYPE;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use clap::Parser;
use http_body::{Body as HttpBody, Frame};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tower::ServiceExt;

use super::{DeadlineStream, RunningServer, bounded_router};
use crate::config::{Cli, ServerLimits};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn test_limits() -> Result<ServerLimits, voom_core::VoomError> {
    ServerLimits::new_for_test(
        1024 * 1024,
        Duration::from_secs(30),
        Duration::from_secs(30),
        Duration::from_secs(30),
        Duration::from_secs(90),
        Duration::from_secs(30),
    )
}

fn boundary_router() -> Result<Router, voom_core::VoomError> {
    let router = Router::new()
        .route(
            "/body",
            post(|body: Bytes| async move { body.len().to_string() }),
        )
        .route(
            "/slow",
            get(|| async { std::future::pending::<()>().await }),
        )
        .route("/ok", get(|| async { "ok" }));
    Ok(bounded_router(router, test_limits()?))
}

async fn response_json(response: axum::response::Response) -> TestResult {
    let status = response.status();
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let body = response.into_body().collect().await?.to_bytes();
    let actual: Value = serde_json::from_slice(&body)?;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        content_type.as_ref().and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        actual,
        json!({
            "schema_version": "0",
            "command": "api.request",
            "status": "error",
            "data": null,
            "warnings": [],
            "error": {
                "code": "PAYLOAD_TOO_LARGE",
                "message": "request body exceeds the 1048576-byte limit",
                "hint": "Send a request body of 1048576 bytes or fewer"
            }
        })
    );
    Ok(())
}

async fn timeout_response_json(response: axum::response::Response) -> TestResult {
    let status = response.status();
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let body = response.into_body().collect().await?.to_bytes();
    let actual: Value = serde_json::from_slice(&body)?;

    assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        content_type.as_ref().and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        actual,
        json!({
            "schema_version": "0",
            "command": "api.request",
            "status": "error",
            "data": null,
            "warnings": [],
            "error": {
                "code": "REQUEST_TIMEOUT",
                "message": "request processing exceeded the 30-second deadline",
                "hint": "Retry a mutation with the same idempotency key if its outcome is unknown"
            }
        })
    );
    Ok(())
}

#[tokio::test]
async fn boundary_body_limit_uses_exact_json_envelope() -> TestResult {
    let request = Request::post("/body").body(Body::from(vec![0_u8; 1024 * 1024 + 1]))?;
    let response = boundary_router()?.oneshot(request).await?;

    response_json(response).await
}

#[tokio::test]
async fn boundary_pending_handler_uses_exact_timeout_envelope() -> TestResult {
    tokio::time::pause();
    let task = tokio::spawn(boundary_router()?.oneshot(Request::get("/slow").body(Body::empty())?));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;

    timeout_response_json(task.await??).await
}

#[tokio::test]
async fn boundary_pending_body_is_inside_processing_timeout() -> TestResult {
    tokio::time::pause();
    let task = tokio::spawn(
        boundary_router()?
            .oneshot(Request::post("/body").body(Body::new(OneFrameThenPending::new()))?),
    );
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;

    timeout_response_json(task.await??).await
}

#[tokio::test]
async fn boundary_normal_response_is_unchanged() -> TestResult {
    let response = boundary_router()?
        .oneshot(Request::get("/ok").body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = response.into_body().collect().await?.to_bytes();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"ok");
    Ok(())
}

struct OneFrameThenPending {
    yielded: bool,
}

impl OneFrameThenPending {
    const fn new() -> Self {
        Self { yielded: false }
    }
}

impl HttpBody for OneFrameThenPending {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.yielded {
            Poll::Pending
        } else {
            self.yielded = true;
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"partial")))))
        }
    }
}

fn lifecycle_limits(
    request_head: Duration,
    connection: Duration,
    shutdown_grace: Duration,
) -> Result<ServerLimits, voom_core::VoomError> {
    ServerLimits::new_for_test(
        1024 * 1024,
        Duration::from_secs(1),
        request_head,
        Duration::from_secs(1),
        connection,
        shutdown_grace,
    )
}

async fn start_cleartext_test_server(
    router: Router,
    limits: ServerLimits,
) -> Result<RunningServer, Box<dyn std::error::Error>> {
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0).to_string();
    let config = Cli::try_parse_from(["voom-api", "--bind", &bind, "--allow-cleartext-loopback"])?
        .validate()?
        .with_limits_for_test(limits);
    Ok(RunningServer::start(config, router).await?)
}

#[test]
fn running_server_has_one_production_listener_entrypoint() {
    let source = include_str!("server.rs");
    assert_eq!(source.matches("pub async fn start(").count(), 1);
    assert_eq!(source.matches("TcpListener::bind").count(), 1);
    assert!(!source.contains("pub async fn start_"));
    assert!(!source.contains("pub fn start_"));
}

async fn raw_http1(addr: SocketAddr, request: &[u8]) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(addr).await?;
    stream.write_all(request).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(response)
}

async fn assert_connection_refused(addr: SocketAddr) -> TestResult {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if TcpStream::connect(addr).await.is_err() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn deadline_stream_times_out_a_blocked_write() -> TestResult {
    let (server, _client) = tokio::io::duplex(1);
    let mut stream = DeadlineStream::new(server, Duration::from_millis(25));
    stream.write_all(b"x").await?;
    let error =
        stream.write_all(b"y").await.err().ok_or_else(|| {
            io::Error::other("blocked deadline stream write unexpectedly succeeded")
        })?;

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    Ok(())
}

#[tokio::test]
async fn deadline_stream_times_out_a_blocked_read() -> TestResult {
    let (server, _client) = tokio::io::duplex(1);
    let mut stream = DeadlineStream::new(server, Duration::from_millis(25));
    let mut byte = [0_u8; 1];
    let error =
        stream.read_exact(&mut byte).await.err().ok_or_else(|| {
            io::Error::other("blocked deadline stream read unexpectedly succeeded")
        })?;

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    Ok(())
}

#[tokio::test]
async fn deadline_stream_times_out_a_blocked_flush() -> TestResult {
    let mut stream = DeadlineStream::new(PendingFlush, Duration::from_millis(25));
    let error =
        stream.flush().await.err().ok_or_else(|| {
            io::Error::other("blocked deadline stream flush unexpectedly succeeded")
        })?;

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    Ok(())
}

#[tokio::test]
async fn cleartext_server_is_http1_and_closes_each_connection() -> TestResult {
    let limits = lifecycle_limits(
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )?;
    let server =
        start_cleartext_test_server(Router::new().route("/", get(|| async { "ok" })), limits)
            .await?;
    let mut stream = TcpStream::connect(server.local_addr()).await?;
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;

    let response = String::from_utf8(response)?;
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.to_ascii_lowercase().contains("connection: close"));
    if stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .is_ok()
    {
        let mut second = Vec::new();
        stream.read_to_end(&mut second).await?;
        assert!(second.is_empty());
    }
    server.shutdown_on(std::future::ready(())).await?;
    Ok(())
}

#[tokio::test]
async fn cleartext_server_closes_a_partial_request_head() -> TestResult {
    let limits = lifecycle_limits(
        Duration::from_millis(25),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )?;
    let server = start_cleartext_test_server(Router::new(), limits).await?;
    let mut stream = TcpStream::connect(server.local_addr()).await?;
    stream.write_all(b"GET / HTTP/1.1\r\nHost:").await?;
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut response)).await??;

    assert!(response.is_empty());
    server.shutdown_on(std::future::ready(())).await?;
    Ok(())
}

#[tokio::test]
async fn cleartext_server_rejects_http2_prior_knowledge() -> TestResult {
    let limits = lifecycle_limits(
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )?;
    let server = start_cleartext_test_server(Router::new(), limits).await?;
    let response = raw_http1(server.local_addr(), b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n").await?;

    assert!(!response.starts_with(b"HTTP/2"));
    server.shutdown_on(std::future::ready(())).await?;
    Ok(())
}

#[tokio::test]
async fn graceful_shutdown_finishes_an_inflight_request() -> TestResult {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let router = Router::new().route(
        "/wait",
        get({
            let started = started.clone();
            let release = release.clone();
            move || {
                let started = started.clone();
                let release = release.clone();
                async move {
                    started.notify_one();
                    release.notified().await;
                    "done"
                }
            }
        }),
    );
    let limits = lifecycle_limits(
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_millis(250),
    )?;
    let server = start_cleartext_test_server(router, limits).await?;
    let addr = server.local_addr();
    let request = tokio::spawn(raw_http1(
        addr,
        b"GET /wait HTTP/1.1\r\nHost: localhost\r\n\r\n",
    ));
    started.notified().await;
    let shutdown = tokio::spawn(server.shutdown_on(std::future::ready(())));
    assert_connection_refused(addr).await?;
    release.notify_one();

    let response = String::from_utf8(request.await??)?;
    assert!(response.ends_with("done"));
    shutdown.await??;
    Ok(())
}

#[tokio::test]
async fn graceful_shutdown_forces_a_stalled_request_at_grace() -> TestResult {
    let started = Arc::new(Notify::new());
    let router = Router::new().route(
        "/wait",
        get({
            let started = started.clone();
            move || {
                let started = started.clone();
                async move {
                    started.notify_one();
                    std::future::pending::<()>().await;
                }
            }
        }),
    );
    let limits = lifecycle_limits(
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_millis(25),
    )?;
    let server = start_cleartext_test_server(router, limits).await?;
    let addr = server.local_addr();
    let request = tokio::spawn(raw_http1(
        addr,
        b"GET /wait HTTP/1.1\r\nHost: localhost\r\n\r\n",
    ));
    started.notified().await;

    tokio::time::timeout(
        Duration::from_secs(1),
        server.shutdown_on(std::future::ready(())),
    )
    .await??;
    let response = request.await?;
    assert!(response.is_err() || response?.is_empty());
    Ok(())
}

#[tokio::test]
async fn unexpected_server_task_exit_fails_loud_without_a_signal() -> TestResult {
    let server = RunningServer {
        local_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        handle: axum_server::Handle::new(),
        task: tokio::spawn(async { Ok(()) }),
        shutdown_grace: Duration::from_secs(1),
    };

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        server.shutdown_on(std::future::pending::<()>()),
    )
    .await?;
    let Err(error) = result else {
        return Err("a stopped listener must fail before any signal arrives".into());
    };
    assert!(matches!(error, super::ServerError::Stopped));
    Ok(())
}

struct PendingFlush;

impl AsyncRead for PendingFlush {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}

impl AsyncWrite for PendingFlush {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Pending
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
