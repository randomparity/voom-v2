---
status: accepted
date: 2026-07-09
deciders: [VOOM core]
---

# 0033 — Toxiproxy network-resilience harness at the worker-protocol boundary

## Context

Issue #321 asks us to "add Toxiproxy to the test stack to verify proper network
resilience in the face of a misbehaving TCP connection." Voom has exactly one
TCP surface: the control plane dispatches every unit of media work to
out-of-process workers over the HTTP/NDJSON worker protocol (ADR-0002); there is
no in-process fast path. That boundary is `voom_worker_protocol::HttpClient`
talking to `HttpServer`, and the client already carries a deliberate failure
contract — a bounded handshake (`DEFAULT_HANDSHAKE_TIMEOUT`), a bounded dispatch
up to the first response line (`DEFAULT_DISPATCH_TIMEOUT`), and typed
`ProtocolError` variants — documented in
`crates/voom-worker-protocol/src/http/client.rs`.

Today that contract is exercised only by hand-rolled in-process fakes
(`http_test.rs`), which can gate an application-layer body write but cannot
produce real TCP-layer faults: a peer RST mid-stream, a refused connection, a
throttled or latency-injected-but-alive link. Verifying resilience to a
"misbehaving TCP connection" requires a real fault-injecting TCP proxy between
client and server. Toxiproxy is the established tool for exactly this and is
driven entirely over a small REST API.

Three decisions have viable alternatives and are settled here: where the tests
live and what they may change; how they are gated in CI; and how Toxiproxy is
controlled and provisioned.

Design doc:
[`docs/superpowers/specs/2026-07-09-issue-321-toxiproxy-net-resilience-design.md`](../superpowers/specs/2026-07-09-issue-321-toxiproxy-net-resilience-design.md).

## Decision

1. **Inject faults at the worker-protocol boundary, as a harness only.** New
   `#[ignore]`d integration tests in
   `crates/voom-worker-protocol/tests/net_resilience.rs` place a Toxiproxy proxy
   between a live in-process `HttpServer` and `HttpClient`, and assert the
   client's existing timeout/error contract under real TCP faults (data-stop
   timeout, `reset_peer`, connection-refused, sub-deadline latency). No
   production code under `src/` changes. If a scenario surfaces a real client
   defect, that fix is a separate, deliberate follow-up — the harness's job is to
   make such a defect visible and reproducible.

2. **Gate opt-in, not per-PR** — mirror the chaos-librarian E2E lane. The tests
   are `#[ignore]`d (excluded from `just test`/`just ci`), driven by a dedicated
   `scripts/net-resilience.sh` behind `just net-resilience` (local, server from
   `PATH`) and `just net-resilience-ci` (hermetic, pinned download), and run by a
   new `.github/workflows/net-resilience.yml` on `workflow_dispatch` and a weekly
   schedule, with the same scheduled-failure tracking-issue job as
   `chaos-e2e.yml`.

3. **Drive Toxiproxy over its REST API with `reqwest`; add no Rust dependency.**
   The test harness creates/deletes proxies and adds toxics with the `reqwest`
   client already in the workspace. Provision `toxiproxy-server` pinned at
   **v2.12.0**, SHA256-verified per platform on download (Toxiproxy ships
   immutable versioned assets, so a real digest pin is correct here — unlike the
   rolling ffmpeg build the `chaos-e2e` workflow documents).

## Consequences

- The single real TCP surface gets real fault coverage without touching
  production code and without a new dependency. The client's timeout/error
  contract becomes an executable, reproducible guarantee instead of a
  code-comment intention.
- Per-PR CI is unchanged: no external binary, no background server, no added
  flake surface on the critical path across ubuntu + macOS runners. The cost is
  that a resilience regression is caught at next dispatch/weekly run rather than
  on the introducing PR — acceptable because the covered code (the client
  deadlines) changes rarely and the tests can be dispatched on demand against any
  branch.
- The tests fail loud if `toxiproxy-server` is unreachable when explicitly
  invoked (AGENTS.md Rule 12); `#[ignore]` — not a silent skip — is what keeps
  them out of the default test run, so a missing server is never mistaken for a
  pass.
- Bumping Toxiproxy is a deliberate change: update the version and the
  per-platform SHA256 map in one place (`scripts/net-resilience.sh` / spec
  table).
- Extending coverage into the NDJSON progress stream (bandwidth/slow-close on the
  post-response body, bounded by lease TTL rather than the client deadlines) is a
  future addition on the same harness; the first cut stays high-signal and small
  (AGENTS.md Rule 3).

## Considered & rejected

- **Gate per-PR in `just ci`.** Fast feedback, but it puts an external Go binary
  and a background server on the critical path of every PR on both ubuntu-latest
  and macos-latest, adding provisioning and flake surface to unrelated changes.
  The repo already sequesters external-tool-heavy tests (chaos-librarian, real
  ffmpeg) behind `#[ignore]` + a scheduled workflow; resilience tests share that
  "needs an external process" character and belong in the same lane
  (AGENTS.md Rule 11, conformance over taste).
- **Local-only recipe with no workflow.** Lowest CI cost, but with no scheduled
  run the tests silently rot — the exact failure mode the `chaos-e2e.yml` weekly
  cron was added to prevent. A dispatch + weekly workflow keeps them honest.
- **Inject faults at the full-CLI / real-worker-binary level.** Higher fidelity
  but far heavier, and it duplicates the chaos-librarian E2E lane while obscuring
  which layer the fault exercises. The protocol crate is the narrowest seam that
  contains the entire TCP contract, so it is the correct and sufficient place
  (AGENTS.md Rule 4, surgical changes).
- **Add a dedicated Toxiproxy Rust client crate** (e.g. a `toxiproxy_rust`
  binding). Rejected: the REST surface we need is three calls, `reqwest` is
  already present, and each dependency is attack surface and maintenance burden
  for no gain (AGENTS.md Rule 3; global "justify new dependencies").
- **Rewrite the existing in-process fakes to simulate TCP faults.** Rejected:
  simulating RST/refused/throttle faithfully means re-implementing a
  fault-injecting proxy — reinventing Toxiproxy, less faithfully, inside the test
  crate.
- **Pin Toxiproxy by a rolling `latest` URL with a runtime version assertion**
  (the approach `chaos-e2e.yml` uses for ffmpeg). Rejected here because that
  pattern exists only to cope with BtbN's garbage-collected rolling ffmpeg
  builds; Toxiproxy publishes stable versioned assets, so a real SHA256 pin is
  both available and strictly safer.
