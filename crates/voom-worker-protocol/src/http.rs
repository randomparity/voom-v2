//! HTTP/1.1 loopback transport over `hyper` 1.x. Phase 1 design §3.8.
//!
//! Exposed routes:
//! - `POST /v1/handshake` (exempt from version/auth/idempotency)
//! - `POST /v1/operations` (gated on version + auth). Requests must
//!   carry `x-voom-idempotency-key`; any JSON body field named
//!   `idempotency_key` is rejected because the header is canonical.
//!   Duplicate keys are compared by request body hash: conflicting or
//!   still-active duplicates fail, while completed responses can be replayed.
//! - 404 on unknown routes
//! - `OperationResponse` envelope as the first NDJSON line on the
//!   response body, followed by the actual progress frames
//!
//! Lease-callback routes (heartbeat/progress/cancel) are supervisor-side APIs,
//! not part of this worker loopback transport.

use std::convert::Infallible;

use bytes::Bytes;

mod client;
mod idempotency;
mod server;
mod streaming;

pub use client::HttpClient;
pub use server::{HttpServer, OperationFuture, OperationHandler, RoutePolicy, route_policy};
pub use streaming::{OperationBody, OperationDispatch, StreamingBody, StreamingFrameWriter};

const PROTOCOL_VERSION_HEADER: &str = "x-voom-protocol-version";
const WORKER_ID_HEADER: &str = "x-voom-worker-id";
const WORKER_EPOCH_HEADER: &str = "x-voom-worker-epoch";
const IDEMPOTENCY_KEY_HEADER: &str = "x-voom-idempotency-key";

pub(crate) type ResponseBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

#[cfg(test)]
use crate::transport::{ClientHandle, DispatchStream, ServerHandle, ServerRunning};
#[cfg(test)]
use crate::{OperationRequest, OperationResponse, ProtocolError, WorkerCredentials};

#[cfg(test)]
use idempotency::{CachedResponse, IdempotencyBegin, IdempotencyCache};
#[cfg(test)]
use server::{require_idempotency_key, validate_no_body_idempotency_key};

#[cfg(test)]
#[path = "http_test.rs"]
mod tests;
