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

**Execution model.** The harness runs the `net_resilience` test binary
**single-threaded** (`cargo test ... -- --ignored --test-threads=1`). The suite
is five short tests; serializing them is cheap and removes cross-test races by
construction. Unique proxy names + per-test upstream servers remain as
defense-in-depth. No scenario depends on a freed ephemeral port staying unbound
(scenarios 3–4 use a `reset_peer` toxic injected directly at the proxy — see
§4.3), so there is no port-reuse race to serialize against in the first place.
The `just`/script harness starts a fresh `toxiproxy-server` per invocation and
kills it on exit, so cross-run state is always clean; a panicked test leaking a
uniquely named proxy is harmless.

### 4.2 Timeout control

To keep tests fast and deterministic, timeout-path tests construct the client
with `HttpClient::with_timeouts(proxy_addr, short, short)` (e.g. 500 ms) rather
than waiting the production 10s/30s. This is the documented purpose of
`with_timeouts` ("Used by tests to drive the timeout paths without waiting the
production defaults"). The assertion is the *typed error*, not the numeric
duration.

### 4.3 Scenarios (first harness — high-signal, one per contract path)

All toxics use `stream: "downstream"` (server→client direction), i.e. they act
on the bytes flowing back to the client — the path whose stall/reset/latency the
client's read deadline and transport error handling actually govern. Direction
is pinned explicitly so a future edit cannot silently swap which path is
exercised.

| # | Fault (`stream: downstream`) | Client call | Client config | Expected result |
|---|---|---|---|---|
| 1 | `timeout` toxic (`timeout=0`, data stops) | `handshake` | `with_timeouts(500ms, 500ms)` | `ProtocolError::Timeout` |
| 2 | `timeout` toxic (`timeout=0`, data stops) | `dispatch` (response head blocked) | `with_timeouts(500ms, 500ms)` | `ProtocolError::Timeout` |
| 3 | `reset_peer` toxic (`timeout=0`, immediate RST) | `handshake` | `with_timeouts(2s, 2s)` | `ProtocolError::InvalidPayload` whose `detail` begins `request:` |
| 4 | `reset_peer` toxic (`timeout=0`, immediate RST) | `dispatch` before response line | `with_timeouts(2s, 2s)` | `ProtocolError::InvalidPayload` whose `detail` begins `request:` |
| 5 | `latency` toxic (fixed **200 ms**) | `handshake` | `HttpClient::new` (production `DEFAULT_HANDSHAKE_TIMEOUT`, 10 s) | `Ok(HandshakeResponse)` with `agreed == offered` |

**Why these assertions, precisely.** The client's contract maps a connection
fault (connect/transport error from `client.request(..).await`) to
`ProtocolError::InvalidPayload { detail: "request: …" }` and a deadline expiry to
`ProtocolError::Timeout` (`client.rs`). So:

- Scenarios 1–2 assert `Timeout` — the hang-until-deadline path, on the
  handshake and dispatch deadlines respectively. Note a precise limit of
  scenario 2: a full-downstream `timeout` toxic blocks **all** server→client
  bytes, including the HTTP response head, so the client hangs at
  `client.request(..).await` (which awaits the head) and the `dispatch_timeout`
  wrapper fires there — it does **not** reach `read_response_line`. Scenario 2
  therefore verifies that the *dispatch deadline* (distinct constant and method
  from scenario 1's handshake deadline) bounds a stalled response, not the
  response-line-read seam specifically. Covering the "head arrives, then the
  response line stalls" sub-path needs a head-through-then-stall toxic
  (`bandwidth` rate 0 after N bytes, or `data_limit`) and is a deliberate future
  extension, kept out of this first high-signal cut (AGENTS.md Rule 3).
- Scenarios 3–4 assert **not `Timeout`, and specifically `InvalidPayload` whose
  `detail` starts with `request:`**, using a `reset_peer` toxic on the handshake
  and dispatch paths respectively. The variant alone already separates "prompt
  connection fault" from "hung until deadline", so no wall-clock threshold is
  asserted (an `elapsed < deadline` check adds no signal and a tight bound flakes
  under CI load). The `request:` prefix disambiguates the overloaded
  `InvalidPayload` (which also covers encode/build/body errors) so the test
  cannot pass on an unrelated payload error. `reset_peer` is chosen over a
  "dead upstream / refused connection" mechanism because it injects the RST
  directly at the proxy and therefore **depends on no ephemeral port staying
  unbound** — a bound-then-dropped port is itself a freed port the kernel may
  reassign, so a refused-connection scenario would merely relocate the
  port-reuse race rather than remove it. Splitting reset across handshake (3)
  and dispatch (4) also covers both distinct connection-fault code paths in the
  client (`handshake`'s and `dispatch`'s `request().await`).
- Scenario 5 encodes intent (AGENTS.md Rule 9). It uses the **default-constructed
  client** (`HttpClient::new`, production `DEFAULT_HANDSHAKE_TIMEOUT` = 10 s), not
  `with_timeouts`, precisely so the assertion is tied to the *production* deadline
  rather than a synthetic one: a 200 ms injected latency completes with a wide
  (~9.8 s) margin, so the test does not flake under CI jitter, and it would trip
  only if a future change tightened `DEFAULT_HANDSHAKE_TIMEOUT` below the injected
  latency. It is a **combined liveness+latency** assertion — it requires the
  in-process `HttpServer` to complete a real handshake through the proxy
  (`agreed == offered`), so a regression that breaks the handshake path, not just
  an over-aggressive deadline, also fails it. (Because a latency toxic delays but
  does not stop data, driving it against the real 10 s deadline stays fast; the
  short `with_timeouts` used by the fault scenarios is unnecessary here.)

If TDD observes that `hyper` surfaces an RST as a distinct-but-reasonable
variant/detail, the test records the observed value and this table is updated to
match; the invariant is that a connection fault yields a **typed, non-`Timeout`,
non-hanging** error, never a hang or a panic.

### 4.4 Toxiproxy control helper

A small test-only struct (no production surface):

```rust
struct Toxiproxy { base: String, http: reqwest::Client }
impl Toxiproxy {
    fn from_env() -> Self;                 // requires TOXIPROXY_ADDR — no default
    async fn create_proxy(&self, name, upstream: SocketAddr) -> SocketAddr; // resolved listen
    async fn add_toxic(&self, name, toxic: serde_json::Value);
    async fn delete_proxy(&self, name);
}
```

`from_env` **requires** the `TOXIPROXY_ADDR` environment variable and panics
with an actionable message ("set TOXIPROXY_ADDR or run `just net-resilience`")
when it is unset. It deliberately has **no `127.0.0.1:8474` default**: the
wrong-server guard must be a property of the code path, not of one entry point.
`#[ignore]` permits a developer to run `cargo test --ignored` directly, bypassing
the shell harness; without a default, that direct path also fails loud instead
of silently driving whatever foreign toxiproxy happens to sit on the well-known
8474 port. The `scripts/net-resilience.sh` harness is the sole thing that sets
`TOXIPROXY_ADDR`, and it points it only at a server the script itself started
(§4.5). If the address is set but the server is unreachable, the helper likewise
**fails loud** rather than skipping — a missing server under explicit invocation
is a setup error, not a skip condition (AGENTS.md Rule 12). `#[ignore]` is what
keeps these tests out of `just test`.

### 4.5 Provisioning

`toxiproxy-server` is pinned at **v2.12.0**. `scripts/net-resilience.sh`:

- resolves the platform (`uname -s`/`-m`), and either finds `toxiproxy-server`
  on `PATH` (local default) or, when `NET_RESILIENCE_DOWNLOAD=1` (CI), downloads
  the pinned release asset and verifies it against the checked-in SHA256 for
  that platform before use;
- picks the control-API address — `TOXIPROXY_ADDR` if set, otherwise
  `127.0.0.1:8474` — and then, **for whichever address was chosen**, fails loud
  before starting if it is already listening (`nc -z` / `/dev/tcp` probe). The
  probe runs unconditionally on the resolved address, not only on the default
  branch, so an operator who points `TOXIPROXY_ADDR` at a busy or stale address
  gets the same actionable collision message as the default-port case. This
  prevents the silent-wrong-server failure mode where a pre-existing toxiproxy
  answers the readiness probe and the script runs the suite against a server it
  did not start and cannot reap. The operator resolves the collision by stopping
  the other server or choosing a free `TOXIPROXY_ADDR`;
- starts the server bound to the chosen address, then confirms readiness by
  polling `/version` **only after** the just-spawned PID is alive, so readiness
  is attributed to the script's own server;
- exports `TOXIPROXY_ADDR` into the test environment so the `from_env` helper and
  the server always agree on the control address;
- kills the spawned server (by PID) and cleans up on `EXIT`/`INT`/`TERM` via a
  `trap`.

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

- `just net-resilience-ci` provisions Toxiproxy, runs the ignored suite
  single-threaded, and exits non-zero if any scenario's assertion fails; the
  server process is always reaped.
- **Done always maps to a green suite.** Either (1) all five scenarios pass
  against the current `HttpClient`, or (2) a scenario surfaced a real client
  defect, a follow-up issue was filed, and *that specific scenario* is excluded
  from the harness run by a `--skip <test_name>` filter added to the
  `cargo test` line in `scripts/net-resilience.sh`, with a comment naming the
  follow-up issue. `#[ignore]` cannot serve as the quarantine: every test in this
  file is already `#[ignore]`d and the harness opts them in with `--ignored`, so
  `--ignored` *runs* ignored tests — adding another `#[ignore]` would not exclude
  the scenario. `--skip` is the mechanism compatible with `--ignored`; it keeps
  the harness green while the defect is tracked, and removing the `--skip` line
  is the single visible step that re-arms the scenario once the defect is fixed.
- `just ci` is unchanged and does not run these tests.
- The new workflow runs on dispatch and weekly, opening a tracking issue on
  scheduled failure (mirroring `chaos-e2e.yml`).
- No production code under `crates/voom-worker-protocol/src/` changes.
