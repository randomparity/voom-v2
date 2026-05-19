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
- TLS-authenticated worker registration and remote network leases
  (Sprint 4). Sprint 2 ships a bearer-token-authenticated HTTP/1.1
  loopback transport (§4.1); the protocol crate is structured to
  outgrow it without an API break.
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

### Phase 1 — Worker protocol foundation + bootstrap conformance

Crates: `voom-worker-protocol` (wire types) and `voom-conformance` (the
black-box contract harness). Adds the versioned HTTP/JSON wire
contract — operation request/response envelopes, progress-stream
frames (NDJSON with the framing invariants in §4.2), structured-error
taxonomy mapped to the failure classes from Sprint 1, a typed
`OperationKind` enum mirroring the fixed operation vocabulary from the
architectural spec, and the local-identity model (per-spawn bearer
token + `worker_id` + `worker_epoch` validation) from §4.1.

`voom-conformance` ships in the same phase so the protocol does not
ship without an authoritative checker. The crate is intentionally
independent of any fake implementation: it only knows how to launch a
worker binary, drive it over the public protocol, and assert contract
invariants. The phase ships one binary `echo-worker` that the
conformance suite runs against — it is the only worker binary in
Phase 1 and exists solely to validate the contract end-to-end.

The contract is transport-agnostic at the type level — `serde` types
and async traits only — but ships one concrete transport (`hyper`
HTTP/1.1 over TCP loopback, bearer-token authenticated per-spawn).
Remote authenticated TLS transport is Sprint 4 and reuses the same
`{ClientHandle, ServerHandle}` traits without changing the message
shapes.

**Exit:** Sprint 1 tests still green; `voom-worker-protocol` exports
`OperationRequest`, `OperationResponse`, `ProgressFrame`,
`ProtocolError`, `OperationKind`, `WorkerCredentials`, and the
`{ClientHandle, ServerHandle}` traits the supervisor and workers will
implement in later phases. Round-trip serde tests, NDJSON frame
parser tests, version-negotiation tests, and bearer-token /
worker-identity negative tests pass. `voom-conformance` runs the
`echo-worker` through the contract suite green and exits non-zero on
any deliberate-mutilation test scenario.

### Phase 2 — Local worker supervisor

Crate: `voom-control-plane`. Adds the local supervisor that owns the
control-plane side of the worker protocol. The supervisor lives in
`voom-control-plane` because Sprint 1 already establishes
`voom-control-plane` as the sole layer that composes durable state
mutations with event writes inside one transaction (see Sprint 1 §5.2);
making the supervisor a control-plane use-case keeps that invariant
without duplication. `voom-scheduler` remains empty in Sprint 2 and is
reserved for the multi-worker selection logic Sprint 4 needs (it will
expose a `WorkerSelector` trait; the Phase 2 supervisor will consume
it through that trait in Sprint 4, but no `WorkerSelector` ships in
Sprint 2 — there is only one worker per operation).

The supervisor (a) registers and supervises local worker processes
via Sprint 1's `WorkerRepo`, (b) dequeues ready tickets via Sprint 1's
`LeaseRepo::acquire_in_tx`, (c) dispatches operations to a supervised
worker over the protocol from Phase 1, (d) consumes the progress
stream and emits the corresponding Sprint 1 events
(`worker.heartbeat`, `ticket.progress`, etc.), and (e) closes the
lease on result or heartbeat timeout. Every durable mutation goes
through a `ControlPlane` use-case method that composes the repo
`_in_tx` call with the matching `EventRepo::append_in_tx` in a single
transaction — the supervisor never writes to a repo without going
through this layer.

The Phase 1 conformance harness gates this phase: Phase 2 may not
declare exit until the supervisor passes a conformance run against
`echo-worker` (validating that the supervisor's interpretation of the
protocol agrees with the harness's interpretation, before any second
worker implementation exists).

No new tables. The supervisor reuses Sprint 1's `workers`, `leases`,
`tickets`, and `events` tables. Heartbeat timeout reuses
`LeaseRepo::expire_due`.

**Exit:** an end-to-end test in `crates/voom-control-plane/tests/` can
register `echo-worker` with the supervisor, lease a synthetic ticket
to it, observe progress events in the durable event log, and assert
clean release on both success and crash paths. Conformance against
`echo-worker` passes from the supervisor side.

### Phase 3 — Fake provider suite

Crates: `voom-fake-support` (new shared library) and `voom-fakes` (new
binaries crate). The shared library carries the lease loop, scenario
runner, progress emitter, and result-envelope helpers; the binaries
crate hosts the eleven fake binaries. Splitting the helper library
out of the binaries crate ensures the Phase 1 conformance harness can
launch each fake without depending on the same helpers (the harness
talks only to the public protocol, never to `voom-fake-support`), so
contract drift in the helper cannot hide behind a family of binaries
that all share the same bug.

Every Phase 3 binary must pass the Phase 1 conformance harness as a
gate before its specific E2E scenario is written. Specifically:

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
real supervisor, every fake passes the conformance harness, and the
durable event log matches the scripted scenario.

### Phase 4 — Chaos worker

Crate: `voom-fakes` (additional binary `chaos-worker`, but the binary
itself uses only `voom-worker-protocol` plus a tiny private helper —
not `voom-fake-support` — so its failure modes cannot accidentally
ride on the shared fake helpers). One worker that, on operation, can:
crash the process, stall past the heartbeat deadline, emit a malformed
result envelope, emit progress frames that never converge to
completion, and exceed the deadline. Failure mode is selected
per-lease by a header or operation argument so tests can script
specific scenarios.

`chaos-worker` must pass the conformance harness for its non-faulting
operations (registration, baseline echo) — the harness only fails on
contract violations during steady-state behavior, not on operations
that are explicitly faulting per scenario.

**Exit:** integration tests in `voom-control-plane/tests/chaos/` cover
all four exit-criteria scenarios (crash, timeout, malformed result,
missed heartbeat) and assert the durable state — `terminal_failure`
issues, lease release reasons, retry classification per Sprint 1's
`FailureClass` taxonomy.

### Phase 5 — Benchmark worker

Crate: `voom-fakes` (additional binary `benchmark-worker`, independent
of `voom-fake-support` for the same independence reason as chaos
above). A worker that accepts a parametrized "no-op" operation and
reports per-operation latency + throughput from the supervisor's
perspective, emitted as structured progress frames the test harness
collects. The harness asserts a throughput floor so regressions in
dispatch / heartbeat / event-emit overhead are caught in CI.

`benchmark-worker` must pass the conformance harness before being
admitted to the throughput suite.

**Exit:** `voom-control-plane/tests/benchmark.rs` records baseline
numbers (operations per second, p50 / p95 dispatch latency) on a
fixed configuration; thresholds chosen so a 2× regression fails the
test.

### Phase 6 — Conformance expansion + final integration validation

Crate: `voom-conformance` (extended). Phase 1 shipped the bootstrap
conformance harness gating every subsequent worker; Phase 6 extends
it to the full architectural-spec contract surface: every operation
kind from the fixed vocabulary, every error category from the failure
taxonomy, cancellation, registration replay, capability mismatch,
worker re-registration after crash, and supervisor-side recovery from
each chaos scenario. The phase also runs the now-complete suite
across every Phase 3 / 4 / 5 binary together as a final integration
gate.

**Exit:** `cargo test -p voom-conformance` runs the full extended
contract suite against all eleven fakes plus chaos and benchmark, and
CI runs the suite as part of `just ci`. No worker binary may merge
without passing it.

## 3. Workspace & Crate Deltas

| Crate | Sprint 2 contents added |
|---|---|
| `voom-worker-protocol` | Phase 1. Wire types (`OperationRequest`, `OperationResponse`, `ProgressFrame`, `ProtocolError`, `WorkerCredentials`), version-negotiation handshake, NDJSON frame codec with framing invariants (§4.2), bearer-token + worker-identity validation, transport traits (`ClientHandle`, `ServerHandle`), one concrete HTTP/1.1 loopback transport. |
| `voom-conformance` | New crate, Phase 1 (bootstrap) + Phase 6 (full). Black-box protocol conformance harness that launches a worker binary over the public protocol only. No dependency on `voom-fake-support`. Ships one minimal `echo-worker` binary in Phase 1 to validate the harness against itself. |
| `voom-control-plane` | Phase 2. `LocalWorkerSupervisor` plus the new `ControlPlane` use-case methods it composes (e.g., `register_worker_process`, `dequeue_and_dispatch`, `record_progress`, `record_result`, `expire_stale_leases`). All durable mutations go through these use-cases per Sprint 1 §5.2. New `crates/voom-control-plane/tests/` integration suite covering supervisor lifecycle, chaos scenarios, and the benchmark harness. |
| `voom-scheduler` | Untouched in Sprint 2. Reserved for Sprint 4's multi-worker `WorkerSelector` trait. |
| `voom-fake-support` | New crate, Phase 3. Shared helpers for fake binaries (lease loop, scenario runner, progress emitter, result-envelope helpers). Consumed only by the eleven `fake-*` binaries — never by `chaos-worker`, `benchmark-worker`, `voom-conformance`, or `voom-control-plane`. |
| `voom-fakes` | New crate, Phases 3 / 4 / 5. Eleven `fake-*` binaries plus `chaos-worker` and `benchmark-worker`. The fake binaries depend on `voom-fake-support`; chaos and benchmark depend only on `voom-worker-protocol` so their behavior cannot ride on the shared helpers. |
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

### 4.1 Transport: in-process spawn + HTTP/1.1 loopback with bearer-token identity

Workers are real OS processes spawned by the supervisor on the same
host. The supervisor talks to them over HTTP/1.1 on `127.0.0.1` with a
per-worker ephemeral port. On spawn the supervisor generates a 32-byte
cryptographically random `worker_secret` and passes it to the child
through an env var (`VOOM_WORKER_SECRET`); the supervisor also assigns
a `worker_id` (Sprint 1 `WorkerId`) and a `worker_epoch: u64` (bumped
on every (re-)spawn of the same logical worker). The worker prints
its bound port to stdout on startup; the supervisor reads it before
issuing the first request.

Every request from the supervisor to the worker carries
`Authorization: Bearer <worker_secret>`, `X-Voom-Worker-Id`, and
`X-Voom-Worker-Epoch` headers. Every callback from the worker to the
supervisor (heartbeat, progress, result) carries the same three
fields. Either side rejects requests whose `worker_secret` does not
match the spawn pair, whose `worker_id` is not the supervisor's
current row, or whose `worker_epoch` is not the supervisor's current
epoch for that worker. A worker that has been retired (epoch bumped
past its value) is rejected with `WORKER_RETIRED` and the supervisor
records the call as a stale-worker event.

Negative tests cover wrong secret, wrong worker_id, stale epoch, and
calls after explicit retire. The model is the same one Sprint 4's
authenticated remote transport will use: TLS replaces loopback,
client-cert binding replaces the spawn-time secret, and the worker_id
+ epoch validation stays identical. No supervisor logic or test
changes when Sprint 4 swaps the transport.

The protocol crate's public API is structured so callers never
construct a raw `hyper::Client` — they go through `ClientHandle` and
`ServerHandle` — and Sprint 4 swaps in an authenticated transport
behind the same trait.

### 4.2 Progress stream: NDJSON with explicit framing invariants

The architectural spec offers NDJSON or SSE. Sprint 2 picks NDJSON
because it is trivially parseable by `serde_json::from_str`
line-by-line and agent-friendly (every frame is one JSON object). The
following framing invariants are part of the Phase 1 wire contract
and the Phase 1 conformance harness pins each one with a positive and
a negative test:

- **Frame identity.** Every frame includes `lease_id` (Sprint 1
  `LeaseId`) and a monotonic `seq: u64` starting at 0 and incrementing
  by 1 per frame on the same lease. Frames with `seq` lower than or
  equal to the supervisor's last-received `seq` for that lease are
  dropped as duplicates and not double-counted. Frames whose
  `lease_id` does not match the lease the supervisor opened the
  stream for are rejected and the stream is aborted.
- **Terminal frame.** Each lease's progress stream ends with exactly
  one terminal frame: `ProgressFrame::Result { ... }` or
  `ProgressFrame::Error { class, code, payload }`. After a terminal
  frame, any further frame on the same stream is a contract violation
  and the supervisor records `malformed_worker_result`.
- **Max frame size.** A single NDJSON line is rejected if it exceeds
  64 KiB. The supervisor closes the stream and records the worker as
  failed with `malformed_worker_result`. The 64 KiB ceiling is tuned
  so realistic result envelopes (synthetic ticket payloads in
  Sprint 2; real worker payloads in Sprint 5) fit comfortably while
  unbounded growth cannot wedge the supervisor's reader.
- **Stall timeout.** If no frame arrives within the lease's
  `progress_idle_deadline` (Sprint 1 lease config) and no heartbeat
  has arrived either, the supervisor closes the stream and treats
  the operation as `worker_timeout`. A worker that keeps heartbeating
  but emits no progress for the same window is also classified as
  `worker_timeout` — heartbeat liveness alone is not sufficient.
- **EOF and truncation.** A stream that closes before its terminal
  frame is `worker_crash` (mapped from EOF) or `malformed_worker_result`
  (mapped from truncated JSON). Both classifications are durable
  Sprint 1 `FailureClass` variants and both record the partial frame
  count in the failure event.
- **Out-of-order and gaps.** A frame whose `seq` is greater than
  `last_seq + 1` is recorded as `malformed_worker_result` and the
  stream is aborted. NDJSON does not retransmit; the contract is
  strict monotonic ordering.

SSE remains an option for a future sprint if a UI consumer needs
native event-id replay; NDJSON with these invariants is sufficient
for Sprint 2.

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

### 4.7 Conformance: independent harness, gates every subsequent phase

Provider conformance tests live in `voom-conformance` (separate
crate, no dependency on `voom-fake-support` or any individual fake).
The harness only knows how to launch a worker binary, drive it over
the public protocol, and assert the contract. Phase 1 ships the
bootstrap harness plus `echo-worker`; every Phase 3 / 4 / 5 worker
binary must pass the harness before its specific E2E tests are
accepted; Phase 6 extends the harness to the full contract surface
and runs every binary together.

The harness is a test crate, not a runtime gate. CI runs it as part
of `just ci`. The supervisor does not invoke it at runtime — the
runtime trusts the wire contract. A future sprint may add a runtime
self-check to `voom-cli worker verify`; that verb is out of scope for
Sprint 2.

## 5. Cross-cutting test discipline

Sprint 2 keeps the Sprint 1 test layout — sibling `*_test.rs` files for
unit tests under `src/`, integration tests under `crates/*/tests/`.
Every new wire type gets a round-trip serde test. Every framing
invariant from §4.2 gets a paired positive / negative test in the
Phase 1 conformance harness. Every supervisor state transition gets a
unit test that drives it directly without spawning a real process.

Three crates spawn child processes in their integration tests:

- `voom-conformance` — launches whichever worker binary is under test
  via the public protocol; the only consumer that talks to the wire
  contract without going through `voom-fake-support` or the
  supervisor.
- `voom-control-plane` — the supervisor's E2E tests; spawns
  `echo-worker` (Phase 2), the eleven fakes (Phase 3), `chaos-worker`
  (Phase 4), and `benchmark-worker` (Phase 5).
- `voom-fakes` / `voom-fake-support` — scenario unit tests on the
  shared helpers; do not spawn anything themselves.

All three reuse the existing in-memory SQLite test harness and the
same migration set.

Bearer-token + worker-identity negative tests are owned by
`voom-conformance` (the harness deliberately mutilates headers and
asserts the worker / supervisor rejects). The supervisor side gets
its own focused unit tests for stale-epoch and retired-worker
rejection — independent of any worker binary.

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
