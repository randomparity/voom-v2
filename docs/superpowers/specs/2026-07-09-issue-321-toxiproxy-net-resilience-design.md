---
name: toxiproxy-net-resilience-design
description: Opt-in network-resilience harness that injects real TCP faults between the worker HttpClient and HttpServer via Toxiproxy, asserting the client's timeout/error contract.
status: draft
date: 2026-07-09
issue: 321
references:
  - docs/adr/0033-toxiproxy-network-resilience-harness.md
  - docs/adr/0002-out-of-process-workers-only.md
  - docs/superpowers/specs/2026-05-25-chaos-librarian-e2e-design.md
  - crates/voom-worker-protocol/src/http/client.rs
---

# Toxiproxy Network-Resilience Harness (#321)

## 1. Goal

Verify that the worker wire client (`voom_worker_protocol::HttpClient`) behaves
correctly when the TCP connection to a worker misbehaves — resets, stalls, is
refused, or is merely slow — rather than hanging indefinitely, panicking, or
surfacing an untyped error. We prove this by injecting **real** TCP-layer faults
with [Toxiproxy](https://github.com/Shopify/toxiproxy) placed between a live
in-process `HttpServer` (the worker) and `HttpClient` (the control plane).

The acceptance target is the client's already-documented failure contract in
`crates/voom-worker-protocol/src/http/client.rs`:

- a bounded handshake round-trip (`DEFAULT_HANDSHAKE_TIMEOUT`, 10s) that yields
  `ProtocolError::Timeout` when the worker never replies;
- a bounded dispatch up to the first response line (`DEFAULT_DISPATCH_TIMEOUT`,
  30s) that yields `ProtocolError::Timeout` when the response line never
  arrives;
- a connection-level failure (RST, refused) that surfaces as a typed
  `ProtocolError` (an `InvalidPayload` request error), promptly, not after the
  deadline.

## 2. Why Toxiproxy (and why not the existing fakes)

`http_test.rs` already simulates a *stalled body* with a hand-rolled
`tokio::net::TcpListener` (`dispatch_returns_after_response_line_before_progress_stream_finishes`).
That approach can gate an application-layer write, but it cannot produce
genuine TCP-layer misbehavior: a peer RST mid-stream, a refused connection, an
added-latency-but-alive link, or a bandwidth-throttled body. Hand-rolling those
means re-implementing a fault-injecting TCP proxy — exactly what Toxiproxy is.
Toxiproxy is the industry-standard tool for this and is controlled entirely over
a small REST API, so the test harness needs no new Rust dependency (it drives
the API with `reqwest`, already a workspace dependency).

## 3. Scope

In scope:

- A new integration test file `crates/voom-worker-protocol/tests/net_resilience.rs`
  whose tests are `#[ignore]`d (they require an external `toxiproxy-server`
  process) and run only via the dedicated recipe/workflow below.
- A test-only Toxiproxy REST control helper (create proxy, add toxic, delete
  proxy) built on `reqwest`, living in the test file / its `tests/support`.
- A `scripts/net-resilience.sh` harness that provisions `toxiproxy-server`
  (pinned version + SHA256, or from `PATH`), starts it, runs the ignored tests,
  and tears the server down.
- Two `just` recipes: `net-resilience` (local; server from `PATH`) and
  `net-resilience-ci` (hermetic; pinned download). Neither is part of `just ci`.
- A dedicated `.github/workflows/net-resilience.yml` (workflow_dispatch +
  weekly schedule) that runs `just net-resilience-ci`, mirroring
  `chaos-e2e.yml` including the scheduled-failure tracking-issue job.

Out of scope:

- Any change to production code in `HttpClient`/`HttpServer`. This is a
  behavior-verification harness; if a test reveals a real client bug, that fix
  is a separate follow-up, not part of this issue.
- Fault injection at the full-CLI / real-worker-binary level (that is the
  chaos-librarian E2E lane). The single TCP surface in voom is the worker
  protocol boundary (ADR-0002), so the protocol crate is the correct and
  sufficient seam.
- Running these tests per-PR or on macOS CI. Per-PR gating is explicitly
  rejected in ADR-0033.
- Injecting faults into the NDJSON progress stream that follows the first
  response line. That stream is bounded by lease TTL / progress-idle timeouts,
  not by the client deadlines under test here; covering it is a possible future
  extension, not part of the first harness.

## 4. Design

### 4.1 Topology

Each test is self-contained and concurrency-safe (cargo runs tests in
parallel):

```
HttpClient ──TCP──▶ Toxiproxy proxy (127.0.0.1:<ephemeral>) ──TCP──▶ HttpServer (127.0.0.1:0)
             fault injected here ▲
```

1. The test binds a real `HttpServer` (from `running_server`-style setup) on
   `127.0.0.1:0` and reads its resolved `SocketAddr` (the upstream).
2. The test creates a **uniquely named** Toxiproxy proxy via
   `POST /proxies` with `listen: "127.0.0.1:0"` and `upstream: "<server addr>"`,
   and reads the resolved `listen` address from the response.
3. The test points `HttpClient` at the proxy's resolved listen address.
4. The test adds the toxic under test via
   `POST /proxies/{name}/toxics`, then exercises `handshake`/`dispatch` and
   asserts the resulting `ProtocolError`.
5. The test deletes the proxy (`DELETE /proxies/{name}`) on completion.

Unique proxy names + per-test upstream servers make concurrent tests
independent. The `just`/script harness starts a fresh `toxiproxy-server` per
invocation and kills it on exit, so cross-run state is always clean; a
panicked test leaking a uniquely named proxy is therefore harmless.

### 4.2 Timeout control

To keep tests fast and deterministic, timeout-path tests construct the client
with `HttpClient::with_timeouts(proxy_addr, short, short)` (e.g. 500 ms) rather
than waiting the production 10s/30s. This is the documented purpose of
`with_timeouts` ("Used by tests to drive the timeout paths without waiting the
production defaults"). The assertion is the *typed error*, not the numeric
duration.

### 4.3 Scenarios (first harness — high-signal, one per contract path)

| # | Toxic (Toxiproxy) | Client call | Expected result |
|---|---|---|---|
| 1 | `timeout` (`timeout=0`, data stops) | `handshake`, short timeout | `ProtocolError::Timeout` |
| 2 | `timeout` (`timeout=0`, data stops) | `dispatch` before response line, short timeout | `ProtocolError::Timeout` |
| 3 | `reset_peer` (`timeout=0`, immediate RST) | `handshake` | typed `ProtocolError` (connection error → `InvalidPayload`), returned well within the deadline (proves no hang) |
| 4 | proxy created then `DELETE`d / disabled → connection refused | `handshake` | typed `ProtocolError` (`InvalidPayload`), prompt |
| 5 | `latency` (fixed delay **under** the client deadline) | `handshake` | `Ok` — regression guard that a slow-but-alive link is not spuriously failed |

Scenario 5 encodes intent (AGENTS.md Rule 9): it fails if a future change makes
the client's deadline too aggressive, which pure fault tests would not catch.

The exact `ProtocolError` variant for scenarios 3–4 is pinned during TDD against
observed `hyper` behavior; the spec asserts "a typed error, promptly" so a
connection error is never allowed to become a hang or a panic. If observed
behavior is a distinct-but-reasonable variant, the test records it — the intent
(typed, prompt, non-hanging) is the invariant.

### 4.4 Toxiproxy control helper

A small test-only struct (no production surface):

```rust
struct Toxiproxy { base: String, http: reqwest::Client }
impl Toxiproxy {
    fn from_env() -> Self;                 // TOXIPROXY_ADDR, default 127.0.0.1:8474
    async fn create_proxy(&self, name, upstream: SocketAddr) -> SocketAddr; // resolved listen
    async fn add_toxic(&self, name, toxic: serde_json::Value);
    async fn delete_proxy(&self, name);
}
```

If the Toxiproxy server is unreachable when these `#[ignore]`d tests are
explicitly invoked, the helper **fails loud** (panics with an actionable
message pointing at `just net-resilience`) rather than skipping — a missing
server under explicit invocation is a setup error, not a skip condition
(AGENTS.md Rule 12). `#[ignore]` is what keeps them out of `just test`.

### 4.5 Provisioning

`toxiproxy-server` is pinned at **v2.12.0**. `scripts/net-resilience.sh`:

- resolves the platform (`uname -s`/`-m`), and either finds `toxiproxy-server`
  on `PATH` (local default) or, when `NET_RESILIENCE_DOWNLOAD=1` (CI), downloads
  the pinned release asset and verifies it against the checked-in SHA256 for
  that platform before use;
- starts the server bound to `127.0.0.1:8474` (or `TOXIPROXY_ADDR`), waits for
  its `/version` endpoint to answer, then runs the ignored tests;
- kills the server and cleans up on `EXIT`/`INT`/`TERM` via a `trap`.

Pinned SHA256 (v2.12.0 `toxiproxy-server-<platform>`):

| platform | sha256 |
|---|---|
| linux-amd64 | `556d891134a3c582dc1e1a3f7335fd55142e5965769855a00b944e13e48302fc` |
| linux-arm64 | `53e770c1c3035b5a9f1bc629fce537db1f95f62b26f4ebe6e756afd701cf077c` |
| darwin-amd64 | `9625bba4bd96117eedae49f982aba4c2f462b268dd406c9ff18186f9b1ef8afe` |
| darwin-arm64 | `aa299966b52f16a8594f1cd0d1e9049dc2e8fe2c04a90c19860e2719b2b95d15` |

Unlike the ffmpeg BtbN case (rolling builds, no stable digest — see the
`chaos-e2e.yml` rationale), Toxiproxy publishes immutable versioned release
assets, so a real SHA256 pin is both possible and correct here.

## 5. Non-goals / rejected

See ADR-0033 for the gating decision (opt-in vs per-PR), the "no new Rust
dependency" decision, and the "harness only, no production change" boundary.

## 6. Success criteria

- `just net-resilience-ci` provisions Toxiproxy, runs the ignored suite, and
  exits non-zero if any scenario's assertion fails; the server process is
  always reaped.
- All five scenarios pass against the current `HttpClient` (or a real client
  defect is surfaced and filed).
- `just ci` is unchanged and does not run these tests.
- The new workflow runs on dispatch and weekly, opening a tracking issue on
  scheduled failure (mirroring `chaos-e2e.yml`).
- No production code under `crates/voom-worker-protocol/src/` changes.
