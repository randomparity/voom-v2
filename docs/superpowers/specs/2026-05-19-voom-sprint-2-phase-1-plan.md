---
name: voom-sprint-2-phase-1-plan
description: Sprint 2 Phase 1 implementation plan — sequencing the 13 commits of the worker protocol foundation onto feat/sprint-2. Every commit ends `just ci` green. No design changes; when this plan and the Phase 1 design disagree, the Phase 1 design wins.
status: draft
date: 2026-05-19
sprint: 2
phase: 1
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-phase-1-design.md
overview_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
scope: commit-by-commit sequencing — no design changes
---

# Sprint 2 Phase 1 — Implementation Plan

## 1. Purpose

The Phase 1 design fixes every API decision, transport choice, and
conformance invariant for the worker protocol foundation. This plan
turns that into a thirteen-commit ordering where each commit ends
`just ci` green and Sprint 1 tests continue to pass.

When this plan and the Phase 1 design disagree, the Phase 1 design
wins. When the Phase 1 design and the Sprint 2 overview design
disagree, the overview wins.

## 2. Branch & merge plan

All thirteen commits land on `feat/sprint-2`, the branch created at
the start of Sprint 2. Phase 2 will continue the branch from the head
of this plan. One PR will open against `main` only after all six
phases complete (per the goal's branch policy).

## 3. Pre-decided judgment calls

Captured here so the per-commit plan is unambiguous:

| Decision | Choice | Rationale |
|---|---|---|
| `voom-conformance` is a regular crate, not a workspace-internal-only test crate | Regular crate published as part of the workspace | The crate exposes a public `Harness` API that Phase 2 / 3 / 4 / 5 will all consume; making it a published library keeps Sprint 4's remote conformance harness on the same code. |
| `echo-worker` lives inside `voom-conformance/src/bin/` | Yes | It is test infrastructure for the conformance harness. It is not a Sprint 2 deliverable; bundling avoids a `voom-echo-worker` crate that exists only to host one bin. |
| Hyper version | `hyper = "1"` (1.x) | Sprint 5's TLS work and ongoing maintenance both target hyper 1.x. `http = "1"` is the matching shared library. |
| `secrecy` version | `secrecy = "0.10"` | Latest stable, ships `SecretString::expose_secret` without the `secret-zeroize` feature dance. |
| `constant_time_eq` version | `constant_time_eq = "0.4"` | Latest stable; one function `constant_time_eq` taking `&[u8], &[u8]`. |
| `blake3` version | `blake3 = "1"` | Latest stable; used for idempotency-replay body hashing. |
| `trybuild` version | `trybuild = "1"` (dev-dep) | Latest stable; used only for the `PercentBps` compile-fail test. |
| `serde_json` `preserve_order` feature | Enabled on `voom-worker-protocol` only | Required by the recursive `idempotency_key` scan so a body that lists keys out of order is rejected at the same position the supervisor saw it. |
| Sibling-test layout | Per ADR-0004 | Every `_test.rs` file linked via `#[path]` from its sibling source; `just check-test-layout` enforces. |
| `LeaseId` / `TicketId` / `WorkerId` come from `voom-core` | Yes | They already exist (Sprint 1). The protocol crate re-uses them rather than re-defining. |
| `ProtocolError` exposed from `voom-worker-protocol`, not `voom-core` | Yes | These errors live on the wire and only `voom-worker-protocol` produces them. `voom-core::VoomError` is the durable-state error type and Phase 2's supervisor will map between the two. |
| Conformance suite uses `tokio::test` | Yes | Async I/O is everywhere; the harness is a `pub async fn run_*` and tests `await` it. |
| Golden bytes are hand-curated and committed verbatim | Yes | Per Phase 1 design §4.4. A unit test in `voom-conformance` re-emits each golden via the typed encoder and asserts byte-for-byte equality; mismatches fail the test, the golden is the source of truth. |
| Workspace deps registered in commit 2, used later | Yes | Per round-3 fix on the Phase 1 design. Each commit between 4 and 12 keeps `just ci` green without adding deps mid-sequence. |
| Phase 1 does NOT add `voom-core` API beyond the three error codes + protocol_version constant | Yes | `FailureClass::ProgressTimeout` lands in Phase 2 (added by the supervisor); `FailureClass::AmbiguousWorkerSelection` also Phase 2. |

## 4. Commit-by-commit plan

Every commit must end `just ci` green. Sibling tests land alongside
the code they cover.

### Commit 1 — `voom-core` additions
**`voom-core::error`**
- Add `ErrorCode::WORKER_RETIRED`, `WORKER_INCARNATION_STALE`,
  `AMBIGUOUS_WORKER_SELECTION`.
- Matching `VoomError(String)` variants and `error_code` mapping.
- Sibling test `error_test.rs` updated for round-trip and exhaustive
  match coverage.

**`voom-core::lib` (or wherever protocol-version lives)**
- `pub const PROTOCOL_VERSION: u32 = 1;`
- `pub const PROTOCOL_VERSION_SUPPORTED_MIN: u32 = 1;`
- `pub const PROTOCOL_VERSION_SUPPORTED_MAX: u32 = 1;`

**Exit:** `just ci` green. Sprint 1's existing tests unchanged.

### Commit 2 — `voom-worker-protocol` shell + workspace deps

**Workspace `Cargo.toml`**
- Add to `[workspace.dependencies]`:
  - `hyper = { version = "1", features = ["http1", "client", "server"] }`
  - `hyper-util = { version = "0.1", features = ["client-legacy", "tokio", "server"] }`
  - `http = "1"`
  - `http-body = "1"`
  - `http-body-util = "0.1"`
  - `bytes = "1"`
  - `secrecy = "0.10"`
  - `constant_time_eq = "0.4"`
  - `blake3 = "1"`
  - `async-trait = "0.1"`
  - `chrono = { version = "0.4", features = ["serde"] }`
  - `serde = { version = "1", features = ["derive"] }`
  - `serde_json = { version = "1", features = ["preserve_order"] }`
  - `tokio = { version = "1", features = ["full"] }`
  - `tracing = "0.1"`

**`crates/voom-worker-protocol/Cargo.toml`**
- Add the deps from above plus a `dev-dependencies` entry for
  `trybuild = "1"`.
- Reference `voom-core` via `voom-core.workspace = true`.

**`crates/voom-worker-protocol/src/`**
- Replace `lib.rs`'s placeholder with empty module declarations per
  Phase 1 design §3.1:
  ```rust
  pub mod envelope;
  pub mod handshake;
  pub mod credentials;
  pub mod operation_kind;
  pub mod ndjson;
  pub mod transport;
  pub mod http;
  pub mod low_level;
  ```
- Each module is one empty file with `//! placeholder` for now.
- `low_level/mod.rs` likewise empty.

**`crates/voom-worker-protocol/build.rs`** — none yet.

**Exit:** `cargo check -p voom-worker-protocol` succeeds; `cargo test
-p voom-worker-protocol` runs the (currently empty) suite green;
`just ci` green; Sprint 1 tests unchanged.

### Commit 3 — `OperationKind` (operation_kind.rs)

Implements Phase 1 design §3.3. Enum + sibling test
`operation_kind_test.rs`:
- Round-trip every variant through `serde_json::to_string` /
  `from_str`.
- Assert the snake_case strings exactly match the architectural-spec
  vocabulary (verify by string equality against a hand-coded
  expected list).
- Assert `serde_json::from_str("\"unknown_op\"")` returns
  `serde::de::Error`.

Re-export `OperationKind` from `voom-worker-protocol::lib`.

**Exit:** `just ci` green.

### Commit 4 — Wire envelope types + PercentBps (envelope.rs)

Implements Phase 1 design §3.2. `OperationRequest`,
`OperationResponse`, `PercentBps`, `ProgressFrame`, `ProtocolError`.
- `PercentBps` with private field, `TryFrom<u16>`, custom serde via
  `try_from = "u16"`, `From<PercentBps> for u16`, `ZERO` / `FULL`
  consts.
- Sibling test:
  - Round-trip every `ProgressFrame` variant.
  - Round-trip `OperationRequest` / `OperationResponse`.
  - `PercentBps`: 0, 10000 OK; 10001, 65535 reject.
  - Deserialize `10001` from JSON rejects.
  - Compile-fail trybuild test: a Rust file under
    `crates/voom-worker-protocol/tests/ui/percent_bps_private_field.rs`
    that tries `PercentBps { bps: 65535 }` and is asserted to fail
    compilation via `trybuild`.
  - `OperationRequest` rejects unknown top-level fields.

Re-export from `lib`.

**Exit:** `just ci` green.

### Commit 5 — Handshake + version negotiation (handshake.rs)

Implements Phase 1 design §3.6. `HandshakeRequest`,
`HandshakeResponse`, and helper:

```rust
pub fn negotiate(offered: u32) -> Result<HandshakeResponse, ProtocolError> {
    if offered < voom_core::PROTOCOL_VERSION_SUPPORTED_MIN
       || offered > voom_core::PROTOCOL_VERSION_SUPPORTED_MAX
    {
        return Err(ProtocolError::UnsupportedProtocolVersion {
            offered,
            supported_min: voom_core::PROTOCOL_VERSION_SUPPORTED_MIN,
            supported_max: voom_core::PROTOCOL_VERSION_SUPPORTED_MAX,
        });
    }
    Ok(HandshakeResponse { agreed: offered })
}
```

Sibling test pins:
- `negotiate(1)` returns `agreed = 1`.
- `negotiate(0)` returns `UnsupportedProtocolVersion`.
- `negotiate(2)` returns `UnsupportedProtocolVersion`.
- Round-trip `HandshakeRequest` and `HandshakeResponse`.

**Exit:** `just ci` green.

### Commit 6 — Credentials + constant-time compare (credentials.rs)

Implements Phase 1 design §3.4. `WorkerCredentials`,
`PresentedCredentials`, `validate_credentials`.
- `WorkerCredentials` carries `worker_id: WorkerId`,
  `worker_epoch: u64`, `secret: SecretString`.
- Does NOT derive `Debug`. A custom `Debug` impl prints
  `worker_id`/`worker_epoch` but `secret: "<redacted>"`.
- `PresentedCredentials` is the parsed-from-headers form: same
  fields but `secret: SecretString` (deserialized from the bearer).
- `validate_credentials(presented, live) -> Result<(), ProtocolError>`
  per design.

Sibling test pins:
- Wrong bearer → `UnauthorizedBearer`.
- Wrong worker_id → `UnknownWorkerId`.
- Stale epoch → `StaleWorkerEpoch`.
- Successful match returns `Ok(())`.
- Custom `Debug` prints `<redacted>` and not the secret bytes.

**Exit:** `just ci` green.

### Commit 7 — NDJSON codec (ndjson.rs)

Implements Phase 1 design §3.5. `NdjsonReader`, `NdjsonWriter`,
`NdjsonOutcome`.

Sibling test pins every reader invariant from the design:
- First frame, `seq = 0`, expected lease → OK.
- `seq ≤ last_seq` → dropped, no error.
- `seq > last_seq + 1` → `OutOfOrderFrame`.
- Wrong `lease_id` → `WrongLeaseId`, stream aborted.
- Line > `max_frame_bytes` → `FrameTooLarge`.
- Frame after terminal → `UnexpectedFrameAfterTerminal`.
- EOF without terminal → `StreamEnd { partial_bytes: n }`.
- EOF mid-frame → `MalformedFrame`.

Writer invariants:
- Second terminal emit → `Err(MalformedFrame)`.
- `seq` auto-increments.
- `emit` with frame whose `lease_id ≠ bound_lease_id` returns Err
  without writing.

**Exit:** `just ci` green.

### Commit 8 — Transport traits + HTTP transport (transport.rs, http.rs)

Implements Phase 1 design §3.7 and §3.8.

`transport.rs`:
- `NdjsonStream` type alias.
- `ClientHandle` and `ServerHandle` traits.
- `DispatchStream`, `ServerRunning` structs.

`http.rs`:
- `HttpClient` impl of `ClientHandle` over `hyper-util`'s
  `client::legacy::Client`.
- `HttpServer` impl of `ServerHandle` over `hyper::server::conn::http1`.
- `route_policy(method, path) -> Option<RoutePolicy>` exact-match
  router covering the matrix in §3.8 of the design.
- Middleware stack applied per route policy:
  1. Version (if gated).
  2. Auth (if gated).
  3. Idempotency: parse header, walk body `serde_json::Value`
     rejecting any `idempotency_key` field at any depth, look up in
     LRU keyed on `(idempotency_key, blake3(body_bytes))`.
- `IdempotencyCache`: simple `std::sync::Mutex<LruCache<(String, [u8; 32]), CachedResponse>>` with capacity 1024.
- `CachedResponse` stores the original `OperationResponse`
  serialized + the NDJSON body buffered in memory; replay rewrites
  both back to the new caller.

Integration test in `crates/voom-worker-protocol/tests/`:
- Spawn `HttpServer` with a one-handler that emits one `Progress`
  + one `Result` for `ProbeFile`.
- `HttpClient::dispatch` succeeds and yields the expected NDJSON
  sequence.
- `handshake(1)` succeeds; `handshake(0)` rejects.
- Auth: wrong bearer → 401-equivalent `UnauthorizedBearer`.
- Routing: GET handshake → 404; POST unknown → 404.

**Exit:** `just ci` green.

### Commit 9 — `low_level` raw-wire module (low_level/)

Implements Phase 1 design §3.9. `http_raw` and `ndjson_raw` modules
with the documented helpers. Sibling tests assert each helper
produces bytes identical to the typed encoder's output for a known
input. Golden helper `golden_line_bytes` returns canonical bytes
for a known `ProgressFrame`.

**Exit:** `just ci` green.

### Commit 10 — `voom-conformance` crate skeleton + Harness

New crate added to `[workspace] members`. Cargo.toml depends on
`voom-worker-protocol` (workspace) and `voom-core` (workspace) and
dev-deps `tokio = { features = ["macros"] }`.

`src/lib.rs`:
- Re-exports `Harness`, `SuiteResult`.

`src/harness.rs`:
- `Harness::new`, `Harness::env`.
- `Harness::run_typed_suite`, `Harness::run_raw_wire_suite`,
  `Harness::run_all` — each returns a `SuiteResult` with
  `passed: vec![]` and `failed: vec![]`. Commits 11–12 fill in.
- `Harness::spawn_worker` private helper that spawns the worker
  binary with the env vars and reads the `BOUND addr=...` line.

`src/lib_test.rs` and `src/harness_test.rs`: empty placeholders.

**Exit:** `just ci` green; `cargo test -p voom-conformance` runs
(no-op).

### Commit 11 — `echo-worker` binary + parent-death watchdog

`crates/voom-conformance/src/bin/echo_worker.rs`:
- Reads `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`, `VOOM_WORKER_EPOCH`.
- Builds `WorkerCredentials`.
- `HttpServer::serve` with a handler for `ProbeFile` per Phase 1
  design §4.5.
- Background tokio task reads `tokio::io::stdin()` in a loop;
  EOF cancels and exits the process.
- Prints `BOUND addr=...` immediately after `serve()` returns.

Smoke test in `crates/voom-conformance/tests/echo_smoke.rs`:
- Spawn `echo-worker` via `Harness`.
- Send one `ProbeFile { path: "/tmp/x" }` via `HttpClient`.
- Assert result echoes the path.
- Close stdin; assert child exits within 200 ms (use
  `tokio::time::timeout` over `Child::wait`).

**Exit:** `just ci` green; the smoke test passes.

### Commit 12 — Typed conformance suite + raw-wire suite + goldens

`src/typed_suite.rs`: implements every assertion from Phase 1
design §4.3 (all 19 cases). Each case is one async function
returning `Result<(), String>`; `Harness::run_typed_suite`
collects results into the `SuiteResult`.

`src/raw_wire_suite.rs`: implements every assertion from Phase 1
design §4.4 (all 22 cases). Uses `voom-worker-protocol::low_level`
to construct bytes. Mutations are pre-computed and applied at the
byte level.

`src/fixtures/golden/`: hand-curated JSON-with-byte-comments
fixture files committed to git. One per canonical frame /
envelope. A unit test `golden_matches_typed_encoder` walks every
fixture, parses it via the typed decoder, re-emits via the typed
encoder, and asserts byte-for-byte equality with the golden.

`src/fixtures/mutations/`: small build helper (compile-time
function, not build.rs) that constructs each mutation from the
matching golden. Mutations are applied at test time, not committed
as bytes.

Integration test in `crates/voom-conformance/tests/echo_full.rs`:
- Spawn `echo-worker` via `Harness`.
- Run `Harness::run_all` — assert `passed` contains all 41 cases
  (19 typed + 22 raw-wire), `failed` is empty.

**Exit:** `just ci` green; `Harness::run_all` against `echo-worker`
returns 41 passed / 0 failed.

### Commit 13 — CI wiring + workspace lint sweep + README

- `justfile`: confirm `just ci` runs `cargo test -p voom-conformance`.
- `crates/voom-conformance/README.md`: how to add a worker to the
  conformance gate. Three sections: "What the harness asserts",
  "Adding a worker binary", "Running locally".
- Workspace `cargo fmt --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo deny check` all green.
- `cargo audit` passes.

**Exit:** `just ci` green; documentation in place; the foundation
is ready for Phase 2 to build on.

## 5. Exit criteria for the phase

When commit 13 lands:

- All thirteen commits green at every commit.
- `voom-worker-protocol` exports the §3 API from the Phase 1 design.
- `voom-conformance::Harness::run_all` against `echo-worker` reports
  41 passed / 0 failed.
- `just ci` green.
- Sprint 1 tests unchanged and passing.

Then Phase 2 (Local Worker Supervisor) begins.
