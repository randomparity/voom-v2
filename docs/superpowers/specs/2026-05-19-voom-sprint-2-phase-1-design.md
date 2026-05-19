---
name: voom-sprint-2-phase-1-design
description: Sprint 2 Phase 1 design — versioned HTTP/JSON worker protocol foundation. Ships voom-worker-protocol (wire types, NDJSON codec with framing invariants, bearer-token + worker-identity validation, ClientHandle/ServerHandle traits, low_level raw-wire module, one HTTP/1.1 loopback transport), voom-conformance (bootstrap harness with paired typed + raw-wire layers and golden-byte mutation fixtures), and echo-worker (minimal worker that exists solely to validate the contract). Per-phase detail under the cross-phase decisions fixed in the Sprint 2 overview.
status: proposed
date: 2026-05-19
sprint: 2
phase: 1
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
parent_sections: §2 Phase 1, §3 (voom-worker-protocol / voom-conformance / voom-core rows), §4.1 transport, §4.2 NDJSON framing, §4.7 conformance two layers
arch_spec: docs/specs/voom-control-plane-design.md (Worker Runtime, Worker Trust And Capability Grants, Error Handling And Recovery)
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
  - docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md
  - docs/adr/0002-out-of-process-workers-only.md
  - docs/adr/0003-sqlx-and-tokio-foundation.md
---

# Sprint 2 Phase 1 — Worker Protocol Foundation

## 1. Goal

Land the wire contract every subsequent Sprint 2 phase depends on:
versioned HTTP/JSON request / response types, NDJSON progress
streaming with the framing invariants from the overview's §4.2,
bearer-token + worker-identity authentication, transport traits, one
concrete HTTP/1.1 loopback transport, a raw-wire `low_level` module
for conformance and chaos use, the bootstrap conformance harness,
and an `echo-worker` binary that exists only to validate the
contract.

This phase ships **no** supervisor logic, **no** scheduler logic,
**no** fake providers, and **no** dispatch outbox. Those are Phase 2
work. Phase 1 is the type layer plus the test infrastructure that
will gate every later phase.

## 2. Scope

Crates touched (per overview §3):

| Crate | What Phase 1 adds |
|---|---|
| `voom-worker-protocol` | All public protocol types, NDJSON codec, version negotiation, transport traits, HTTP/1.1 loopback transport, `low_level` raw-wire module. |
| `voom-conformance` | New crate. Bootstrap conformance harness (typed + raw-wire layers), golden-byte fixtures, mutation suite, `echo-worker` binary. |
| `voom-core` | `protocol_version` constant, three new `ErrorCode` variants (`WORKER_RETIRED`, `WORKER_INCARNATION_STALE`, `AMBIGUOUS_WORKER_SELECTION`). `FailureClass::ProgressTimeout` and `FailureClass::AmbiguousWorkerSelection` are added in Phase 2 (the supervisor introduces them) — Phase 1 only adds the `ErrorCode` variants the protocol layer needs. |
| All others | Untouched. |

No new SQL migrations. No `voom-store` changes. Sprint 1 tests
continue passing unmodified.

## 3. Public API of `voom-worker-protocol`

### 3.1 Module layout

```
voom-worker-protocol/
├── src/
│   ├── lib.rs              — re-exports + crate-level docs
│   ├── envelope.rs         — OperationRequest, OperationResponse, ProgressFrame, ProtocolError
│   ├── envelope_test.rs    — sibling unit tests
│   ├── handshake.rs        — version negotiation
│   ├── handshake_test.rs
│   ├── credentials.rs      — WorkerCredentials, bearer-token + worker_id + worker_epoch
│   ├── credentials_test.rs
│   ├── operation_kind.rs   — OperationKind enum (fixed vocabulary)
│   ├── operation_kind_test.rs
│   ├── ndjson.rs           — frame codec with §4.2 invariants
│   ├── ndjson_test.rs
│   ├── transport.rs        — ClientHandle, ServerHandle traits
│   ├── transport_test.rs
│   ├── http.rs             — HTTP/1.1 loopback transport (uses hyper)
│   ├── http_test.rs
│   └── low_level/
│       ├── mod.rs          — raw HTTP + raw NDJSON primitives
│       ├── http_raw.rs     — raw request/response bytes
│       ├── http_raw_test.rs
│       ├── ndjson_raw.rs   — raw line-by-line read/write
│       └── ndjson_raw_test.rs
```

Every `_test.rs` is linked from its source with `#[path]` per ADR-0004.

### 3.2 Wire types (envelope.rs)

```rust
/// Top-level operation request from supervisor → worker.
///
/// The supervisor POSTs an `OperationRequest` to the worker's
/// `/v1/operations` endpoint. The HTTP response body is an NDJSON
/// stream of `ProgressFrame`s terminated by exactly one `Result` or
/// `Error` frame.
///
/// `lease_id` is the sole work-identity authority on the wire.
/// `ticket_id` is intentionally absent — the supervisor knows the
/// ticket from its durable `leases` row; embedding it on the wire
/// would create a second source of truth a malicious worker could
/// claim against. The wire stays narrow: bind the lease, derive
/// everything else from durable state.
///
/// Idempotency is carried ONLY in the `X-Voom-Idempotency-Key`
/// request header (§3.8). It is intentionally NOT present in the
/// body. The middleware applies request-hash-keyed replay semantics:
/// same key + same body → replay original outcome; same key +
/// different body → `DuplicateIdempotencyKey`. A raw-wire mutation
/// test pins that a body field named `idempotency_key` is rejected
/// as `HeaderBodyKeyMismatch` regardless of value.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRequest {
    pub operation: OperationKind,
    pub lease_id: LeaseId,
    pub payload: serde_json::Value,       // operation-specific; deny_unknown_fields applies recursively only to typed sub-shapes the worker decodes
    pub heartbeat_deadline_ms: u32,
    pub progress_idle_deadline_ms: u32,
}

/// Worker → supervisor response for the immediate ack on
/// `/v1/operations`. The supervisor verifies it before consuming
/// the NDJSON body. The idempotency key and protocol version are
/// echoed in response headers (`X-Voom-Idempotency-Key`,
/// `X-Voom-Protocol-Version`), not in the body.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationResponse {
    pub lease_id: LeaseId,
    pub accepted_at: chrono::DateTime<chrono::Utc>,
}

/// 0..=10000 basis points so `Eq` is derivable and on-wire JSON is
/// integer (no NaN, no float-equality foot-guns). 0 → 0%, 10000 → 100%.
///
/// Field is private; only `TryFrom<u16>` and `Deserialize` can
/// construct one, so the `0..=10000` invariant is enforced at every
/// boundary (typed callers and deserialized JSON alike). A direct
/// `PercentBps(65535)` literal does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent, try_from = "u16", into = "u16")]
pub struct PercentBps {
    bps: u16,
}

impl PercentBps {
    pub const ZERO: Self = Self { bps: 0 };
    pub const FULL: Self = Self { bps: 10_000 };
    pub fn bps(self) -> u16 { self.bps }
}

impl TryFrom<u16> for PercentBps {
    type Error = ProtocolError;
    fn try_from(bps: u16) -> Result<Self, Self::Error> {
        if bps > 10_000 {
            Err(ProtocolError::InvalidPayload { detail: format!("percent_bps={bps} > 10000") })
        } else {
            Ok(Self { bps })
        }
    }
}

impl From<PercentBps> for u16 {
    fn from(p: PercentBps) -> u16 { p.bps }
}
```

Tests pin:
- `PercentBps::try_from(0)` and `try_from(10_000)` succeed.
- `try_from(10_001)` and `try_from(65_535)` reject with `InvalidPayload`.
- Deserializing `10001` from JSON via `serde_json::from_str` returns
  the same error.
- A direct `PercentBps { bps: 65535 }` does not compile from outside
  the module (compile-fail trybuild test).

```rust
/// One frame on the NDJSON progress stream.
///
/// Every frame includes `lease_id` and `seq`. Per §4.2: `seq` is
/// monotonic starting at 0; duplicates with seq ≤ last_seen are
/// dropped; gaps abort the stream as `malformed_worker_result`.
/// `lease_id` is checked against the expected lease the
/// `NdjsonReader` was constructed with — any frame whose `lease_id`
/// differs is rejected as `MalformedFrame` BEFORE any other check
/// (§3.5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressFrame {
    Progress {
        lease_id: LeaseId,
        seq: u64,
        emitted_at: chrono::DateTime<chrono::Utc>,
        percent: Option<PercentBps>,
        message: Option<String>,
        payload: Option<serde_json::Value>,
    },
    Result {
        lease_id: LeaseId,
        seq: u64,
        emitted_at: chrono::DateTime<chrono::Utc>,
        payload: serde_json::Value,
    },
    Error {
        lease_id: LeaseId,
        seq: u64,
        emitted_at: chrono::DateTime<chrono::Utc>,
        class: FailureClass,             // from voom-core::failure
        code: ErrorCode,                 // from voom-core::error
        message: String,
        payload: Option<serde_json::Value>,
    },
}

/// Protocol-level errors (distinct from FailureClass; these are
/// errors processing the wire contract itself, not work outcomes).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProtocolError {
    UnsupportedProtocolVersion { offered: u32, supported_min: u32, supported_max: u32 },
    UnknownOperation { name: String },
    InvalidPayload { detail: String },
    UnauthorizedBearer,
    UnknownWorkerId { presented: WorkerId },
    StaleWorkerEpoch { presented: u64, current: u64 },
    WorkerRetired { worker_id: WorkerId, epoch: u64 },
    DuplicateIdempotencyKey { key: String, original_status: String },
    FrameTooLarge { bytes: u64, max: u64 },
    MalformedFrame { detail: String },
    OutOfOrderFrame { expected_seq: u64, got_seq: u64 },
    WrongLeaseId { expected: LeaseId, got: LeaseId },
    UnexpectedFrameAfterTerminal,
    HeaderBodyKeyMismatch,                 // body carries idempotency_key (forbidden)
    InternalServerError,
}
```

### 3.3 OperationKind (operation_kind.rs)

One variant per architectural-spec fixed-operation, with **no test-only
variant**. Conformance and `echo-worker` use `OperationKind::ProbeFile`
with a synthetic-but-typed payload (one path string), so the wire
vocabulary stays exactly the architectural-spec fixed list. New
plugin-defined operations stay outside Sprint 2 scope (Sprint 8
plugin SDK).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    ScanLibrary,
    ProbeFile,
    HashFile,
    IdentifyMedia,
    ScoreQuality,
    SyncExternalSystem,
    BackUpFile,
    Remux,                  // remux/containerize
    TranscodeVideo,
    EditTracks,
    ExtractAudio,
    TranscribeAudio,
    VerifyArtifact,
    CommitArtifact,
    DeleteArtifact,
}
```

Serialization is snake_case to match the architectural-spec vocabulary
verbatim. A round-trip test pins this against every variant.
`echo-worker` (§4.5) handles `ProbeFile` only and rejects all other
variants with `ProtocolError::UnknownOperation` (so a test asserting
that rejection is meaningful).

### 3.4 Credentials (credentials.rs)

```rust
/// In-memory representation of a worker's identity for one
/// dispatch direction. The supervisor builds this at spawn time;
/// every callback validates against it.
#[derive(Debug, Clone)]
pub struct WorkerCredentials {
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
    pub secret: secrecy::SecretString,    // bearer token; zeroized on drop
}

impl WorkerCredentials {
    /// Build the request headers `Authorization`, `X-Voom-Worker-Id`,
    /// `X-Voom-Worker-Epoch`. Borrows the secret without copying.
    pub fn to_headers(&self) -> hyper::HeaderMap { ... }
}

/// Validates inbound credentials against the live record. The
/// supervisor calls this on every callback; the worker calls it on
/// every supervisor → worker call.
pub fn validate_credentials(
    presented: &PresentedCredentials,
    live: &WorkerCredentials,
) -> Result<(), ProtocolError> {
    if presented.worker_id != live.worker_id { return Err(ProtocolError::UnknownWorkerId { presented: presented.worker_id }); }
    if presented.epoch < live.worker_epoch { return Err(ProtocolError::StaleWorkerEpoch { presented: presented.epoch, current: live.worker_epoch }); }
    if presented.epoch > live.worker_epoch { return Err(ProtocolError::StaleWorkerEpoch { ... }); }
    if !constant_time_eq(presented.secret.expose_secret().as_bytes(), live.secret.expose_secret().as_bytes()) {
        return Err(ProtocolError::UnauthorizedBearer);
    }
    Ok(())
}
```

`secrecy::SecretString` zeroizes on drop and never appears in `Debug`
output — a static test (compile-time) asserts `WorkerCredentials`
does not derive `Debug` with the secret visible. Constant-time
compare is required to prevent timing oracles. Crate dep:
`secrecy = "0.10"` and `constant_time_eq = "0.4"`.

### 3.5 NDJSON codec (ndjson.rs)

The reader is constructed with the **expected `LeaseId`** so the lease
boundary is enforced at the lowest layer that handles bytes. A frame
whose `lease_id` does not match the expected lease is rejected as
`WrongLeaseId` BEFORE seq or terminal-state checks run; the stream is
aborted. This closes the durable-corruption class where a worker
could emit a valid terminal frame for a different lease and still
satisfy the seq/terminal invariants.

```rust
pub struct NdjsonReader<R: AsyncRead + Unpin> {
    reader: tokio::io::BufReader<R>,
    expected_lease_id: LeaseId,
    last_seq: Option<u64>,
    terminal_seen: bool,
    max_frame_bytes: usize,  // 64 KiB default
    bytes_since_last_frame: usize,
}

impl<R> NdjsonReader<R> {
    pub fn new(reader: R, expected_lease_id: LeaseId) -> Self;
    pub fn with_max_frame_bytes(mut self, max: usize) -> Self;
    pub async fn next_frame(&mut self) -> Result<NdjsonOutcome, ProtocolError>;
    // NdjsonOutcome:
    //   Frame(ProgressFrame),
    //   StreamEnd { partial_bytes: usize },       // EOF without terminal
    //   Terminated { terminal: ProgressFrame },   // delivers terminal frame; subsequent calls return UnexpectedFrameAfterTerminal
}

pub struct NdjsonWriter<W: AsyncWrite + Unpin> {
    writer: tokio::io::BufWriter<W>,
    bound_lease_id: LeaseId,         // writer is bound to one lease for its lifetime
    next_seq: u64,
    terminal_sent: bool,
}

impl<W> NdjsonWriter<W> {
    pub fn new(writer: W, bound_lease_id: LeaseId) -> Self;
    /// `frame.lease_id()` must equal `self.bound_lease_id`; mismatch
    /// returns Err and the frame is NOT written.
    pub async fn emit(&mut self, frame: ProgressFrame) -> Result<(), ProtocolError>;
    pub async fn flush_and_close(self) -> std::io::Result<()>;
}
```

Reader invariants pinned by sibling unit tests AND raw-wire mutations:

- frame on first line, seq = 0, expected lease → OK
- frame with seq ≤ last_seq → frame dropped, no error
- frame with seq > last_seq + 1 → `OutOfOrderFrame`
- frame whose `lease_id` ≠ expected → `WrongLeaseId`, stream aborted
- line length > max_frame_bytes → `FrameTooLarge`, stream aborted
- frame after terminal → `UnexpectedFrameAfterTerminal`
- EOF with no terminal → `StreamEnd { partial_bytes: bytes }`
- EOF in mid-frame (partial JSON) → `MalformedFrame { detail }`

Writer invariants:

- second terminal emit → panic in debug, `Err(MalformedFrame)` in release
- seq auto-increments; caller cannot supply seq directly (closes the
  golden-bytes-mismatch foot-gun)

### 3.6 Version negotiation (handshake.rs)

```rust
pub const PROTOCOL_VERSION: u32 = 1;
pub const SUPPORTED_MIN: u32 = 1;
pub const SUPPORTED_MAX: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HandshakeRequest {
    pub offered: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HandshakeResponse {
    pub agreed: u32,
}
```

Worker exposes `POST /v1/handshake` accepting `HandshakeRequest` and
returning `HandshakeResponse` (or a `ProtocolError` body on
mismatch). The handshake route is exempt from version, auth, and
idempotency middleware — that exemption is encoded in the single
`route_policy(method, path) -> Option<RoutePolicy>` lookup defined
in §3.8 (no separate `is_version_gated_route` helper exists). The
lookup is exact-match on `(method, path)`; unknown routes return
`None` and the server replies `404 Not Found` before any handler
runs. This lets the handshake handler return a structured
`ProtocolError::UnsupportedProtocolVersion { offered, supported_min,
supported_max }` instead of a generic middleware rejection, so the
"version skew" test is positive-shape rather than "did middleware
return some error".

A `POST` (not `GET`) carries the offered version in a JSON body
where serde validation gives a clean negative test for malformed
content. The supervisor calls handshake before issuing any operation.

Test pins: `GET /v1/handshake` → 404, `POST /v1/handshake/` (trailing
slash) → 404, `POST /v1/unknown` → 404, `DELETE /v1/operations` → 404.

### 3.7 Transport traits (transport.rs)

The public traits MUST NOT expose `hyper` types — Sprint 4's TLS
transport will swap the underlying implementation, and consumers
that hold `hyper::body::Incoming` would break. The body type is a
crate-owned alias over `Pin<Box<dyn AsyncRead + Send>>`:

```rust
/// Crate-owned NDJSON byte stream type. The `voom-worker-protocol`
/// crate is the only thing that knows the concrete inner reader.
pub type NdjsonStream = NdjsonReader<Pin<Box<dyn tokio::io::AsyncRead + Send>>>;

#[async_trait::async_trait]
pub trait ClientHandle: Send + Sync {
    async fn handshake(&self, offered: u32) -> Result<HandshakeResponse, ProtocolError>;
    /// Caller supplies a fresh `idempotency_key` (ULID) per dispatch.
    /// The same key on a retry MUST yield the same outcome via
    /// worker-side dedupe.
    async fn dispatch(
        &self,
        creds: &WorkerCredentials,
        idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError>;
}

pub struct DispatchStream {
    pub response: OperationResponse,
    pub frames: NdjsonStream,
}

#[async_trait::async_trait]
pub trait ServerHandle: Send + Sync {
    async fn serve(&self, addr: std::net::SocketAddr) -> Result<ServerRunning, ProtocolError>;
}

pub struct ServerRunning {
    pub bound: std::net::SocketAddr,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
    pub joined: tokio::task::JoinHandle<()>,
}
```

The traits are deliberately small. A supervisor consumes
`ClientHandle`; a worker exposes `ServerHandle`. Sprint 4 replaces
`HttpClient` / `HttpServer` (the hyper-backed implementors) with TLS
equivalents behind the same trait without touching consumers, and
`NdjsonStream` continues to wrap the new inner `AsyncRead`.

### 3.8 HTTP/1.1 loopback transport (http.rs)

One `HttpClient` (consumes `ClientHandle`) and one `HttpServer`
(produces `ServerHandle`) both built on `hyper` 1.x. Server registers
the routes:

- `POST /v1/operations` — accept operation, return
  `OperationResponse` immediately, then stream NDJSON progress
  frames on the response body.
- `POST /v1/handshake` — accept `HandshakeRequest`, return
  `HandshakeResponse`. This route is exempt from version, auth,
  and idempotency middleware (see the matrix below); the
  handshake handler returns
  `ProtocolError::UnsupportedProtocolVersion` directly when
  `offered` is outside `[SUPPORTED_MIN, SUPPORTED_MAX]`.
- `POST /v1/leases/{id}/heartbeat` — accept heartbeat (worker → supervisor
  direction; the supervisor's server hosts this. The worker side does
  not implement it.).
- `POST /v1/leases/{id}/progress` — same direction as heartbeat.
- `POST /v1/leases/{id}/cancel` — supervisor → worker. Worker
  responds, then drains current operation. Phase 1 ships the route;
  Phase 2's supervisor wires its use.

Every request goes through one middleware. The middleware looks up
the route policy via `route_policy(method, path) -> Option<RoutePolicy>`
(exact match, fail-closed on unknown routes via 404):

| Method + Path | Version-gated? | Auth-gated? | Idempotency-gated? |
|---|---|---|---|
| `POST /v1/handshake` | no | no | no |
| `POST /v1/operations` | yes | yes | yes |
| `POST /v1/leases/{id}/heartbeat` | yes | yes | no |
| `POST /v1/leases/{id}/progress` | yes | yes | no |
| `POST /v1/leases/{id}/cancel` | yes | yes | no |

`route_policy` is exact-match on the literal pattern — path
parameters like `{id}` use a small router that matches the
parameter segment and rejects mismatching shapes (trailing slash,
double slash, missing segment). Unknown method+path pairs return
`None` from `route_policy` and the server responds with
`404 Not Found` before any handler or middleware runs. Tests pin
the matrix exhaustively, including: `GET /v1/handshake`,
`POST /v1/handshake/`, `POST /v1/unknown`, `DELETE /v1/operations`,
`POST /v1/leases//heartbeat` (empty id) — all 404.

The handshake exemption covers all three gates because the
supervisor does not yet hold credentials when it issues handshake
(in Sprint 4's authenticated transport, handshake will gain its own
TLS client-cert gate).

For gated routes:

1. **Version (if gated).** Parse `X-Voom-Protocol-Version` header;
   reject mismatches with `ProtocolError::UnsupportedProtocolVersion`.
2. **Auth (if gated).** Parse `Authorization: Bearer ...`,
   `X-Voom-Worker-Id`, `X-Voom-Worker-Epoch`. Validate via
   `validate_credentials`.
3. **Idempotency (if gated).** Parse `X-Voom-Idempotency-Key`
   (canonical idempotency location). Apply **explicit byte-replay
   semantics** keyed on the supervisor's persisted request bytes:
   - The supervisor's `lease_dispatch_intents` row (overview §4.8)
     persists `request_body_bytes: BLOB` alongside the
     `idempotency_key` at dispatch time. On retry after crash, the
     supervisor sends those exact persisted bytes — not a freshly
     reserialized `OperationRequest`. This is documented as a
     contract requirement on the supervisor side and pinned by a
     test in Phase 2.
   - The worker computes `body_hash = blake3(request_body_bytes)`
     on the raw bytes it received over the wire and stores
     `(idempotency_key, body_hash)` in its recent-key LRU. Replay
     succeeds iff both the key AND the hash match.
   - JSON canonicalization (key ordering, whitespace) is
     deliberately NOT relied on. The contract is "same bytes →
     same outcome"; the supervisor guarantees same bytes via the
     persisted blob. A test pins that a same-key, byte-reordered
     JSON retry (same logical request, different serialization)
     fails with `DuplicateIdempotencyKey` so the contract is
     enforced; another test pins that exact-byte replay succeeds.
   - The request body MUST NOT carry an `idempotency_key` field —
     not at the top level, not in any nested object, not in any
     array element. Enforcement: middleware parses the body once as
     `serde_json::Value` and recursively walks every object/array
     before any typed deserialization, rejecting on any field
     named `idempotency_key` with `HeaderBodyKeyMismatch`. The
     name `idempotency_key` is reserved across all operation
     payload schemas (documented constraint, not a serde
     enforcement, because `payload` is `serde_json::Value`).
   - Combined with `#[serde(deny_unknown_fields)]` on
     `OperationRequest`, this catches: same-value and
     different-value body collisions at the top level, the
     `{"payload": {"idempotency_key": ...}}` nested case, and the
     `{"payload": [{"idempotency_key": ...}]}` array case.
     Three paired raw-wire mutation tests cover the three shapes.

Phase 1 does NOT ship the supervisor's HTTP server (Phase 2 work),
but it DOES ship a tiny embedded server inside `voom-conformance` so
the conformance harness can play the supervisor role against
`echo-worker`. That tiny embedded server reuses `HttpServer` from
this crate.

### 3.9 `low_level` raw-wire module (low_level/)

Public re-export-controlled API:

```rust
pub mod low_level {
    pub use self::http_raw::{
        RawHttpRequest,
        RawHttpResponse,
        write_raw_request, read_raw_request,
        write_raw_response, read_raw_response,
    };
    pub use self::ndjson_raw::{
        RawLine,
        write_raw_line,
        read_raw_line,
        golden_line_bytes,   // returns canonical bytes for a known frame
    };
}
```

`low_level` does NOT use the typed envelope serde. It writes bytes
directly to an `AsyncWrite`. Conformance and chaos consume this
module to construct deliberately malformed wire bytes.

## 4. `voom-conformance` design

### 4.1 Crate shape

```
voom-conformance/
├── Cargo.toml
├── src/
│   ├── lib.rs                       — public Harness API
│   ├── lib_test.rs
│   ├── harness.rs                   — Harness::launch(worker_binary, scenario)
│   ├── harness_test.rs
│   ├── typed_suite.rs               — typed-API contract assertions
│   ├── typed_suite_test.rs
│   ├── raw_wire_suite.rs            — raw-byte mutation assertions
│   ├── raw_wire_suite_test.rs
│   └── fixtures/
│       ├── golden/                  — canonical bytes per frame type
│       └── mutations/               — pre-computed mutated bytes
└── src/bin/
    └── echo_worker.rs               — minimal worker, exits when stdin closes
```

`echo-worker` lives in `voom-conformance/src/bin/` not in `voom-fakes`
because it is conformance test infrastructure, not a Sprint 2
deliverable in its own right.

### 4.2 Harness API

```rust
pub struct Harness {
    worker_binary: std::path::PathBuf,
    extra_env: Vec<(String, String)>,
}

impl Harness {
    pub fn new(worker_binary: impl Into<std::path::PathBuf>) -> Self;
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self;
    pub async fn run_typed_suite(&self) -> SuiteResult;
    pub async fn run_raw_wire_suite(&self) -> SuiteResult;
    pub async fn run_all(&self) -> SuiteResult;
}

pub struct SuiteResult {
    pub passed: Vec<&'static str>,
    pub failed: Vec<(&'static str, String)>,
}
```

The harness spawns the worker binary with the same stdin-pipe pattern
the supervisor will use in Phase 2 (so the worker self-exits when the
harness drops). It speaks the protocol through `HttpClient` for typed
assertions and through `low_level` for raw-wire mutation assertions.

### 4.3 Typed suite assertions

Each is one `#[tokio::test]` style function inside `typed_suite.rs`,
collected into `run_typed_suite`. Names match the architectural-spec
vocabulary so failures point at the right concept:

- `handshake_returns_supported_version`
- `handshake_rejects_below_supported_min`
- `handshake_rejects_above_supported_max`
- `operation_accept_returns_response_envelope`
- `operation_probe_file_emits_one_progress_and_one_terminal_result`
- `operation_unknown_returns_unknown_operation`
- `operation_invalid_payload_returns_invalid_payload`
- `progress_frame_seq_starts_at_zero`
- `progress_frame_seq_is_monotonic`
- `progress_terminal_followed_by_nothing`
- `credentials_bearer_required`
- `credentials_worker_id_required`
- `credentials_epoch_required`
- `credentials_wrong_bearer_rejected`
- `credentials_stale_epoch_rejected`
- `credentials_after_retire_rejected`
- `idempotency_duplicate_key_rejected`
- `cancellation_drains_current_operation`
- `stdin_eof_terminates_worker`     (parent-death watchdog)

### 4.4 Raw-wire suite assertions

Each loads or constructs golden bytes, optionally applies a mutation,
sends them via `low_level`, and asserts the worker's reply:

- `golden_handshake_request_round_trips`
- `golden_operation_request_round_trips`
- `golden_progress_frame_round_trips`
- `tamper_with_seq__rejects_out_of_order`
- `truncate_at_byte_within_frame__rejects_malformed`
- `flip_one_byte_in_json__rejects_malformed`
- `wrong_content_length__rejects_malformed`
- `oversize_frame__rejects_frame_too_large`
- `frame_with_no_lease_id__rejects_malformed`
- `frame_with_wrong_lease_id__rejects_wrong_lease`     (load-bearing — closes the cross-lease leak)
- `frame_with_negative_seq__rejects_malformed`         (serde catches; verify)
- `frame_after_terminal__rejects`
- `wrong_bearer_header__rejects_unauthorized`
- `missing_worker_id_header__rejects_unauthorized`
- `wrong_worker_epoch__rejects_stale_epoch`
- `body_carries_idempotency_key__rejects_header_body_mismatch`
- `header_missing_idempotency_key__rejects_invalid_payload`
- `duplicate_idempotency_key__rejects_duplicate`
- `handshake_below_supported_min__rejects_unsupported_version` (positive structured error, not generic middleware reject)
- `handshake_above_supported_max__rejects_unsupported_version`
- `partial_response_body__classified_as_stream_end`

Golden bytes are **hand-curated** (not generated by the typed encoder
at test time). Each golden file is a small JSON-with-byte-comments
artifact committed to git that documents the canonical wire shape.
A separate unit test re-emits each golden via the typed encoder and
asserts byte-for-byte equality — if the encoder drifts away from the
hand-curated canonical, the test fails and the encoder must be fixed
(NOT the golden updated silently). Generating goldens from the
encoder at build time would defeat the purpose: encoder + decoder
could agree on a wrong wire format and the suite would happily pass.

### 4.5 `echo-worker` binary

Minimal Tokio binary, ~150 lines. Behavior:

- Reads `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`, `VOOM_WORKER_EPOCH`
  from env on startup.
- Binds an ephemeral 127.0.0.1 port, prints `BOUND addr=...` to
  stdout.
- Spawns a tokio task to read stdin in a loop; when stdin closes
  (parent dies), the task cancels the running operation and exits
  the process.
- Implements `OperationKind::ProbeFile` only: accepts a `payload`
  shaped `{ "path": "<string>" }`, emits one `Progress` frame at
  `seq = 0` carrying the path back, then one `Result` frame at
  `seq = 1` whose `payload` echoes the request, then closes the
  stream.
- An `OperationKind::ProbeFile` request with a malformed payload
  (missing `path`, wrong type) returns `ProtocolError::InvalidPayload`.
- Rejects every other `OperationKind` with
  `ProtocolError::UnknownOperation`. (`echo-worker` deliberately
  advertises support for `ProbeFile` only; the conformance suite
  uses the rejection paths as positive tests for the contract.)

`echo-worker` does NOT depend on `voom-fake-support` (which does not
exist yet — Phase 3 creates it).

## 5. Commit plan

Every commit ends `just ci` green. Sprint 1 tests stay passing.

### Commit 1 — `voom-core` additions
- `protocol_version: u32 = 1` constant exposed as `voom_core::PROTOCOL_VERSION`.
- `ErrorCode::WORKER_RETIRED`, `WORKER_INCARNATION_STALE`,
  `AMBIGUOUS_WORKER_SELECTION`.
- Matching `VoomError` cases and `error_code` mapping.
- Sibling unit-test additions for round-trip.

### Commit 2 — Empty `voom-worker-protocol` shell
- Replace placeholder with real module layout (per §3.1).
- Add workspace dependencies in `[workspace.dependencies]`:
  `secrecy = "0.10"` (used by `credentials.rs` in commit 6),
  `constant_time_eq = "0.4"` (also commit 6),
  `blake3 = "1"` (used by HTTP idempotency middleware in commit 8),
  `trybuild = "1"` as a dev-dependency on `voom-worker-protocol`
  (used by the PercentBps compile-fail test in commit 4),
  `serde_json = { version = "1", features = ["preserve_order"] }`
  on `voom-worker-protocol` (used by recursive idempotency-key scan
  in commit 8).
- All modules empty stubs; lib.rs re-exports nothing yet.
- Sprint 1 tests still green; clippy/rustfmt happy.

### Commit 3 — `OperationKind` + round-trip
- §3.3 enum with all variants.
- Sibling test exhaustively asserts snake_case serde round-trip and
  rejects unknown variants.

### Commit 4 — Wire envelope types
- §3.2 `OperationRequest`, `OperationResponse`, `ProgressFrame`,
  `ProtocolError`. Sibling tests for round-trip per variant.

### Commit 5 — Handshake + version negotiation
- §3.6 types + the supported-range check.

### Commit 6 — Credentials + constant-time compare
- §3.4 `WorkerCredentials`, `PresentedCredentials`,
  `validate_credentials`.
- Compile-time test asserts `WorkerCredentials` does NOT derive
  `Debug` exposing the secret (using `static_assertions`).
- Negative tests: wrong bearer, wrong worker_id, stale epoch.

### Commit 7 — NDJSON codec
- §3.5 reader + writer + all invariants from §4.2 of the overview.
- One sibling test per invariant in §3.5.

### Commit 8 — Transport traits + HTTP transport
- §3.7 traits.
- §3.8 `HttpClient` and `HttpServer`.
- Round-trip test: spawn `HttpServer` with a handler that returns
  one `ProbeFile` result; `HttpClient::dispatch` succeeds and
  yields the expected NDJSON sequence.

### Commit 9 — `low_level` raw-wire module
- §3.9 module with `http_raw` and `ndjson_raw`.
- Golden-bytes helpers.
- Sibling tests asserting raw bytes match typed-encoded bytes.

### Commit 10 — `voom-conformance` crate skeleton + Harness
- New crate. `Harness::new`, `Harness::run_typed_suite` returning an
  empty `SuiteResult` ("not implemented yet" gate so commits 11–12
  can fill it in).
- Wired into the workspace `[members]` and CI.

### Commit 11 — `echo-worker` binary + parent-death watchdog
- `voom-conformance/src/bin/echo_worker.rs` per §4.5.
- Smoke test in `voom-conformance/tests/` that spawns it, lets it
  bind, sends one `ProbeFile { path: "/tmp/x" }` over the typed API,
  asserts the result echoes the path.
- Tests for stdin-EOF causing exit within 100 ms.

### Commit 12 — Typed conformance suite + raw-wire suite
- §4.3 and §4.4 assertions, all green against `echo-worker`.
- Golden-bytes fixtures committed under
  `voom-conformance/src/fixtures/golden/`.
- Mutation fixtures generated by build script and verified at test
  time (so golden + mutation stay in lockstep).

### Commit 13 — CI wiring + workspace lint sweep
- `just ci` runs `cargo test -p voom-conformance` and the
  `echo-worker` smoke test.
- Workspace `cargo fmt`, `cargo clippy`, `cargo deny` pass.
- README in `voom-conformance` describing how to add a worker to the
  conformance gate (this is what Phase 3 / 4 / 5 will follow).

## 6. Exit criteria

The phase is complete and Phase 2 may begin when:

- All 13 commits land green on `feat/sprint-2`.
- `just ci` green at every commit.
- `voom-worker-protocol` exports the §3 API.
- `voom-conformance` runs `echo-worker` through both typed and
  raw-wire suites and exits 0.
- Every §4.3 typed assertion has a paired §4.4 raw-wire mutation
  where applicable.
- Sprint 1 tests untouched and passing.
- One adversarial-review round (up to three) has accepted the
  design.

## 7. Out of scope (deferred to Phase 2 or later)

- `LocalWorkerSupervisor` and any control-plane wiring.
- `WorkerSelector` trait.
- `worker_incarnations` table and migration 0005.
- Dispatch outbox (`lease_dispatch_intents`).
- Watchdog state machine.
- Any real worker logic beyond `echo-worker`'s `ProbeFile` echo.
- TLS, remote-node authentication (Sprint 4).
- Cancellation that does actual work (Phase 1 ships the route; Phase 2
  wires the supervisor's call).
- `voom-fake-support` and the eleven fakes (Phase 3).
- `chaos-worker`, `benchmark-worker` (Phases 4 / 5).
- Full conformance contract surface (Phase 6 extends the harness).
