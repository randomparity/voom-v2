---
name: voom-sprint-2-phase-4-chaos-worker-design
description: Sprint 2 Phase 4 follow-up design — promote chaos-worker from scaffold to active conformance target and implement worker-level chaos modes before supervisor durable-state E2E assertions.
status: proposed
date: 2026-05-21
sprint: 2
phase: 4
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
parent_sections: §2 Phase 4 chaos worker; §4.2 progress stream invariants; §4.9 supervisor watchdog state machine
predecessor_spec: docs/superpowers/specs/2026-05-21-voom-sprint-2-phases-4-5-6-conformance-fill-in-design.md
scope: chaos-worker contract implementation and conformance promotion; supervisor durable-state chaos E2E remains deferred
---

# Sprint 2 Phase 4 — Chaos Worker Contract Implementation

## 1. Goal

The previous Phase 4/5/6 slice put `chaos-worker` on disk as a
scaffold, and the conformance fill-in made active-worker admission
real. This slice promotes `chaos-worker` from scaffold to active by
implementing a real protocol worker with deterministic, payload-driven
fault modes.

The implementation target is the worker contract boundary:

1. `chaos-worker` starts like any Sprint 2 local worker.
2. Its non-faulting baseline operation passes the active conformance
   Tier 1 suite.
3. Its fault modes are directly testable without involving the
   supervisor durable-state layer yet.

Supervisor chaos E2E tests remain a follow-up. This keeps the fault
injector stable before `voom-control-plane/tests/chaos/` maps those
faults onto `WorkerCrash`, `WorkerTimeout`, `ProgressTimeout`, and
`MalformedWorkerResult` state transitions.

## 2. Scope

In scope:

- Replace `crates/voom-fakes/src/bin/chaos_worker.rs` placeholder
  behavior with a real loopback HTTP worker.
- Keep `chaos-worker` independent from `voom-fake-support`.
- Select all chaos behavior through `OperationRequest.payload.mode`.
- Default missing `mode` to `baseline` so generic conformance requests
  continue to work.
- Promote `chaos-worker` to an active required entry in
  `crates/voom-conformance/voom-fakes-manifest.toml`.
- Add worker-level tests for baseline behavior, process crash, stall,
  malformed progress body, non-converging progress, deadline-oriented
  slow progress, and invalid payload rejection.

Out of scope:

- Supervisor durable-state assertions under
  `voom-control-plane/tests/chaos/`.
- Benchmark-worker implementation.
- Phase 3 fake-provider completion.
- Raw HTTP envelope corruption from a process-backed worker. This
  slice corrupts the NDJSON progress body after a valid
  `OperationResponse`; raw envelope corruption remains covered by
  conformance-owned negative fixtures until a later supervisor E2E
  design needs a process-backed version.

Exit criteria:

- `chaos-worker` is no longer listed under `[scaffold].binaries`.
- `cargo test -p voom-conformance --all-features` launches
  `chaos-worker` and `echo-worker` as active workers.
- `chaos-worker` passes typed and raw-wire Tier 1 conformance through
  its default baseline behavior.
- Direct worker-mode tests prove each fault mode is deterministic and
  distinguish deliberate chaos from invalid test payloads.

## 3. Architecture

`chaos-worker` stays in `crates/voom-fakes/src/bin/chaos_worker.rs`.
It depends on `voom-worker-protocol` directly and uses `HttpServer`
for the normal worker boundary. It does not use `voom-fake-support`,
because the chaos worker must not inherit behavior from the shared
fake-provider helper path.

The binary has three internal responsibilities:

| Part | Responsibility |
|---|---|
| Bootstrap | Parse `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`, `VOOM_WORKER_EPOCH`, and `VOOM_WORKER_BIND`; start `HttpServer`; print `BOUND addr=...`; shut down on stdin EOF. |
| Payload parser | Decode `payload.mode` and optional timing fields into a typed internal `ChaosMode`. Reject unknown modes and malformed fields before fault execution. |
| Mode dispatcher | Convert a valid `OperationRequest` into an `OperationDispatch`, a structured `ProtocolError`, a process exit, or a deliberately non-terminating response body. |

The startup contract mirrors `echo-worker`:

- required env: `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`,
  `VOOM_WORKER_EPOCH`;
- optional env: `VOOM_WORKER_BIND`, defaulting to `127.0.0.1:0`;
- readiness signal: one stdout line, `BOUND addr=<actual>`;
- parent-death behavior: stdin EOF triggers graceful server shutdown.

The worker supports `OperationKind::ProbeFile` for conformance and
for all direct mode tests in this slice. Other operation kinds return
`ProtocolError::UnknownOperation`. This avoids widening the operation
surface before the supervisor E2E tests need it.

### 3.1 Cross-package binary resolution

`echo-worker` is a binary target in `voom-conformance`, so Cargo makes
`CARGO_BIN_EXE_echo-worker` available to that package's integration
tests. `chaos-worker` lives in the separate `voom-fakes` package, so
`cargo test -p voom-conformance` cannot rely on
`CARGO_BIN_EXE_chaos-worker` being set.

Promotion therefore includes one explicit resolution step:

- direct `voom-fakes` tests use `CARGO_BIN_EXE_chaos-worker`;
- `voom-conformance` either receives an explicit manifest `path` for
  `chaos-worker`, or the integration test builds
  `cargo build -p voom-fakes --bin chaos-worker` and resolves the
  binary from Cargo's shared target directory before launching the
  active conformance suites;
- missing `chaos-worker` remains a hard failure once the manifest marks
  it active.

The implementation plan must choose one of those two conformance
resolution mechanisms and pin it with a manifest/resolution test. It
must not silently skip `chaos-worker` because it belongs to another
package.

## 4. Payload Contract

Chaos behavior is selected by the operation payload, not by headers.
Headers remain reserved for protocol versioning, worker identity, auth,
and idempotency.

Common payload shape:

```json
{
  "mode": "baseline",
  "progress_count": 3,
  "progress_interval_ms": 50,
  "stall_ms": 500
}
```

All fields except `mode` are optional. Missing `mode` means
`baseline`. Numeric fields must be non-negative and small enough for
CI-bound tests; the implementation should reject values above a fixed
cap rather than sleeping for unbounded user-supplied durations. A
reasonable Sprint 2 cap is 30 seconds per operation.

Mode names:

| Mode | Behavior |
|---|---|
| `baseline` | Emit `Progress(seq=0)` and `Result(seq=1)` with a small result payload echoing the mode. |
| `crash` | Terminate the worker process after the operation is accepted and before any terminal frame is delivered. |
| `stall` | Accept the operation and emit no progress for `stall_ms`, long enough for caller-side timeout tests. |
| `malformed_result` | Return a valid `OperationResponse` followed by malformed NDJSON body bytes. |
| `non_converging_progress` | Emit valid monotonic progress frames and then keep the response open without a terminal frame. |
| `deadline_exceeded` | Emit progress at `progress_interval_ms` cadence without completing before the caller's configured progress deadline. |

Invalid payloads are not chaos modes. Unknown `mode`, wrong field
types, excessive timing values, and unsupported combinations return
`ProtocolError::InvalidPayload`.

## 5. Data Flow

Baseline conformance flow:

1. The conformance harness loads `voom-fakes-manifest.toml`.
2. It resolves `chaos-worker` through the cross-package resolution
   mechanism in §3.1.
3. It launches `chaos-worker` with standard worker credentials.
4. The worker binds, prints `BOUND addr=...`, and waits for
   `/v1/operations`.
5. The typed and raw-wire conformance suites send ordinary
   `ProbeFile` requests. Because those requests do not carry
   `payload.mode`, the worker defaults to `baseline`.
6. The worker emits a valid ordered progress stream and passes the
   same active-worker Tier 1 assertions as `echo-worker`.

Fault-mode test flow:

1. A direct test launches `chaos-worker`.
2. The test sends a `ProbeFile` request with an explicit
   `payload.mode`.
3. The worker executes the selected deterministic behavior.
4. The test classifies the observable worker-boundary outcome:
   process exit, caller-side timeout, malformed stream rejection, or
   valid non-terminal progress.

The worker does not write durable state and does not know how a later
supervisor maps boundary observations to lease failure classes.

## 6. Error Handling

Expected protocol rejections return structured `ProtocolError`s:

- unsupported operation kind -> `UnknownOperation`;
- missing or malformed required payload fields for a selected mode ->
  `InvalidPayload`;
- timing values above the allowed cap -> `InvalidPayload`;
- malformed request JSON, auth errors, version errors, and
  idempotency conflicts remain owned by `HttpServer` and the protocol
  crate.

Deliberate chaos modes must not be implemented as structured
`ProtocolError`s. They should create the observable failure being
tested: process exit, an idle stream, malformed NDJSON, valid progress
without a terminal frame, or progress timing that exceeds the caller's
deadline.

`crash` may call `std::process::exit(101)` from the operation path.
The exact non-zero status is not semantically important, but tests
should assert non-zero exit and absence of a clean terminal frame.

`malformed_result` corrupts the progress body only. The HTTP status and
initial `OperationResponse` stay valid so the malformed-stream case
targets the NDJSON reader boundary, matching Sprint 2 §4.2.

## 7. Tests

Sibling/unit tests:

- payload parser accepts missing mode as `baseline`;
- payload parser accepts each known mode;
- parser rejects unknown mode;
- parser rejects negative, non-integer, or excessive timing values;
- frame builder for `baseline` emits `seq=0` progress and `seq=1`
  result;
- malformed body fixture is not valid NDJSON.

Integration tests:

- manifest promotion test proves `chaos-worker` is active and not
  scaffolded;
- direct launch test proves `chaos-worker` prints `BOUND addr=...`;
- `baseline` operation returns valid ordered frames and exits on stdin
  EOF;
- conformance integration launches `chaos-worker` through the manifest
  and passes active-worker Tier 1;
- `crash` exits non-zero before a terminal frame is accepted;
- `stall` keeps the response pending until a short caller-side timeout;
- `malformed_result` is rejected by `NdjsonReader`;
- `non_converging_progress` yields valid progress then no terminal
  frame before timeout;
- `deadline_exceeded` produces timing suitable for later watchdog
  `ProgressTimeout` assertions;
- invalid payloads return structured protocol errors and do not crash
  the process.

Minimum verification:

- `cargo test -p voom-fakes --all-features`
- `cargo test -p voom-conformance --all-features`
- `just ci`

## 8. Implementation Slices

1. Add the internal chaos payload parser and sibling tests.
2. Replace the placeholder binary with real worker bootstrap plus
   baseline behavior.
3. Add direct launch and baseline integration tests.
4. Promote `chaos-worker` in the conformance manifest and make
   conformance pass against it.
5. Add process-backed fault modes and direct mode tests.
6. Run full verification and keep the branch green.

Each slice keeps conformance green for active workers before the next
fault mode lands.

## 9. Follow-ups

After this worker-contract slice lands:

- design `voom-control-plane/tests/chaos/` around durable state and
  watchdog classification;
- map `crash`, `stall`, `non_converging_progress`,
  `deadline_exceeded`, and `malformed_result` to the supervisor's
  failure taxonomy;
- decide whether a later process-backed raw-envelope corruption mode
  is needed beyond the conformance-owned negative fixtures;
- continue to Phase 5 `benchmark-worker` only after the Phase 4 worker
  and supervisor E2E slices are complete.
