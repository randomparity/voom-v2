---
name: voom-sprint-2-design
description: Sprint 2 (Synthetic Provider Suite MVP) overview design for VOOM — versioned HTTP/JSON worker protocol, local worker supervisor, eleven fake providers, chaos worker, benchmark worker, and provider conformance tests. Decomposes the sprint into six phases on `feat/sprint-2`, fixes cross-phase architectural decisions, and defers per-phase detail to the phase-level design docs.
status: proposed
date: 2026-05-19
sprint: 2
branch: feat/sprint-2
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md
  - docs/adr/0001-durable-jobs-over-events.md
  - docs/adr/0002-out-of-process-workers-only.md
  - docs/adr/0003-sqlx-and-tokio-foundation.md
  - docs/adr/0004-sibling-unit-tests.md
---

# VOOM Sprint 2 — Synthetic Provider Suite MVP

## 1. Goal & Scope

Sprint 1 turned the Sprint 0 skeleton into a durable-but-callerless control
plane. Sprint 2 puts callers on the other end of the lease lifecycle:
**out-of-process workers** that register with the control plane, accept
leases over a versioned HTTP/JSON protocol, stream structured progress,
and produce typed results. Every worker in Sprint 2 is **synthetic**;
no real media tooling (`ffmpeg`, `ffprobe`, `mkvmerge`) ships in this
sprint. Real media workers are Sprint 5.

The architectural-spec exit criteria for Sprint 2 are:

- A synthetic end-to-end plan runs through the real scheduler.
- Chaos tests cover worker crash, timeout, malformed result, and missed
  heartbeat.
- Benchmark worker reports scheduler throughput.

"The real scheduler" in this sprint is the Sprint 1 lease-acquire /
heartbeat / release / expire lifecycle, plus the new **local worker
supervisor** added in Phase 2 that drives the dequeue → dispatch → result
loop. Full multi-worker scoring (capability + locality + cost) is Sprint 4.

Out of scope for Sprint 2 (deferred to named later sprints):

- A policy grammar/compiler that produces multi-phase DAGs (Sprint 3).
- Authenticated worker registration and remote network leases over the
  wire (Sprint 4). Sprint 2 ships an in-process unix-socket / loopback
  transport that the protocol crate is structured to outgrow without an
  API break — see §4.1.
- A `nodes` table and node-level concurrency / locality / cost scoring
  (Sprint 4 — Sprint 2 reuses Sprint 1's `workers` table with `kind =
  'synthetic'` for every fake).
- Real media workers (Sprint 5).
- Filesystem watcher and continuous daemon loop (Sprint 6).
- Web UI (Sprint 7).
- Plugin SDK and namespaced operation schemas (Sprint 8).
- Approval gates, rollback flows, metrics endpoint, trace-ID propagation
  (Sprint 9).
- Production packaging (Sprint 10).

## 2. Phase Plan

Sprint 2 is committed to `feat/sprint-2` as six phases in order. Each
phase ships its own design doc, plan doc, implementation commits, an
adversarial-review round (up to three), and a `/simplify` pass before
the next phase begins. Each phase ends with `just ci` green at every
commit and the existing Sprint 1 tests still passing.

### Phase 1 — Worker protocol foundation

Crate: `voom-worker-protocol`. Adds the versioned HTTP/JSON wire
contract — operation request/response envelopes, progress-stream frames
(NDJSON), structured-error taxonomy mapped to the failure classes from
Sprint 1, and a typed `OperationKind` enum mirroring the fixed operation
vocabulary from the architectural spec.

The contract is transport-agnostic at the type level — `serde` types and
async traits only — but ships one concrete transport (`hyper` HTTP/1.1
over TCP loopback) so the rest of Sprint 2 can drive it. Remote
authenticated transport is explicitly Sprint 4.

**Exit:** Sprint 1 tests still green; `voom-worker-protocol` exports
`OperationRequest`, `OperationResponse`, `ProgressFrame`,
`ProtocolError`, `OperationKind`, and the
`{ClientHandle, ServerHandle}` traits the supervisor and workers will
implement in Phase 2 / Phase 3. Round-trip serde tests, NDJSON frame
parser tests, and version-negotiation tests pass.

### Phase 2 — Local worker supervisor

Crate: `voom-scheduler`. Adds the local supervisor that owns the
control-plane side of the worker protocol. It (a) spawns and supervises
local fake-worker processes via the Sprint 1 `WorkerRepo`, (b) dequeues
ready tickets via Sprint 1's `LeaseRepo::acquire_in_tx`, (c) dispatches
operations to one supervised worker over the protocol from Phase 1,
(d) consumes the progress stream and emits the corresponding Sprint 1
events (`worker.heartbeat`, `ticket.progress`, etc.), and (e) closes
the lease on result or heartbeat timeout.

No new tables. The supervisor reuses Sprint 1's `workers`, `leases`,
`tickets`, and `events` tables. Heartbeat timeout reuses
`LeaseRepo::expire_due`.

**Exit:** an end-to-end test in `crates/voom-scheduler/tests/` can
spawn a trivial echo worker, lease a synthetic ticket to it, observe
progress events in the durable event log, and assert clean release on
both success and crash paths.

### Phase 3 — Fake provider suite

Crate: `voom-fakes` (new). One binary target per fake worker (eleven
binaries) sharing a small library of helpers for HTTP setup, lease
loop, progress emission, and result envelopes. Every fake:

- `fake-scanner` — emits deterministic file-discovery scenarios from a
  scripted scenario file
- `fake-prober` — returns canned media snapshots
- `fake-transcoder` — simulates duration / progress / output size /
  codec change / failures
- `fake-remuxer` — simulates container and track mutations
- `fake-backup-store` — simulates local + object-store backup
- `fake-health-checker` — pass / fail / degraded
- `fake-identity-provider` — path / external-id / runtime / duplicate
  evidence
- `fake-external-system` — Plex/Jellyfin/Radarr/Sonarr-style reads,
  writes, path mappings, rate limits, refresh failures
- `fake-quality-scorer` — named-profile quality scores
- `fake-issue-provider` — durable issues with severity + priority
- `fake-use-lease-provider` — playback / external scan / manual lock

Each fake is driven by a deterministic scenario format (JSON or RON;
decided in the Phase 3 design) so tests are reproducible.

**Exit:** an end-to-end test runs a multi-step synthetic flow
(scan → probe → identity → quality → issue → use-lease) through the
real supervisor and asserts the durable event log matches the scripted
scenario.

### Phase 4 — Chaos worker

Crate: `voom-fakes` (additional binary `chaos-worker`). One worker
that, on operation, can: crash the process, stall past the heartbeat
deadline, emit a malformed result envelope, emit progress frames that
never converge to completion, and exceed the deadline. Failure mode is
selected per-lease by a header or operation argument so tests can
script specific scenarios.

**Exit:** integration tests in `voom-scheduler/tests/chaos/` cover all
four exit-criteria scenarios (crash, timeout, malformed result, missed
heartbeat) and assert the durable state — `terminal_failure` issues,
lease release reasons, retry classification per Sprint 1's
`FailureClass` taxonomy.

### Phase 5 — Benchmark worker

Crate: `voom-fakes` (additional binary `benchmark-worker`). A worker
that accepts a parametrized "no-op" operation and reports per-operation
latency + throughput from the supervisor's perspective, emitted as
structured progress frames the test harness collects. The harness
asserts a throughput floor so regressions in dispatch / heartbeat /
event-emit overhead are caught in CI.

**Exit:** `voom-scheduler/tests/benchmark.rs` records baseline numbers
(operations per second, p50 / p95 dispatch latency) on a fixed
configuration; thresholds chosen so a 2× regression fails the test.

### Phase 6 — Provider conformance tests

Crate: `voom-fakes` (new test binary / library `conformance`). A
protocol-level conformance suite that every Sprint 2 worker and any
future worker must pass: registration, capability advertisement, lease
accept, heartbeat cadence, progress frame schema, result envelope
schema, error envelope schema, cancellation, structured-error
classification. The suite runs every existing fake through every
contract assertion and exits non-zero on any failure.

**Exit:** `cargo test -p voom-fakes conformance --bin <every-fake>`
runs all eleven fakes plus chaos and benchmark through the same
contract suite; CI runs the conformance binary as part of `just ci`.

## 3. Workspace & Crate Deltas

| Crate | Sprint 2 contents added |
|---|---|
| `voom-worker-protocol` | Phase 1. Wire types (`OperationRequest`, `OperationResponse`, `ProgressFrame`, `ProtocolError`), version-negotiation handshake, NDJSON frame codec, transport traits (`ClientHandle`, `ServerHandle`), one concrete HTTP/1.1 loopback transport. |
| `voom-scheduler` | Phase 2. `LocalWorkerSupervisor` driving the dequeue → dispatch → progress → result loop. New `crates/voom-scheduler/tests/` integration suite. |
| `voom-fakes` | New crate (Phases 3 / 4 / 5 / 6). Eleven `fake-*` binaries, one `chaos-worker` binary, one `benchmark-worker` binary, one shared library of fake-worker primitives (`lease loop`, `scenario runner`, `result envelope helpers`), one `conformance` test harness. |
| `voom-control-plane` | Phase 2. New use-case method(s) the supervisor calls (e.g., `dequeue_ready_lease`, `report_progress`, `report_result`). No new repos; reuses Sprint 1. |
| `voom-core` | Phase 1 may add a small `protocol_version` constant and matching error-code variants if the protocol error taxonomy needs codes not already in Sprint 1. Kept minimal. |
| `voom-cli` | Phase 2 / Phase 3 may add read-only inspection verbs over progress events and supervisor state if Sprint 1's existing verbs are insufficient. Read-only only. |
| `voom-api`, `voom-events`, `voom-store`, `voom-policy`, `voom-plan`, `voom-artifact` | Untouched. No Sprint 2 deliverables land here. |

`voom-events` is deliberately not touched even though the supervisor
emits events — Sprint 1 already defined the relevant `EventKind`
variants (`worker.registered`, `lease.acquired`, `lease.heartbeat`,
`lease.released`, `ticket.progress`, `ticket.failed`, etc.). Phase 2's
job is to wire the supervisor into those existing variants, not invent
new ones. If a Sprint 2 phase truly needs a new event kind, the
delta is added to `voom-events` in that phase's plan with an explicit
note in the per-phase design.

## 4. Cross-phase architectural decisions

These decisions are fixed here so each per-phase design starts from a
shared baseline.

### 4.1 Transport: in-process spawn + HTTP/1.1 loopback

Workers are real OS processes spawned by the supervisor on the same
host. The supervisor talks to them over HTTP/1.1 on `127.0.0.1` with a
per-worker ephemeral port. The worker prints its bound port to stdout
on startup; the supervisor reads it before issuing the first request.
This is the cheapest realistic transport that exercises the same wire
format remote nodes will use in Sprint 4, with no authentication and
no network attack surface.

Sprint 4 will add TLS, an auth header, and remote node registration
without changing the protocol message shapes. The protocol crate's
public API is structured so callers never construct a raw
`hyper::Client` — they go through `ClientHandle` and `ServerHandle` —
and Sprint 4 swaps in an authenticated transport behind the same trait.

### 4.2 Progress stream: NDJSON over HTTP response body

The architectural spec offers NDJSON or SSE. Sprint 2 picks NDJSON
because it is trivially parseable by `serde_json::from_str` line-by-line,
agent-friendly (every frame is one JSON object), and does not require
the worker to implement SSE comment/keepalive semantics. SSE remains
available as a future Phase 1 extension if a streaming consumer needs
it; it is not required for Sprint 2.

### 4.3 Dispatch direction: supervisor pulls, worker accepts

The supervisor initiates every operation request (`POST /v1/operations`
to the worker's HTTP endpoint). Workers do not poll the control plane.
This gives the supervisor full control over backpressure, cancellation,
and per-worker concurrency, and matches the eventual Sprint 4 model
(scheduler dispatches; worker accepts).

Heartbeats are also worker → supervisor `POST /v1/leases/{id}/heartbeat`
calls. The supervisor's HTTP server (running as part of the control
plane process in Sprint 2) accepts them and refreshes
`leases.last_heartbeat_at` via Sprint 1's `LeaseRepo::heartbeat_in_tx`.
Missed heartbeats are detected by Sprint 1's `LeaseRepo::expire_due`,
which the supervisor runs periodically.

### 4.4 Worker lifecycle: spawned-and-supervised, one process per fake binary

The supervisor spawns each fake worker as its own OS process, holds the
`tokio::process::Child` handle, and joins it cleanly on shutdown. A
worker that exits unexpectedly is detected by both the
`tokio::process::Child` exit watcher (immediate, process-level signal)
and the heartbeat timeout (eventual, durable-state signal). The
supervisor records the crash via the existing Sprint 1 lease-failure
path. There is no in-process worker fast path; the architectural spec
forbids it (ADR-0002).

### 4.5 Capability advertisement: at registration time, durable in `worker_capabilities`

Workers advertise capabilities (operation kinds, codecs, hardware) in
the registration payload. The supervisor stores them via Sprint 1's
`WorkerRepo::register_in_tx`, which already writes to the existing
`worker_capabilities` table. No new schema in Sprint 2.

### 4.6 Determinism for synthetic providers

Every fake is deterministic given a `(scenario_path, seed)` pair. The
seed is reused across runs in CI. Tests assert exact event sequences,
exact result envelopes, and (for `chaos-worker` and
`benchmark-worker`) exact failure mode selection. Non-determinism is a
test bug, not a feature.

### 4.7 Conformance: contract suite is a test crate, not a runtime gate

Provider conformance tests (Phase 6) live in the `voom-fakes`
crate's test target, run on every worker as part of `just ci`, and
fail the build on any contract violation. They are not invoked by the
supervisor at runtime — the runtime trusts the wire contract. A future
sprint may add a runtime self-check to `voom-cli worker verify`; that
verb is out of scope for Sprint 2.

## 5. Cross-cutting test discipline

Sprint 2 keeps the Sprint 1 test layout — sibling `*_test.rs` files for
unit tests under `src/`, integration tests under `crates/*/tests/`.
Every new wire type gets a round-trip serde test. Every supervisor
state transition gets a unit test that drives it directly without
spawning a real process. The end-to-end tests in `voom-scheduler` and
`voom-fakes` are the only ones that actually spawn child processes;
they live behind the existing in-memory SQLite test harness and reuse
the same migration set.

`just check-test-layout` already enforces the sibling-tests convention
and is wired into `just ci`. No changes needed there.

## 6. Workflow

Per the goal directive driving this branch:

1. For each phase: design doc → adversarial review (≤ 3 rounds) → plan
   doc → adversarial review (≤ 3 rounds) → implementation commits →
   adversarial review (≤ 3 rounds) → `/simplify` once → next phase.
2. All phases land on `feat/sprint-2`. Every commit ends `just ci`
   green.
3. After Phase 6 completes, one PR opens against `main`. CI runs to
   green. The PR is **not** merged by this branch's owner; review +
   merge is a human gate.

## 7. Out-of-scope, explicitly

- Real `ffmpeg` / `ffprobe` / `mkvmerge` workers (Sprint 5).
- Authenticated remote-node registration over TLS (Sprint 4).
- A `nodes` table and node-level scoring (Sprint 4).
- Multi-worker scheduling decisions based on locality / cost (Sprint 4).
- Policy DAG compilation (Sprint 3); Sprint 2 hand-writes its
  end-to-end test "plans" as direct ticket-creation sequences.
- Filesystem watcher / continuous daemon (Sprint 6).
- Web UI (Sprint 7).
- Plugin SDK / namespaced operations (Sprint 8).
- Approval gates / rollback / metrics endpoint / trace-ID propagation
  (Sprint 9).
- Production packaging / upgrade migration tests (Sprint 10).

These deferrals match the architectural-spec sprint roadmap; Sprint 2
does not pull work forward from a later sprint just because it would
be convenient.
