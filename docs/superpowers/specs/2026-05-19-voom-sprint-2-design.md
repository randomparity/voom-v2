---
name: voom-sprint-2-design
description: Sprint 2 (Synthetic Provider Suite MVP) overview design for VOOM — versioned HTTP/JSON worker protocol, workflow-executor scheduler closeout, eleven fake providers, chaos worker, benchmark worker, and provider conformance tests. Decomposes the sprint into seven documented phases on `feat/sprint-2`, fixes cross-phase architectural decisions, and defers per-phase detail to the phase-level design docs.
status: proposed
date: 2026-05-19
sprint: 2
branch: feat/sprint-2
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-22-voom-sprint-2-closeout-acceptance-plan.md
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

The release-readiness mapping from these criteria to tests, commands,
and provider inventory is the Sprint 2 closeout acceptance plan:
`docs/superpowers/specs/2026-05-22-voom-sprint-2-closeout-acceptance-plan.md`.

"The real scheduler" in this sprint is the Sprint 1 lease-acquire /
heartbeat / release / expire lifecycle plus the Sprint 2
`WorkerSelector` capacity boundary. Phase 2 originally named the owning
loop `LocalWorkerSupervisor`; the implemented closeout path exposes that
dequeue → lease → dispatch → result behavior as the Phase 7
`WorkflowExecutor`, not as a standalone supervisor API. The Phase 7
executor is therefore the Sprint 2 exit-gate scheduler surface: it uses
durable tickets and leases, `SingleWorkerPerKindSelector`, process-backed
workers over `voom-worker-protocol`, heartbeat/progress watchdogs, and
durable terminal transitions. The broader supervisor outbox/incarnation
reconciliation surface described below remains design context for later
sprints. Full multi-worker scoring (capability + locality + cost) is
Sprint 4.

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

Sprint 2 is committed to `feat/sprint-2` as seven documented phases.
Phases 1-6 establish the protocol, scheduler boundary, fake workers,
chaos/benchmark workers, and conformance foundation. Phase 7 is the
closeout gate that proves those pieces through the implemented durable
`WorkflowExecutor` scheduler path. Each phase ships its own design doc,
plan doc when needed, implementation commits, an adversarial-review
round (up to three), and a `/simplify` pass before the next phase begins.
Each phase ends with `just ci` green at every commit and the existing
Sprint 1 tests still passing.

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
`ProtocolError`, `OperationKind`, `WorkerCredentials`, the
`{ClientHandle, ServerHandle}` traits, and a `low_level` module with
raw HTTP/NDJSON primitives (§4.7). Round-trip serde tests, NDJSON
frame parser tests, version-negotiation tests, bearer-token /
worker-identity negative tests, and the §4.2 framing-invariant tests
pass. `voom-conformance` runs the `echo-worker` through both the
typed and raw-wire contract suites green and exits non-zero on every
golden-byte mutation in the suite.

### Phase 2 — Local worker supervisor design surface

Implementation note: the following section is the original Phase 2
supervisor design target. The branch did not ship a public
`LocalWorkerSupervisor` type or `worker_incarnations`/dispatch-intent
outbox in Sprint 2. Instead, Sprint 2 closeout promoted the durable
scheduler behavior into the Phase 7 `WorkflowExecutor`; that executor is
the tested dequeue → lease → dispatch → result path for the sprint exit
criteria.

Crates: `voom-control-plane` (scheduler/supervisor use-cases) and
`voom-scheduler` (minimal `WorkerSelector` trait). The deferred
supervisor design also calls for `voom-store` worker-incarnation and
dispatch-intent repositories. The supervisor lives in
`voom-control-plane` because Sprint 1 already establishes
`voom-control-plane` as the sole layer that composes durable state
mutations with event writes inside one transaction (see Sprint 1 §5.2);
the implemented `WorkflowExecutor` keeps that invariant by routing
durable job, ticket, lease, and dependency mutations through
control-plane use cases. `voom-scheduler` ships a minimal
`WorkerSelector` trait + `SingleWorkerPerKindSelector` default impl so
the operation-to-worker routing has a typed boundary today; Sprint 4
will swap in multi-worker scoring behind the same trait without changing
workflow-executor test code.

The deferred standalone supervisor design (a) registers and supervises local worker processes
via Sprint 1's `WorkerRepo` plus the new `WorkerIncarnationRepo`
(§4.8), (b) dequeues ready tickets via Sprint 1's
`LeaseRepo::acquire_in_tx`, (c) selects exactly one worker through
`WorkerSelector` (rejecting ambiguous and zero-match cases with
typed failure classes), (d) dispatches operations to that worker
over the protocol from Phase 1, (e) consumes the progress stream
via the watchdog state machine (§4.9) and emits the corresponding
Sprint 1 events (`worker.heartbeat`, `ticket.progress`, etc.), and
(f) closes the lease on result or any of the three watchdog
deadlines (exit / heartbeat / progress) with the precedence-correct
`FailureClass`. Every durable mutation goes through a `ControlPlane`
use-case method that composes the repo `_in_tx` call with the
matching `EventRepo::append_in_tx` in a single transaction — the
supervisor never writes to a repo without going through this layer.

The implemented Sprint 2 exit gate uses the Phase 7 `WorkflowExecutor`
instead of this standalone supervisor API. Phase 1/6 conformance gates
the worker protocol and fake binaries directly; a later sprint that
reintroduces the standalone supervisor must add the corresponding
supervisor-side conformance run.

**Deferred standalone supervisor exit:** when this design surface is
implemented in a later sprint, end-to-end tests in
`crates/voom-control-plane/tests/` should cover:

- supervisor register → dequeue → dispatch → progress → result →
  release happy path against `echo-worker`;
- restart reconciliation (§4.8) reaping orphans across all four
  supervisor-crash points;
- watchdog precedence (§4.9) across the six paired scenarios;
- `WorkerSelector` rejecting zero matches and ambiguous matches
  with typed failure classes;
- conformance against `echo-worker` passes from the supervisor
  side, both typed and raw-wire.

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
implemented `WorkflowExecutor` scheduler path, every fake passes the
conformance harness, and the durable event log matches the scripted
scenario.

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
reports per-operation latency + throughput in structured progress frames
the test harness collects. The full supervisor-throughput gate from the
original plan was superseded in Sprint 2 closeout by Phase 7's durable
workflow throughput summary, which measures dispatch throughput through
the implemented `WorkflowExecutor` scheduler path.

`benchmark-worker` must pass the conformance harness before being
admitted to the throughput suite.

**Exit:** `voom-control-plane/tests/benchmark.rs` records baseline
numbers (operations per second, p50 / p95 dispatch latency) on a
fixed configuration and validates positive throughput plus generous
sanity ceilings. Hard machine-calibrated regression thresholds are
deferred until the full supervisor benchmark has enough baseline data.

### Phase 6 — Conformance expansion + final integration validation

Crate: `voom-conformance` (extended). Phase 1 shipped the bootstrap
conformance harness gating every subsequent worker; Phase 6 extends
it to the full architectural-spec contract surface: every operation
kind from the fixed vocabulary, every error category from the failure
taxonomy, cancellation, registration replay, capability mismatch, and
worker re-registration after crash. Workflow-level chaos recovery is
covered by the Phase 7 `WorkflowExecutor` integration tests; standalone
supervisor recovery remains deferred with §4.8. The phase also runs the
now-complete suite across every Phase 3 / 4 / 5 binary together as a
final integration gate.

**Exit:** `cargo test -p voom-conformance` runs the full extended
contract suite against all eleven fakes plus chaos and benchmark, and
CI runs the suite as part of `just ci`. No worker binary may merge
without passing it.

### Phase 7 — Durable simulated workflow closeout

Crate: `voom-control-plane` (extended). Phase 7 is the Sprint 2
acceptance gate: it runs the default synthetic media workflow through
durable jobs, tickets, leases, `SingleWorkerPerKindSelector`,
process-backed worker protocol dispatch, progress/heartbeat watchdogs,
dependency promotion, terminal state, chaos classification, and
scheduler-throughput reporting.

**Exit:** the Sprint 2 closeout acceptance matrix passes. In practice
that means the Phase 6 conformance/fake-provider prerequisite is green,
`voom-control-plane` durable workflow tests prove the happy path and
chaos cases through `WorkflowExecutor`, the benchmark path reports
non-zero scheduler throughput, and `just ci` passes.

## 3. Workspace & Crate Deltas

| Crate | Sprint 2 contents added |
|---|---|
| `voom-worker-protocol` | Phase 1. Wire types (`OperationRequest`, `OperationResponse`, `ProgressFrame`, `ProtocolError`, `WorkerCredentials`), version-negotiation handshake, NDJSON frame codec with framing invariants (§4.2), bearer-token + worker-identity validation, transport traits (`ClientHandle`, `ServerHandle`), one concrete HTTP/1.1 loopback transport. Public typed-encode API plus a `low_level` module exposing raw HTTP / NDJSON primitives so `voom-conformance` and `chaos-worker` can construct malformed wire bytes outside the typed encoder (§4.7). |
| `voom-conformance` | New crate, Phase 1 (bootstrap) + Phase 6 (full). Black-box protocol conformance harness that launches a worker binary over the public protocol only. No dependency on `voom-fake-support`. Ships one minimal `echo-worker` binary in Phase 1 to validate the harness against itself. The Phase 1 harness includes a **raw-wire mutation suite**: golden-byte HTTP/NDJSON fixtures plus mutations that bypass `ClientHandle`/`ServerHandle` and assert the worker rejects malformed bytes (§4.7). |
| `voom-control-plane` | Phase 2/7. The original Phase 2 design named a standalone `LocalWorkerSupervisor`; the implemented Sprint 2 scheduler surface is Phase 7's `WorkflowExecutor`. It composes `ControlPlane` use cases for job/ticket creation, worker selection, lease acquire/release/fail, dependency promotion, heartbeat/progress watchdogs, chaos mapping, and durable workflow summaries. The separate worker-incarnation outbox and public supervisor API remain later-sprint work. |
| `voom-scheduler` | Phase 2 ships a minimal `WorkerSelector` trait plus a `SingleWorkerPerKindSelector` default impl: select exactly one active worker advertising the requested `OperationKind`; return `FailureClass::NoEligibleWorker` for zero matches; return `FailureClass::AmbiguousWorkerSelection` (new variant — added to `voom-core::failure` in the same phase) for multiple matches unless an explicit override is set. Sprint 4 swaps in multi-worker scoring behind the same trait without changing scheduler callers. |
| `voom-store` | Sprint 2 reuses the existing durable jobs, tickets, leases, workers, capabilities, and grants tables for the implemented `WorkflowExecutor` path. The originally proposed `0005_worker_incarnations.sql`, `lease_dispatch_intents`, and `WorkerIncarnationRepo` are deferred with the standalone supervisor/outbox work in §4.8. |
| `voom-fake-support` | New crate, Phase 3. Shared helpers for fake binaries (lease loop, scenario runner, progress emitter, result-envelope helpers). Consumed only by the eleven `fake-*` binaries — never by `chaos-worker`, `benchmark-worker`, `voom-conformance`, or `voom-control-plane`. |
| `voom-fakes` | New crate, Phases 3 / 4 / 5. Eleven `fake-*` binaries plus `chaos-worker` and `benchmark-worker`. The fake binaries depend on `voom-fake-support`; chaos and benchmark depend only on `voom-worker-protocol::low_level` so their malformed-frame behavior cannot ride on the shared typed encoder. |
| `voom-core` | Phase 1 adds a `protocol_version` constant plus error-code variants for `WORKER_RETIRED`, `WORKER_INCARNATION_STALE`, `AMBIGUOUS_WORKER_SELECTION`. Phase 2 adds `FailureClass::ProgressTimeout` (distinct from `WorkerTimeout` for callbacks-but-no-progress) and `FailureClass::AmbiguousWorkerSelection`. |
| `voom-cli` | No implemented Sprint 2 closeout dependency. A later standalone supervisor sprint may add read-only inspection verbs over progress events, supervisor state, and `worker_incarnations` if Sprint 1's existing verbs are insufficient. Read-only only. |
| `voom-api`, `voom-events`, `voom-policy`, `voom-plan`, `voom-artifact` | Untouched. No Sprint 2 deliverables land here. |

`voom-events` is deliberately not touched. Sprint 1 already defined the
relevant `EventKind` variants (`worker.registered`, `lease.acquired`,
`lease.heartbeat`, `lease.released`, `ticket.progress`,
`ticket.failed`, etc.), and Sprint 2's implemented `WorkflowExecutor`
closeout path records durable job, ticket, lease, progress, and failure
state through the existing control-plane/store APIs without inventing
new event kinds. A later standalone supervisor may wire its process and
callback lifecycle into those same variants; if that work truly needs a
new event kind, the delta belongs in that later phase's plan with an
explicit note in its per-phase design.

## 4. Cross-phase architectural decisions

These decisions are fixed here so each per-phase design starts from a
shared baseline.

### 4.1 Transport: process-backed HTTP/1.1 loopback with bearer-token identity

For the implemented Sprint 2 closeout path, fake providers, chaos, and
benchmark workers are real OS processes spawned by the Phase 7 test
fixture and registered with the `WorkflowExecutor` runtime registry. The
executor dispatches to them over HTTP/1.1 on `127.0.0.1` with
per-worker ephemeral endpoints and bearer-token credentials carried by
the worker protocol. This keeps Sprint 2 on the real wire contract while
avoiding an unshipped public supervisor daemon API.

The deferred standalone supervisor design uses the same protocol shape
but owns the process lifecycle itself: on spawn it generates a 32-byte
cryptographically random `worker_secret`, passes it to the child through
an env var (`VOOM_WORKER_SECRET`), assigns a `worker_id` (Sprint 1
`WorkerId`) and a `worker_epoch: u64`, reads the child's bound port from
stdout, and then issues requests.

Every scheduler-to-worker request carries `Authorization: Bearer
<worker_secret>`, `X-Voom-Worker-Id`, and `X-Voom-Worker-Epoch` headers.
Deferred worker-to-supervisor callbacks would carry the same three
fields. Either side rejects requests whose `worker_secret` does not
match the active credential, whose `worker_id` is not the expected
worker row, or whose `worker_epoch` is stale for that worker. A worker
that has been retired is rejected with `WORKER_RETIRED`; the deferred
supervisor records that call as a stale-worker event.

Negative tests cover wrong secret, wrong worker_id, stale epoch, and
calls after explicit retire at the protocol/conformance boundary. The
model is the same one Sprint 4's authenticated remote transport will
use: TLS replaces loopback, client-cert binding replaces the spawn-time
secret, and the worker_id + epoch validation stays identical. Scheduler
call sites and protocol tests should not change when Sprint 4 swaps the
transport.

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
  equal to the scheduler's last-received `seq` for that lease are
  dropped as duplicates and not double-counted. Frames whose
  `lease_id` does not match the lease the scheduler opened the
  stream for are rejected and the stream is aborted.
- **Terminal frame.** Each lease's progress stream ends with exactly
  one terminal frame: `ProgressFrame::Result { ... }` or
  `ProgressFrame::Error { class, code, payload }`. After a terminal
  frame, any further frame on the same stream is a contract violation
  and the scheduler records `malformed_worker_result`.
- **Max frame size.** A single NDJSON line is rejected if it exceeds
  64 KiB. The scheduler closes the stream and records the worker as
  failed with `malformed_worker_result`. The 64 KiB ceiling is tuned
  so realistic result envelopes (synthetic ticket payloads in
  Sprint 2; real worker payloads in Sprint 5) fit comfortably while
  unbounded growth cannot wedge the scheduler's reader.
- **Stall timeout.** Heartbeat liveness and progress liveness are
  evaluated independently by the watchdog (§4.9). A worker that
  keeps heartbeating but emits no progress for `progress_idle_deadline`
  is classified `FailureClass::ProgressTimeout` (new variant; distinct
  from `WorkerTimeout`/`WorkerCrash`). A worker that misses heartbeats
  but the stream remains open is classified `FailureClass::WorkerTimeout`.
  A worker whose process exit is observed before either deadline is
  `FailureClass::WorkerCrash`. The watchdog state machine pins
  precedence and idempotency when two or more deadlines / a terminal
  result race — see §4.9 for the full table.
- **EOF and truncation.** A stream that closes before its terminal
  frame is `worker_crash` (mapped from EOF on a healthy connection or
  via the process-exit watcher) or `malformed_worker_result` (mapped
  from truncated JSON inside an otherwise valid frame). Both
  classifications record the partial frame count in the failure event.
- **Out-of-order and gaps.** A frame whose `seq` is greater than
  `last_seq + 1` is recorded as `malformed_worker_result` and the
  stream is aborted. NDJSON does not retransmit; the contract is
  strict monotonic ordering.

SSE remains an option for a future sprint if a UI consumer needs
native event-id replay; NDJSON with these invariants is sufficient
for Sprint 2.

### 4.3 Dispatch direction: scheduler pulls, worker accepts

The implemented Sprint 2 scheduler path initiates every operation
request through `WorkflowExecutor`: it dequeues a ready ticket, acquires
a durable lease, selects one registered worker with
`SingleWorkerPerKindSelector`, and dispatches `POST /v1/operations` to
that worker's HTTP endpoint. Workers do not poll the control plane. This
gives the scheduler full control over backpressure, cancellation, and
per-worker concurrency, and matches the eventual Sprint 4 model
(scheduler dispatches; worker accepts).

For Phase 7, heartbeat/progress liveness is observed from the worker
response stream and executor-owned watchdogs. The executor refreshes
lease heartbeat/progress state through the Sprint 1 control-plane/store
APIs and owns the timeout decision and failure-class mapping. The
deferred standalone supervisor callback form may add worker →
supervisor `POST /v1/leases/{id}/heartbeat` calls and an HTTP callback
server; that is not required for the Sprint 2 closeout gate.

### 4.4 Worker lifecycle: process-backed fake workers, one process per fake binary

The implemented closeout tests spawn each fake worker as its own OS
process in the Phase 7 test fixture, register the process-backed runtime
with `WorkflowExecutor`, and cleanly shut the provider down at the end
of each run. A worker that exits unexpectedly is surfaced through the
worker protocol stream and mapped by the executor's watchdog path to the
existing Sprint 1 lease-failure path. There is no in-process worker fast
path; the architectural spec forbids it (ADR-0002).

The deferred standalone supervisor owns the `tokio::process::Child`
handles itself, detects process exit with a child watcher plus heartbeat
timeout, and records crashes through the same lease-failure semantics.

### 4.5 Capability advertisement: at registration time, durable in `worker_capabilities`

Workers advertise capabilities (operation kinds, codecs, hardware) in
the registration payload. The implemented closeout path stores runtime
worker registrations through the existing Sprint 1 worker/capability
repositories and uses those durable rows for scheduler selection. No new
schema in Sprint 2. The deferred standalone supervisor uses the same
repository path when that API lands.

### 4.6 Determinism for synthetic providers

Every fake is deterministic given a `(scenario_path, seed)` pair. The
seed is reused across runs in CI. Tests assert exact event sequences,
exact result envelopes, and (for `chaos-worker` and
`benchmark-worker`) exact failure mode selection. Non-determinism is a
test bug, not a feature.

### 4.7 Conformance: independent harness, raw-wire path, gates every subsequent phase

Provider conformance tests live in `voom-conformance` (separate
crate, no dependency on `voom-fake-support` or any individual fake).
The harness only knows how to launch a worker binary, drive it over
the public protocol, and assert the contract. Phase 1 ships the
bootstrap harness plus `echo-worker`; every Phase 3 / 4 / 5 worker
binary must pass the harness before its specific E2E tests are
accepted; Phase 6 extends the harness to the full contract surface
and runs every binary together.

The harness has **two layers** so a bug in the typed encoder cannot
be hidden by the typed decoder agreeing with it:

- **Typed layer.** Uses `ClientHandle`/`ServerHandle` from
  `voom-worker-protocol`, encodes via the typed API, asserts decoded
  values match. This catches contract semantics.
- **Raw-wire layer.** Uses `voom-worker-protocol::low_level` (raw
  HTTP / NDJSON primitives that bypass typed encode) to build byte
  fixtures by hand. Golden-byte fixtures pin the on-wire shape of
  each handshake, operation request, progress frame, and result
  envelope. Mutation tests (`tamper_with_seq`, `truncate_at_byte`,
  `flip_one_byte`, `wrong_content_length`, `oversize_frame`,
  `wrong_bearer`, `wrong_worker_id`, `stale_epoch`, etc.) assert the
  receiver rejects. This catches codec drift, auth-bypass bugs, and
  buffer-handling regressions that a typed-only suite would miss.

`chaos-worker` (§Phase 4) uses the same `low_level` API to emit
deliberately malformed frames during its faulting operations — it
must not be possible to fault below the type layer by going through
the typed encoder, because the typed encoder is what the conformance
suite is trying to falsify.

The harness is a test crate, not a runtime gate. CI runs it as part
of `just ci`. The scheduler/executor does not invoke it at runtime —
the runtime trusts the wire contract. A future sprint may add a runtime
self-check to `voom-cli worker verify`; that verb is out of scope for
Sprint 2.

### 4.8 Deferred worker incarnation persistence, dispatch outbox, and restart reconciliation

This section is later-sprint design context for the standalone
`LocalWorkerSupervisor`/outbox surface. It did not ship in Sprint 2 and
is not part of the Phase 7 `WorkflowExecutor` closeout gate. Sprint 2's
implemented scheduler path uses durable ticket/lease rows plus
process-backed runtime registrations held by the Phase 7 test fixture;
it does not persist worker incarnations or lease dispatch intents.

Bearer-token identity (§4.1) is per-spawn. The HTTP dispatch is a
side effect outside any DB transaction, so the durable state needs a
**two-step outbox pattern** that survives every supervisor-crash
point. The deferred supervisor design introduces a migration like:

```sql
CREATE TABLE worker_incarnations (
    incarnation_id    INTEGER PRIMARY KEY,
    worker_id         INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    epoch             INTEGER NOT NULL,
    state             TEXT NOT NULL,            -- 'spawning' | 'live' | 'retired'
    pid               INTEGER NOT NULL,
    pgid              INTEGER NOT NULL,
    endpoint          TEXT,                     -- NULL until state = 'live'
    secret_hash       TEXT NOT NULL,            -- argon2id(secret); plaintext never persists
    binary_path       TEXT NOT NULL,            -- absolute path; verified before kill
    process_birth_id  TEXT NOT NULL,            -- portable process identity proof (see below)
    started_at        TEXT NOT NULL,
    retired_at        TEXT,
    retire_reason     TEXT,                     -- 'graceful' | 'orphan_reaped' | 'crash_detected' | 'epoch_bumped' | 'kill_skipped_identity_mismatch'
    UNIQUE(worker_id, epoch),
    CHECK (state IN ('spawning', 'live', 'retired')),
    CHECK ((state = 'live') = (endpoint IS NOT NULL)),
    CHECK ((state = 'retired') = (retired_at IS NOT NULL))
) STRICT;

CREATE TABLE lease_dispatch_intents (
    intent_id        INTEGER PRIMARY KEY,
    lease_id         INTEGER NOT NULL REFERENCES leases(id) ON DELETE RESTRICT,
    incarnation_id   INTEGER NOT NULL REFERENCES worker_incarnations(incarnation_id),
    idempotency_key  TEXT NOT NULL,             -- ULID; presented as X-Voom-Idempotency-Key
    state            TEXT NOT NULL,             -- 'pending' | 'dispatched' | 'completed' | 'failed' | 'abandoned'
    created_at       TEXT NOT NULL,
    dispatched_at    TEXT,
    completed_at     TEXT,
    UNIQUE(lease_id, incarnation_id),
    UNIQUE(idempotency_key),
    CHECK (state IN ('pending', 'dispatched', 'completed', 'failed', 'abandoned'))
) STRICT;
```

The supervisor's dispatch flow is a strict outbox sequence —
**HTTP dispatch never happens inside a DB transaction**:

1. **Pre-spawn commit.** INSERT `worker_incarnations` row with
   `state = 'spawning'`, pid = 0 placeholder. Commit. (Reserves the
   epoch durably so two supervisors cannot reuse it.)
2. **Spawn.** `tokio::process::Command::spawn`, capture child handle,
   wait for stdout port line, compute `process_birth_id`.
3. **Live commit.** UPDATE the row to `state = 'live'` with `pid`,
   `pgid`, `endpoint`, `binary_path`, `process_birth_id`. Commit.
4. **Intent commit.** INSERT `lease_dispatch_intents` row with
   `state = 'pending'` and a fresh ULID `idempotency_key`. Commit.
5. **Dispatch.** HTTP POST to the worker's endpoint carrying the
   `idempotency_key` header. The worker MUST reject a duplicate
   `idempotency_key` for the same lease/incarnation pair — this
   makes the dispatch retryable from the supervisor side without
   risking double execution.
6. **Dispatched commit.** On the HTTP request's first byte response,
   UPDATE the intent to `state = 'dispatched'`. (If the supervisor
   crashes between step 5 and step 6, the worker may have started
   work but the intent still says `pending`. Restart reconciliation
   handles this — see below.)
7. **Result commit.** When the watchdog accepts a terminal result,
   UPDATE the intent to `state = 'completed'` (or `'failed'`) in the
   same transaction that closes the lease.

**Restart reconciliation** runs as the first step of
`LocalWorkerSupervisor::start`:

1. Open `BEGIN IMMEDIATE`.
2. `SELECT * FROM worker_incarnations WHERE state != 'retired'`.
3. For each non-retired incarnation, verify process identity by
   inspecting the OS:
   - On macOS: `libproc::proc_pidpath(pid)` plus
     `proc_pidinfo(PROC_PIDBSDINFO)` start time.
   - On Linux: `/proc/<pid>/exe` symlink plus
     `/proc/<pid>/stat` start time (field 22 — clock ticks since
     boot).
   - Both compared against the stored `binary_path` and
     `process_birth_id`. Mismatch (including pid no longer exists)
     means the original process is gone; signal-then-kill is
     **skipped** and the row is retired with
     `retire_reason = 'kill_skipped_identity_mismatch'`. The
     identity check is the load-bearing invariant that prevents
     `kill(-pgid)` from signaling an unrelated process group after
     PID/PGID reuse.
   - Match means the original process is still alive: send
     `kill(-pgid, SIGTERM)`, wait a short grace period, send
     `kill(-pgid, SIGKILL)`. Retire the row with
     `retire_reason = 'orphan_reaped'`.
4. For every non-terminal lease whose `lease_dispatch_intents` row
   references a retired incarnation, decide based on intent state:
   - `pending` → no HTTP request was confirmed; mark intent
     `abandoned`, mark lease `ready` again with attempt count
     bumped. No `FailureClass` is recorded because no work was
     observed; the next dispatch is a clean retry against a fresh
     incarnation.
   - `dispatched` → work may have run; record
     `FailureClass::WorkerCrash` (treat as failure for retry-policy
     purposes) and mark intent `failed`.
   - `completed` / `failed` → terminal already; no action.
5. Commit.

Idempotency keys + the worker-side dedupe make retries from `pending`
safe even if the original HTTP dispatch did reach the worker — the
worker recognizes the duplicate key and either no-ops or returns the
cached prior result.

The parent-death belt-and-suspenders is part of the deferred supervisor
design for long-running operations: every spawned worker inherits a
parent-death watchdog
implemented via a stdin pipe held open by the supervisor. The worker
reads stdin in a background task; when the supervisor exits, the
pipe closes, the read returns EOF, and the worker performs its own
graceful shutdown (cancel in-flight operation, exit). This means
even if startup reconciliation cannot prove process identity (the
edge case where the orphan exited and a new process happens to reuse
the PID + binary path), the original orphan has already exited
voluntarily because its supervisor parent did. The stdin-pipe
mechanism is portable across macOS and Linux without platform
`#[cfg]`.

The deferred supervisor implementation should ship disk-backed restart
tests:

- supervisor crashes between step 1 and step 2 (no `live` row);
- supervisor crashes between step 3 and step 4 (live row, no
  intent);
- supervisor crashes between step 4 and step 5 (intent pending, no
  HTTP issued);
- supervisor crashes after dispatch but before step 6 (intent
  pending, but HTTP did reach worker — idempotency-key test);
- supervisor crashes mid-progress-stream (intent dispatched);
- supervisor crashes after the result was written but before lease
  release event (intent dispatched, result already in DB);
- supervisor crashes during reconciliation itself (re-run is
  idempotent);
- PID-reuse case: orphan exits, unrelated process is given the same
  pid before reconciliation runs; identity check refuses to signal,
  row retired as `kill_skipped_identity_mismatch`.

### 4.9 Implemented workflow watchdog state machine and deferred supervisor arbiter

Sprint 2 Phase 7 implements the watchdog semantics in
`WorkflowExecutor` dispatch tasks: terminal frames, malformed frames,
stream end/crash, heartbeat timeout, progress timeout, and dispatch
timeout are mapped into durable lease/ticket state with the precedence
covered by `durable_workflow` and workflow executor tests. The
single-arbiter/process-exit channel design below remains the deferred
standalone supervisor form for a later sprint.

The supervisor's per-lease watchdog tracks three independent
deadlines and an exit observer:

| Signal | Source | Maps to |
|---|---|---|
| Process exit observed | `tokio::process::Child::wait` | `WorkerCrash` |
| Last heartbeat older than `heartbeat_deadline` | `LeaseRepo::heartbeat_in_tx` plus an in-memory timer | `WorkerTimeout` |
| Last progress frame older than `progress_idle_deadline` | NDJSON reader plus an in-memory timer | `ProgressTimeout` (new `FailureClass` variant) |
| Terminal `ProgressFrame::Result` / `ProgressFrame::Error` | NDJSON reader | `Succeeded` or worker-supplied failure class |

The watchdog runs as a **single arbiter task** per lease — the
NDJSON stream reader and the process-exit observer feed the arbiter
through one `tokio::sync::mpsc` channel. This guarantees ordering:
exit-observed cannot preempt a terminal frame that was emitted
earlier on the wire.

The arbiter evaluates events in this strict precedence, and only one
terminal state ever wins per lease:

1. If a terminal result has been accepted, ignore later signals
   (lease is already `succeeded` / `failed`).
2. Else if a terminal `ProgressFrame::Result` / `ProgressFrame::Error`
   has been received and parses cleanly, accept it (even if a
   process-exit observation is queued behind it). Terminal frames
   always take precedence over a subsequent exit observation,
   provided they were emitted before the exit on the same FIFO.
3. Else if process exit has been observed AND the stream reader has
   drained all remaining bytes from the connection through EOF, and
   no complete terminal frame is buffered, classify `WorkerCrash`
   and fail the lease. The arbiter MUST NOT classify `WorkerCrash`
   on exit observation alone — it MUST drain first.
4. Else if heartbeat deadline has passed, classify `WorkerTimeout`
   and fail the lease.
5. Else if progress idle deadline has passed, classify
   `ProgressTimeout` and fail the lease.
6. Otherwise, keep waiting.

In the deferred supervisor form, every classification calls a single `ControlPlane` use-case
(`fail_lease_with_class`) which composes
`LeaseRepo::fail_lease_in_tx` plus the matching event in one
transaction. The use-case is idempotent on lease state: if the lease
has already transitioned (e.g. a terminal result raced with a
heartbeat-deadline miss), the second call returns
`AlreadyTerminal { existing_class }` and the watchdog records the
race as a non-fatal audit event without overwriting.

For the deferred supervisor, `LeaseRepo::expire_due` is the safety-net
for cases where the supervisor itself has lost track of a lease (crash before
incarnation row was written, or watchdog deadlocked). It picks up
expiry-time-passed leases the watchdog did not handle and assigns
`WorkerCrash` since the original failure class is unknowable from
durable state alone. Deferred supervisor work should widen the
`expire_due` test matrix for supervisor-owned leases and prove this
safety-net does not double-fail leases the watchdog already terminated.

The implemented Phase 7 precedence table is pinned by
`crates/voom-control-plane/src/workflow/executor_test.rs` and
`crates/voom-control-plane/tests/durable_workflow.rs`. A later standalone
supervisor should add paired tests in `crates/voom-control-plane/tests/watchdog/`:

- heartbeat-only, no progress, no exit → `WorkerTimeout`
- progress-only, no heartbeat, no exit → `ProgressTimeout`
- heartbeat-and-progress, sudden exit → `WorkerCrash` (overrides
  not-yet-fired deadlines, only after drain confirms no buffered
  terminal frame)
- terminal result at deadline boundary → `Succeeded` (race won by
  result; deadline miss observed and recorded as audit only)
- **terminal result flushed, process exits, supervisor accepts after
  observing exit** → `Succeeded` (drain-before-classify guarantees
  the wire-emitted result wins; this is the load-bearing test for
  the race round 3 surfaced)
- two deadlines fire simultaneously → exit > heartbeat > progress
  precedence (after drain)
- watchdog terminated lease and `expire_due` fires later → no
  double-fail; safety-net is a no-op on terminal leases

## 5. Cross-cutting test discipline

Sprint 2 keeps the Sprint 1 test layout — sibling `*_test.rs` files for
unit tests under `src/`, integration tests under `crates/*/tests/`.
Every new wire type gets a round-trip serde test. Every framing
invariant from §4.2 gets a paired positive / negative test in the
Phase 1 conformance harness. Every implemented workflow/scheduler state
transition gets a unit test that drives it directly without spawning a
real process; deferred standalone supervisor transitions should follow
the same rule when that surface lands.

Three crates spawn child processes in their integration tests:

- `voom-conformance` — launches whichever worker binary is under test
  via the public protocol; the only consumer that talks to the wire
  contract without going through `voom-fake-support` or the
  supervisor.
- `voom-control-plane` — workflow/scheduler E2E tests; spawns the
  process-backed fake providers, `chaos-worker`, and benchmark-related
  workers used by the implemented closeout path. Deferred standalone
  supervisor E2E tests should use the same process-cleanup discipline.
- `voom-fakes` / `voom-fake-support` — scenario unit tests on the
  shared helpers; do not spawn anything themselves.

All three reuse the existing in-memory SQLite test harness and the
same migration set.

Bearer-token + worker-identity negative tests are owned by
`voom-conformance` (the harness deliberately mutilates headers and
asserts the worker rejects). Deferred supervisor-side identity tests
should cover stale-epoch and retired-worker rejection independent of any
worker binary when that API lands.

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
