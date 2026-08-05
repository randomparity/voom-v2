//! Process-level HTTP request bounds and listener lifecycle.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::Response;
use axum_server::Handle;
use axum_server::accept::{Accept, DefaultAcceptor};
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use hyper_util::rt::TokioTimer;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Sleep, sleep};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use voom_core::ErrorCode;

use crate::config::{ServerConfig, ServerLimits, TransportConfig};
use crate::err_response;

const TIMEOUT_MESSAGE: &str = "request processing exceeded the 30-second deadline";
const TIMEOUT_HINT: &str =
    "Retry a mutation with the same idempotency key if its outcome is unknown";
const BODY_LIMIT_MESSAGE: &str = "request body exceeds the 1048576-byte limit";
const BODY_LIMIT_HINT: &str = "Send a request body of 1048576 bytes or fewer";

/// I/O stream that fails every operation after one total connection deadline.
#[derive(Debug)]
pub struct DeadlineStream<I> {
    inner: I,
    deadline: Pin<Box<Sleep>>,
}

impl<I> DeadlineStream<I> {
    pub fn new(inner: I, duration: Duration) -> Self {
        Self {
            inner,
            deadline: Box::pin(sleep(duration)),
        }
    }
}

fn deadline_expired(deadline: Pin<&mut Sleep>, cx: &mut Context<'_>) -> io::Result<()> {
    if deadline.poll(cx).is_ready() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "API connection deadline exceeded",
        ))
    } else {
        Ok(())
    }
}

impl<I: AsyncRead + Unpin> AsyncRead for DeadlineStream<I> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = deadline_expired(this.deadline.as_mut(), cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for DeadlineStream<I> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = deadline_expired(this.deadline.as_mut(), cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = deadline_expired(this.deadline.as_mut(), cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = deadline_expired(this.deadline.as_mut(), cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = deadline_expired(this.deadline.as_mut(), cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// Acceptor that starts one total I/O deadline after its inner acceptor succeeds.
#[derive(Clone, Debug)]
pub struct ConnectionDeadlineAcceptor<A> {
    inner: A,
    duration: Duration,
}

impl<A> ConnectionDeadlineAcceptor<A> {
    pub const fn new(inner: A, duration: Duration) -> Self {
        Self { inner, duration }
    }
}

impl<I, S, A> Accept<I, S> for ConnectionDeadlineAcceptor<A>
where
    A: Accept<I, S>,
    A::Future: Send + 'static,
    A::Stream: 'static,
    A::Service: 'static,
{
    type Stream = DeadlineStream<A::Stream>;
    type Service = A::Service;
    type Future =
        Pin<Box<dyn Future<Output = io::Result<(Self::Stream, Self::Service)>> + Send + 'static>>;

    fn accept(&self, stream: I, service: S) -> Self::Future {
        let accepted = self.inner.accept(stream, service);
        let duration = self.duration;
        Box::pin(async move {
            let (stream, service) = accepted.await?;
            Ok((DeadlineStream::new(stream, duration), service))
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(
        "failed to load TLS identity; verify --tls-cert and --tls-key are readable PEM files, \
         the certificate chain is ordered correctly, and the private key matches"
    )]
    TlsConfig,
    #[error("failed to bind API listener; verify --bind and local socket permissions")]
    Bind(#[source] io::Error),
    #[error("API server task failed while serving connections")]
    Serve(#[source] io::Error),
    #[error("API server task stopped unexpectedly")]
    Join(#[source] JoinError),
}

/// One listening server and its bounded shutdown lifecycle.
#[derive(Debug)]
pub struct RunningServer {
    local_addr: SocketAddr,
    handle: Handle<SocketAddr>,
    task: JoinHandle<io::Result<()>>,
    shutdown_grace: Duration,
}

impl RunningServer {
    pub async fn start(config: ServerConfig, router: Router) -> Result<Self, ServerError> {
        let bind = config.bind();
        let limits = config.limits();
        match config.transport().clone() {
            TransportConfig::Tls {
                cert_path,
                key_path,
            } => {
                let tls_config = load_tls_config(&cert_path, &key_path).await?;
                let (listener, local_addr) = bind_listener(bind)?;
                let handle = Handle::new();
                let server = configured_server(listener, handle.clone(), limits)?;
                let rustls =
                    RustlsAcceptor::new(tls_config).handshake_timeout(limits.tls_handshake);
                let acceptor = ConnectionDeadlineAcceptor::new(rustls, limits.connection);
                let task = tokio::spawn(
                    server
                        .acceptor(acceptor)
                        .serve(bounded_router(router, limits).into_make_service()),
                );
                finish_start(local_addr, handle, task, limits).await
            }
            TransportConfig::CleartextLoopback => {
                let (listener, local_addr) = bind_listener(bind)?;
                let handle = Handle::new();
                let server = configured_server(listener, handle.clone(), limits)?;
                let acceptor =
                    ConnectionDeadlineAcceptor::new(DefaultAcceptor::new(), limits.connection);
                let task = tokio::spawn(
                    server
                        .acceptor(acceptor)
                        .serve(bounded_router(router, limits).into_make_service()),
                );
                finish_start(local_addr, handle, task, limits).await
            }
        }
    }

    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.handle.connection_count()
    }

    pub async fn shutdown_on<F>(self, signal: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()>,
    {
        let Self {
            handle,
            mut task,
            shutdown_grace,
            ..
        } = self;
        tokio::pin!(signal);
        tokio::select! {
            result = &mut task => flatten_server_task(result),
            () = &mut signal => {
                handle.graceful_shutdown(Some(shutdown_grace));
                flatten_server_task(task.await)
            }
        }
    }
}

fn bind_listener(bind: SocketAddr) -> Result<(std::net::TcpListener, SocketAddr), ServerError> {
    let listener = std::net::TcpListener::bind(bind).map_err(ServerError::Bind)?;
    listener.set_nonblocking(true).map_err(ServerError::Bind)?;
    let local_addr = listener.local_addr().map_err(ServerError::Bind)?;
    Ok((listener, local_addr))
}

fn configured_server(
    listener: std::net::TcpListener,
    handle: Handle<SocketAddr>,
    limits: ServerLimits,
) -> Result<axum_server::Server<SocketAddr>, ServerError> {
    let mut server = axum_server::from_tcp(listener)
        .map_err(ServerError::Bind)?
        .http1_only()
        .handle(handle);
    server
        .http_builder()
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.request_head)
        .keep_alive(false);
    Ok(server)
}

async fn finish_start(
    local_addr: SocketAddr,
    handle: Handle<SocketAddr>,
    task: JoinHandle<io::Result<()>>,
    limits: ServerLimits,
) -> Result<RunningServer, ServerError> {
    let listening = handle.listening().await.ok_or_else(|| {
        ServerError::Bind(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "listener did not report a bound address",
        ))
    })?;
    debug_assert_eq!(listening, local_addr);
    Ok(RunningServer {
        local_addr,
        handle,
        task,
        shutdown_grace: limits.shutdown_grace,
    })
}

async fn load_tls_config(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<RustlsConfig, ServerError> {
    let cert_pem = tokio::fs::read(cert_path)
        .await
        .map_err(|_| ServerError::TlsConfig)?;
    let key_pem = tokio::fs::read(key_path)
        .await
        .map_err(|_| ServerError::TlsConfig)?;
    let certificates = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ServerError::TlsConfig)?;
    if certificates.is_empty() {
        return Err(ServerError::TlsConfig);
    }
    let private_key =
        PrivateKeyDer::from_pem_slice(&key_pem).map_err(|_| ServerError::TlsConfig)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| ServerError::TlsConfig)?
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|_| ServerError::TlsConfig)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(RustlsConfig::from_config(Arc::new(config)))
}

fn flatten_server_task(result: Result<io::Result<()>, JoinError>) -> Result<(), ServerError> {
    result
        .map_err(ServerError::Join)?
        .map_err(ServerError::Serve)
}

/// Apply the fixed process-wide request bounds to an application router.
pub fn bounded_router(router: Router, limits: ServerLimits) -> Router {
    router
        .layer(RequestBodyLimitLayer::new(limits.max_request_body_bytes))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            limits.request_processing,
        ))
        .layer(middleware::map_response(normalize_boundary_response))
}

async fn normalize_boundary_response(response: Response) -> Response {
    match response.status() {
        StatusCode::REQUEST_TIMEOUT => err_response(
            StatusCode::REQUEST_TIMEOUT,
            "api.request",
            ErrorCode::RequestTimeout.as_str(),
            TIMEOUT_MESSAGE.to_owned(),
            Some(TIMEOUT_HINT.to_owned()),
        ),
        StatusCode::PAYLOAD_TOO_LARGE => err_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "api.request",
            ErrorCode::PayloadTooLarge.as_str(),
            BODY_LIMIT_MESSAGE.to_owned(),
            Some(BODY_LIMIT_HINT.to_owned()),
        ),
        _ => response,
    }
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
