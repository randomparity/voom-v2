//! Transport-agnostic traits the supervisor and workers implement
//! against. Phase 1 design §3.7.
//!
//! `ClientHandle` is what a supervisor consumes; `ServerHandle` is
//! what a worker exposes. The concrete HTTP/1.1 loopback transport
//! lives in `crate::http`; Sprint 4's TLS swap will plug into the
//! same traits without touching consumers.

use std::pin::Pin;

use async_trait::async_trait;
use tokio::io::AsyncRead;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::{
    HandshakeResponse, NdjsonReader, OperationRequest, OperationResponse, ProtocolError,
    WorkerCredentials, WorkerIdentityResponse,
};

/// Crate-owned NDJSON byte stream type. Erases the underlying
/// transport's body type (hyper today, TLS-wrapped tomorrow) behind
/// `AsyncRead`.
pub type NdjsonStream = NdjsonReader<Pin<Box<dyn AsyncRead + Send + Unpin>>>;

/// Outcome of `ClientHandle::dispatch`.
pub struct DispatchStream {
    pub response: OperationResponse,
    pub frames: NdjsonStream,
}

impl std::fmt::Debug for DispatchStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchStream")
            .field("response", &self.response)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait ClientHandle: Send + Sync + std::fmt::Debug {
    /// Negotiate the protocol version with the worker.
    async fn handshake(&self, offered: u32) -> Result<HandshakeResponse, ProtocolError>;

    /// Authenticate the serving worker's recorded identity.
    async fn identity(
        &self,
        credentials: &WorkerCredentials,
    ) -> Result<WorkerIdentityResponse, ProtocolError>;

    /// Dispatch one operation.
    ///
    /// `idempotency_key` is an opaque, non-empty identifier for one logical dispatch attempt.
    /// A transport retry must reuse the same key and identical serialized request bytes. If the
    /// completed response remains cached by the same worker instance, the worker replays it. A
    /// duplicate received while the original is in flight is rejected. A new logical attempt
    /// must use a distinct key. The transport does not prescribe ULID or concrete syntax.
    async fn dispatch(
        &self,
        creds: &WorkerCredentials,
        idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError>;
}

/// Handle returned by `ServerHandle::serve` so the caller can both
/// observe the bound address and request graceful shutdown.
pub struct ServerRunning {
    pub bound: std::net::SocketAddr,
    pub shutdown: oneshot::Sender<()>,
    pub joined: JoinHandle<()>,
}

impl std::fmt::Debug for ServerRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerRunning")
            .field("bound", &self.bound)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait ServerHandle: Send + Sync + std::fmt::Debug {
    /// Bind to `addr` and start serving. The returned handle owns
    /// shutdown + join.
    async fn serve(&self, addr: std::net::SocketAddr) -> Result<ServerRunning, ProtocolError>;
}
