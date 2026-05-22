---
name: voom-sprint-2-phase-5-control-plane-benchmark-harness-design
description: Sprint 2 Phase 5 follow-up design — add a control-plane-owned benchmark harness at the currently implemented worker protocol boundary before full supervisor throughput gates.
status: proposed
date: 2026-05-21
sprint: 2
phase: 5
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
parent_sections: §2 Phase 5 benchmark worker; §4.2 progress stream; §4.7 conformance; §5 test discipline
predecessor_spec: docs/superpowers/specs/2026-05-21-voom-sprint-2-phase-5-benchmark-worker-design.md
scope: test-only control-plane benchmark harness against benchmark-worker over the public worker protocol; full durable supervisor throughput thresholds remain deferred
---

# Sprint 2 Phase 5 — Control-Plane Benchmark Harness

## 1. Goal

The previous Phase 5 slice made `benchmark-worker` an active protocol
worker and stabilized its benchmark-mode progress/result frames. This
slice proves those frames are useful from the control-plane side by
adding the first `voom-control-plane` benchmark integration test.

The target is the boundary that exists today:

1. `voom-control-plane` test code launches `benchmark-worker`.
2. The test drives it with `voom_worker_protocol::HttpClient`.
3. The test consumes the worker's NDJSON progress stream and records
   local latency samples plus worker-reported throughput.

This is not the full supervisor benchmark from the Sprint 2 overview.
It is a deliberately narrow precursor that keeps measurement logic
visible in the control-plane test suite while avoiding unimplemented
durable supervisor behavior.

## 2. Scope

In scope:

- Add `crates/voom-control-plane/tests/benchmark.rs`.
- Add only the `voom-control-plane` dev-dependencies and Tokio
  feature flags needed for async process launch, `HttpClient`, and
  NDJSON frame consumption.
- Spawn `benchmark-worker` as a child process with ordinary Sprint 2
  worker credentials and parent-death stdin behavior.
- Dispatch benchmark-mode `OperationKind::ProbeFile` requests through
  the public `ClientHandle` API.
- Validate benchmark progress/result frame schema, cadence, monotonic
  counters, and positive worker throughput.
- Compute observational min/median/max summaries for dispatch ack
  latency, stream completion latency, and
  `worker_ops_per_second_milli`.

Out of scope:

- Implementing `LocalWorkerSupervisor`.
- Scheduler dequeue or `WorkerSelector` routing.
- Durable lease dispatch intents, worker incarnation rows, restart
  reconciliation, or watchdog arbitration.
- Hard CI throughput floors, p50 gates, or p95 gates.
- Production benchmark modules or public control-plane benchmark API.
- Changes to `benchmark-worker` behavior unless the test uncovers a
  worker contract bug.

Exit criteria:

- `cargo test -p voom-control-plane --test benchmark --all-features`
  launches a prebuilt `benchmark-worker`, completes all samples,
  validates the metric stream, and includes a compact summary in
  assertion/failure diagnostics.
- The test fails on protocol errors, missing benchmark fields,
  unexpected frame counts, non-monotonic progress, non-positive worker
  throughput, early worker exit, bind timeout, or cleanup failure.
- The test uses generous sanity ceilings instead of machine-specific
  throughput thresholds.

## 3. Architecture

The harness is test-only and local to
`crates/voom-control-plane/tests/benchmark.rs`. It must not add
production code or expose new crate API. If repeated setup code becomes
large during implementation, keep helpers private inside the
integration test file.

The test talks only to public protocol surfaces:

| Part | Responsibility |
|---|---|
| Worker launcher | Resolve and spawn `benchmark-worker`, pass credentials and `VOOM_WORKER_BIND=127.0.0.1:0`, keep stdin open, and parse `BOUND addr=...` from stdout. |
| Benchmark runner | Build benchmark-mode `OperationRequest`s, call `ClientHandle::dispatch`, measure request-to-ack and request-to-terminal durations with `std::time::Instant`, and drain the NDJSON stream. |
| Frame validator | Require cadence-correct progress frames, monotonic worker elapsed time, monotonic `operations_completed`, one terminal result, and no frame after terminal. |
| Summary calculator | Sort successful samples and expose min/median/max for dispatch ack latency, stream completion latency, and worker-reported throughput through assertion/failure diagnostics. |

The initial request shape is fixed so results are comparable across
runs:

```json
{
  "path": "/library/benchmark.mkv",
  "mode": "benchmark",
  "operations": 1000,
  "emit_every": 100
}
```

This yields ten expected progress frames and one terminal result. The
test should run one warmup sample and five measured samples. Warmup
validates the stream like every other sample but is excluded from the
summary so process startup and first-use effects do not skew the
observational numbers.

## 4. Binary Resolution

`CARGO_BIN_EXE_benchmark-worker` is not guaranteed for
`voom-control-plane` integration tests because `benchmark-worker`
lives in the separate `voom-fakes` package. The harness must resolve
the binary using the same cross-package rule established by
`voom-conformance`:

1. Prefer `VOOM_BENCHMARK_WORKER_BIN` when set.
2. Otherwise use `CARGO_BIN_EXE_benchmark-worker` when Cargo provides
   it.
3. Otherwise resolve `target/debug/benchmark-worker`, using
   `CARGO_TARGET_DIR` if set and the workspace `target` directory if
   not.

Missing binary is a test setup failure with an actionable message. It
must not be silently skipped, because this phase is the first
control-plane-owned benchmark gate.

The primary verification flow must build the cross-package worker
binary before running the control-plane integration test:

```bash
cargo build -p voom-fakes --bin benchmark-worker
cargo test -p voom-control-plane --test benchmark --all-features
```

The control-plane test may rely on that prerequisite instead of
invoking Cargo recursively from inside the test process.

## 5. Data Flow

Each measured sample follows this sequence:

1. Generate a unique lease id and idempotency key.
2. Build an `OperationRequest` with `OperationKind::ProbeFile`,
   `heartbeat_deadline_ms = 1000`, `progress_idle_deadline_ms = 1000`,
   and the fixed benchmark payload.
3. Record `request_start = Instant::now()`.
4. Call `HttpClient::dispatch`.
5. Record dispatch ack latency when the immediate `OperationResponse`
   is returned. Zero-duration local measurements are valid.
6. Consume frames from the returned NDJSON stream until the terminal
   result.
7. Record stream completion latency. Zero-duration local measurements
   are valid.
8. Validate the progress cadence and terminal benchmark summary.
9. Assert one additional read after terminal returns the protocol's
   terminal-after-terminal error.
10. Add the sample to the summary if it is not the warmup run.

The terminal result payload must include:

- `mode = "benchmark"`;
- `operations = 1000`;
- `progress_frames = 10`;
- `elapsed_worker_ns > 0`;
- `worker_ops_per_second_milli > 0`.

Every progress frame must include:

- `mode = "benchmark"`;
- `sample_index` matching zero-based frame order;
- `operations_completed` equal to `100, 200, ..., 1000`;
- `elapsed_worker_ns` greater than or equal to the previous progress
  frame's elapsed value.

`sample_index` is the benchmark-worker's progress-frame index within
one benchmark operation. It is not the outer warmup/measured sample
number used by the control-plane harness.

The harness records local timings as observational values only. The
worker's own elapsed time and throughput remain the worker-reported
metric contract; the control-plane test does not attempt to reconcile
clock domains.

## 6. Thresholds

The first benchmark gate must avoid flaky performance assertions.
Assertions are therefore limited to contract and sanity checks:

- each sample completes within five seconds;
- dispatch ack latency is at or below one second;
- stream completion latency is at or below five seconds;
- worker-reported throughput is positive;
- measured sample count is exactly five;
- progress frame count is exactly ten per sample.

The compact summary includes min/median/max for:

- dispatch ack latency;
- stream completion latency;
- `worker_ops_per_second_milli`.

The harness must not use `println!` or `eprintln!` for the summary.
Workspace lint settings deny stdout/stderr print macros under
`just ci`, and `allow` attributes are denied. The summary should be
available through assertion messages and helper return values so a
failing test gives useful diagnostics without violating lint policy.

These numbers become baseline data for a later supervisor benchmark
design. A later slice may promote calibrated values into CI regression
thresholds after the full supervisor dispatch path exists and enough
local/CI observations are available.

## 7. Failure Handling And Cleanup

The harness must fail fast with precise messages for:

- worker process exits before printing `BOUND addr=...`;
- bind line is malformed or times out;
- dispatch returns a protocol error;
- stream read returns malformed frames, missing fields, wrong cadence,
  non-monotonic counters, or no terminal result;
- terminal result is followed by another frame;
- child cleanup times out.

Cleanup should close stdin, wait for the worker with a bounded timeout,
and kill the child on timeout. The cleanup path must run even after
test failure so repeated benchmark runs do not leave local worker
processes behind.

The integration test should use a `Result`-returning async body rather
than direct panics for ordinary validation failures. The body records
the first error, runs explicit async cleanup, and then returns the
recorded error. The launch guard must also have a synchronous `Drop`
fallback using `tokio::process::Child::start_kill` or equivalent so
panic/unwind paths still signal the child even though `Drop` cannot
await process shutdown.

## 8. Testing

Primary verification:

- `cargo build -p voom-fakes --bin benchmark-worker`
- `cargo test -p voom-control-plane --test benchmark --all-features`

Regression checks for the worker boundary this test depends on:

- `cargo test -p voom-fakes --test benchmark_worker --all-features`
- `cargo test -p voom-fakes --bin benchmark-worker --all-features`

Branch verification:

- `just ci`

The implementation plan should start by adding a failing
`voom-control-plane` integration test that cannot compile or cannot
resolve `benchmark-worker` until the dev-dependency and launcher work
is added. The final implementation should keep all benchmark helpers
private to the integration test unless a later supervisor design
introduces production benchmark orchestration.

## 9. Future Work

This phase intentionally stops before the full Sprint 2 benchmark exit
criterion. Follow-up designs can:

- route the same benchmark request through `LocalWorkerSupervisor`;
- measure scheduler dequeue and durable lease release overhead;
- record p50/p95 dispatch and stream latencies across the supervisor
  path;
- calibrate CI regression thresholds from repeated observations;
- compare direct-protocol numbers from this harness against full
  supervisor numbers to isolate overhead.
