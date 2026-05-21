---
name: voom-sprint-2-phase-5-benchmark-worker-design
description: Sprint 2 Phase 5 follow-up design — promote benchmark-worker from scaffold to active conformance target and implement deterministic worker-level benchmark measurement before supervisor throughput gates.
status: proposed
date: 2026-05-21
sprint: 2
phase: 5
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
parent_sections: §2 Phase 5 benchmark worker; §4.6 determinism for synthetic providers; §4.7 conformance
predecessor_spec: docs/superpowers/specs/2026-05-21-voom-sprint-2-phases-4-5-6-conformance-fill-in-design.md
scope: benchmark-worker contract implementation, conformance promotion, and schema-stable worker-level benchmark frames; supervisor throughput thresholds remain deferred
---

# Sprint 2 Phase 5 — Benchmark Worker Contract Implementation

## 1. Goal

The previous Phase 4/5/6 slice put `benchmark-worker` on disk as a
scaffold, and the conformance fill-in made active-worker admission
real. This slice promotes `benchmark-worker` from scaffold to active
by implementing a real protocol worker with bounded, payload-driven
benchmark behavior.

The implementation target is the worker contract boundary:

1. `benchmark-worker` starts like any Sprint 2 local worker.
2. Its non-benchmark baseline operation passes the active conformance
   Tier 1 suite.
3. Its benchmark operation emits stable progress and result metrics
   that later supervisor benchmark tests can collect.

Supervisor throughput gates remain a follow-up. This keeps the metric
emitter stable before `voom-control-plane/tests/benchmark.rs` chooses
CI thresholds for operations per second and dispatch latency.

## 2. Scope

In scope:

- Replace `crates/voom-fakes/src/bin/benchmark_worker.rs` placeholder
  behavior with a real loopback HTTP worker.
- Keep `benchmark-worker` independent from `voom-fake-support`.
- Select benchmark behavior through `OperationRequest.payload.mode`.
- Default missing `mode` to `baseline` so generic conformance requests
  continue to work.
- Promote `benchmark-worker` to an active required entry in
  `crates/voom-conformance/voom-fakes-manifest.toml`.
- Add worker-level tests for startup, baseline behavior, benchmark
  progress/result schema, idempotency replay, and invalid payload
  rejection.

Out of scope:

- Supervisor benchmark assertions under
  `voom-control-plane/tests/benchmark.rs`.
- CI throughput floors, p50 latency thresholds, or p95 latency
  thresholds.
- Phase 3 fake-provider completion.
- Raw HTTP envelope corruption or fault injection. Those belong to
  conformance negative fixtures and `chaos-worker`.

Exit criteria:

- `benchmark-worker` is no longer listed under `[scaffold].binaries`.
- `cargo test -p voom-conformance --all-features` launches
  `benchmark-worker`, `chaos-worker`, and `echo-worker` as active
  workers.
- `benchmark-worker` passes typed and raw-wire Tier 1 conformance
  through its default baseline behavior.
- Direct worker tests prove benchmark mode emits schema-stable metric
  frames with deterministic cadence and totals, without asserting
  machine-specific performance numbers.

## 3. Architecture

`benchmark-worker` stays in
`crates/voom-fakes/src/bin/benchmark_worker.rs`. It depends on
`voom-worker-protocol` directly and does not use `voom-fake-support`,
because the benchmark worker must measure the worker protocol boundary
without inheriting behavior from the shared fake-provider helper path.

Unlike `chaos-worker`, `benchmark-worker` does not need deliberately
open response bodies or malformed streams. Every valid request returns
promptly with a complete ordered progress stream. The implementation
may therefore use `voom-worker-protocol::HttpServer` if its handler
shape fits the required frame construction. If the existing server API
does not expose enough control for idempotency and frame payloads, the
implementation may use the same small local HTTP shim pattern as
`chaos-worker`.

The implementation plan should prefer a small private shared helper
inside `voom-fakes` if that reduces duplication between
`chaos-worker` and `benchmark-worker` without widening public API.
The helper boundary must stay internal to `voom-fakes`; this slice
does not add new `voom-worker-protocol` public API. If extracting that
helper risks obscuring the worker behavior or expanding the task, the
benchmark worker may initially mirror the minimal local shim and leave
deduplication for the simplification pass.

The binary has three internal responsibilities:

| Part | Responsibility |
|---|---|
| Bootstrap | Parse `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`, `VOOM_WORKER_EPOCH`, and `VOOM_WORKER_BIND`; start the local HTTP server; print `BOUND addr=...`; shut down on stdin EOF. |
| Payload parser | Decode `payload.path`, `payload.mode`, `payload.operations`, and `payload.emit_every` into a typed internal benchmark config. Reject unknown modes and malformed fields before work starts. |
| Operation runner | Convert a valid `OperationRequest` into baseline frames or schema-stable benchmark progress and result frames. |

The startup contract mirrors the other Sprint 2 local workers:

- required env: `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`,
  `VOOM_WORKER_EPOCH`;
- optional env: `VOOM_WORKER_BIND`, defaulting to `127.0.0.1:0`;
- readiness signal: one stdout line, `BOUND addr=<actual>`;
- parent-death behavior: stdin EOF triggers graceful server shutdown.

The worker supports `OperationKind::ProbeFile` for conformance and for
all direct benchmark tests in this slice. Other operation kinds return
`ProtocolError::UnknownOperation`. This avoids widening the operation
surface before supervisor benchmark tests need it.

### 3.1 Cross-package binary resolution

`echo-worker` is a binary target in `voom-conformance`, so Cargo makes
`CARGO_BIN_EXE_echo-worker` available to that package's integration
tests. `benchmark-worker` lives in the separate `voom-fakes` package,
so `cargo test -p voom-conformance` cannot rely on
`CARGO_BIN_EXE_benchmark-worker` being set.

The conformance fill-in and Phase 4 work already establish the
cross-package resolution rule used for `chaos-worker`: an explicit
manifest path wins, otherwise the conformance integration test builds
the `voom-fakes` binary and resolves it from Cargo's shared target
directory. Promotion for `benchmark-worker` must use the same
mechanism. Missing `benchmark-worker` is a hard failure once the
manifest marks it active.

## 4. Payload Contract

Benchmark behavior is selected by the operation payload, not by
headers. Headers remain reserved for protocol versioning, worker
identity, auth, and idempotency.

Baseline payload shape:

```json
{
  "path": "/library/example.mkv"
}
```

Benchmark payload shape:

```json
{
  "path": "/library/example.mkv",
  "mode": "benchmark",
  "operations": 100,
  "emit_every": 10
}
```

`path` is required for every `ProbeFile` request, including
conformance baseline requests. Missing or non-string `path` is
`ProtocolError::InvalidPayload`; it must not fall through to
`baseline`. Missing `mode` means `baseline` only after the normal
`ProbeFile` payload contract has been validated.

Mode names:

| Mode | Behavior |
|---|---|
| `baseline` | Emit `Progress(seq=0)` and `Result(seq=1)` with a small result payload echoing the mode and path. |
| `benchmark` | Run a deterministic no-op loop for `operations`, emit benchmark progress frames every `emit_every` completions, and emit a terminal result with the final metric summary. |

Benchmark config:

- `operations` is required for `benchmark` mode and must be within
  `1..=10_000`.
- `emit_every` is optional for `benchmark` mode. Missing
  `emit_every` defaults to `operations`, producing one progress frame
  before the terminal result.
- `emit_every` must be within `1..=operations`.
- Unknown `mode`, wrong field types, missing `path`, missing
  `operations` for benchmark mode, excessive operation counts, and
  unsupported combinations return `ProtocolError::InvalidPayload`.

## 5. Metric Frames

Benchmark progress and result payloads are structured data carried
inside ordinary protocol frames. They are not new protocol frame
types.

Each benchmark progress frame should include:

- `mode: "benchmark"`;
- `operations_total`;
- `operations_completed`;
- `elapsed_worker_ns`;
- `sample_index`.

The terminal result should include:

- `mode: "benchmark"`;
- `operations_total`;
- `progress_frames`;
- `elapsed_worker_ns`;
- `worker_ops_per_second_milli`;
- `first_operation_started_at`;
- `completed_at`.

`worker_ops_per_second_milli` is an integer rate scaled by 1000 to
avoid float encoding differences in tests. Direct tests assert schema,
ordering, monotonic elapsed time, cadence, and final totals. They must
not assert a fixed rate value because that number is
machine-dependent. The only performance claim in this slice is that
the worker emits a well-formed measurement summary for the later
supervisor benchmark harness to consume.

## 6. Data Flow

Baseline conformance flow:

1. The conformance harness loads `voom-fakes-manifest.toml`.
2. It resolves `benchmark-worker` through the established
   cross-package binary resolution mechanism.
3. It launches `benchmark-worker` with standard worker credentials.
4. The worker binds, prints `BOUND addr=...`, and waits for
   `/v1/operations`.
5. The typed and raw-wire conformance suites send ordinary
   `ProbeFile` requests with valid `payload.path`. Because those
   requests do not carry `payload.mode`, the worker defaults to
   `baseline`.
6. The worker emits a valid ordered progress stream and passes the
   same active-worker Tier 1 assertions as `echo-worker` and
   `chaos-worker`.

Benchmark-mode test flow:

1. A direct test launches `benchmark-worker`.
2. The test sends a `ProbeFile` request with valid `payload.path`,
   `payload.mode = "benchmark"`, `operations`, and optional
   `emit_every`.
3. The worker executes the configured no-op loop.
4. The worker emits progress frames at the configured cadence.
5. The worker emits one terminal result with the final metric summary.
6. The test verifies frame order, progress totals, schema stability,
   and stdin EOF shutdown.

The worker does not write durable state and does not know how a later
supervisor calculates dispatch latency, lease release timing, or CI
thresholds.

## 7. Error Handling

Expected protocol rejections return structured `ProtocolError`s:

- unsupported operation kind -> `UnknownOperation`;
- missing or malformed `payload.path` -> `InvalidPayload`;
- unknown `mode` -> `InvalidPayload`;
- missing or malformed benchmark config fields -> `InvalidPayload`;
- `operations = 0`, `operations > 10_000`, `emit_every = 0`, or
  `emit_every > operations` -> `InvalidPayload`;
- malformed request JSON, auth errors, version errors, and
  idempotency conflicts must match the current conformance
  expectations for worker HTTP behavior.

Idempotency behavior follows the active-worker contract: exact replay
with the same key returns the cached fixed response, and the same key
with a different request body is rejected. The cache must be bounded.
Because benchmark mode completes promptly, both baseline and completed
benchmark responses may be cached as fixed byte responses.

Benchmark results are payload data, not protocol status. A low
`worker_ops_per_second_milli` number is still a successful protocol
result in this slice. Later supervisor benchmark tests decide which
throughput numbers constitute a regression.

## 8. Tests

Sibling/unit tests:

- payload parser accepts `{ "path": "/library/example.mkv" }` as
  `baseline`;
- payload parser accepts `mode = "benchmark"` with valid
  `operations` and `emit_every`;
- payload parser defaults missing `emit_every` to `operations`;
- parser rejects `{}` and `{ "mode": "baseline" }` with missing
  `path`;
- parser rejects unknown mode;
- parser rejects missing, non-integer, zero, or excessive
  `operations`;
- parser rejects non-integer, zero, or too-large `emit_every`;
- frame builder for `baseline` emits `seq=0` progress and `seq=1`
  result;
- benchmark frame builder emits monotonic `operations_completed` and
  a terminal result whose totals match the requested operation count.

Integration tests:

- manifest promotion test proves `benchmark-worker` is active and not
  scaffolded;
- direct launch test proves `benchmark-worker` prints
  `BOUND addr=...`;
- `baseline` operation returns valid ordered frames and exits on stdin
  EOF;
- `benchmark` operation returns cadence-correct progress frames and a
  terminal metric summary;
- idempotent replay of the same benchmark request returns the cached
  response;
- same idempotency key with a different benchmark body is rejected;
- invalid benchmark payloads return structured protocol errors and do
  not crash the process;
- conformance integration launches `benchmark-worker` through the
  manifest and passes active-worker Tier 1.

Minimum verification:

- `cargo test -p voom-fakes --test benchmark_worker --all-features`
- `cargo test -p voom-fakes --bin benchmark-worker --all-features`
- `cargo build -p voom-fakes --bin benchmark-worker`
- `cargo test -p voom-conformance --all-features`
- `just ci`

## 9. Implementation Slices

1. Add the internal benchmark payload parser and sibling tests.
2. Replace the placeholder binary with real worker bootstrap plus
   baseline behavior.
3. Add direct launch and baseline integration tests.
4. Promote `benchmark-worker` in the conformance manifest and make
   conformance pass against it.
5. Add benchmark mode, metric frame construction, idempotency replay,
   and direct benchmark tests.
6. Run full verification and keep the branch green.

Each slice keeps conformance green for active workers before the next
benchmark behavior lands.

## 10. Follow-ups

After this worker-contract slice lands:

- design `voom-control-plane/tests/benchmark.rs` around supervisor
  dispatch throughput and latency measurement;
- choose fixed benchmark configurations for CI and local development;
- set p50, p95, and operations-per-second thresholds from observed
  baseline numbers, with enough margin to avoid noisy CI failures;
- decide whether benchmark summaries should later be stored as
  durable events or remain test-only observations.
