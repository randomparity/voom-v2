# Real subprocess crash-recovery implementation plan

## Goal

Extend the opt-in distributed stress harness so a configured set of first attempts dispatches to
real supervised `chaos-worker` subprocesses that exit while holding leases, after which the
existing recovery coordinator, synthetic retry lanes, and conservation oracle prove expiry,
reassignment, single terminal settlement, and complete child reaping.

The process prelude owns one dedicated remote node per crashed attempt. A responsive supervisor
actor owns watcher tasks and exactly-once exit observations; ordinary synthetic sessions start
only after the selected crash set has been acquired and every crash child has been reaped. ADR
0095 and the reviewed design specification are authoritative for this architecture.

Tech stack: Rust 2024, Tokio, reqwest for control-plane HTTP, `voom-worker-protocol::ClientHandle`
for worker dispatch, axum for the existing loopback API fixture, sqlx-backed typed repositories,
and the existing `ManualClock` recovery harness.

## Global constraints

- Branch: `feat/real-subprocess-crash-recovery-606`; base: `main`; scope token:
  `q606-bc84e057`.
- Host architecture: `x86_64`; target architectures: none declared; relationship:
  `no-target-declared`.
- No new dependency, feature, production ticket/API/schema/migration behavior, or public protocol
  contract. Changes stay in `voom-fakes`, its test harness, `justfile`, and its stress runbook.
- `VOOM_STRESS_PROCESS_CRASH_PERCENT` defaults to `0`, accepts `0..=25`, and the sum with the
  existing stall/crash percentages is at most 25.
- Readiness is one newline-terminated frame of at most 4 KiB within five seconds. Post-dispatch
  socket termination plus exit observation is bounded by five seconds. After kill, the controlled
  child remains owned until final reap.
- The supervisor is the sole child owner. Live service delivers one exit status per `Wait`;
  terminal shutdown may discard undeliverable tombstones only after command-channel closure and
  completed reap/accounting.
- Selected process tickets remain the only ready tickets during the prelude. Their durable media
  payload is unchanged; only the worker-protocol dispatch payload is replaced with the chaos crash
  payload. Synthetic retries consume the original durable payload.
- Unit tests follow the sibling `<source>_test.rs` layout. Real SQLite tests use real Tokio time
  and the injected `ManualClock`, never paused Tokio time.
- Guardrails: `cargo test -p voom-fakes`; `just stress`; `just fmt-check`; `just lint`;
  `just check-test-layout`; `just test`; `just ci`.

## File map

- Create `crates/voom-fakes/src/process_supervisor.rs`: test-support actor, child watchers,
  readiness parsing, shutdown/reap, pending waiter and completed-status tombstone state.
- Create `crates/voom-fakes/src/process_supervisor_test.rs`: deterministic actor lifecycle,
  oversized readiness, stay-alive timeout cleanup, delayed wait, and cancellation cleanup tests.
- Modify `crates/voom-fakes/src/lib.rs`: export `process_supervisor` for the remote runner and its
  tests.
- Modify `crates/voom-fakes/src/remote_runner.rs`: preserve activated worker epoch; add the
  process-crash prelude entry point using existing private activation/readiness/acquire methods and
  `voom_worker_protocol::ClientHandle`.
- Modify `crates/voom-fakes/src/remote_runner_test.rs`: process runner request/identity/error tests
  that do not duplicate supervisor lifecycle tests.
- Modify `crates/voom-fakes/src/bin/chaos_worker.rs`: accept `TranscodeVideo` only when parsed mode
  is `Crash`, leaving the baseline and other fault allowlist unchanged.
- Modify `crates/voom-fakes/src/bin/chaos_worker_test.rs`: prove transcode crash acceptance and
  transcode baseline rejection at the dispatch boundary.
- Modify `crates/voom-fakes/src/remote_stress_test.rs`: configuration, selected-root seeding,
  process prelude orchestration, observations, and conservation evidence.
- Modify `justfile`: prebuild `chaos-worker` before the opt-in stress test so the existing
  `voom_test_support::cargo_bin_or_build` lookup is a no-op under the prebuilt-worker guard.
- Modify `docs/runbooks/distributed-stress-harness.md`: new knob, process evidence, and a small
  real-process example.

## Task 1: Supervise real crash workers without losing exit evidence

This task creates the cancellation-safe child lifecycle used by the process prelude and extends
the existing chaos binary at the narrow operation boundary needed by a transcode stress lease.

### Interfaces

Create these interfaces in `process_supervisor.rs`:

```rust
pub struct ProcessSupervisor { /* bounded command sender + actor join */ }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChildId(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyChild {
    pub child_id: ChildId,
    pub pid: u32,
    pub bound: std::net::SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildExit {
    pub child_id: ChildId,
    pub code: Option<i32>,
    pub success: bool,
}

#[derive(Debug)]
pub enum ProcessSupervisorError { /* spawn/readiness/wait/protocol/join variants */ }

impl ProcessSupervisor {
    pub fn start() -> Self;
    pub async fn spawn(
        &self,
        binary: std::path::PathBuf,
        credentials: voom_worker_protocol::WorkerCredentials,
    ) -> Result<ReadyChild, ProcessSupervisorError>;
    pub async fn wait(&self, child_id: ChildId)
        -> Result<ChildExit, ProcessSupervisorError>;
    pub async fn shutdown(self) -> Result<Vec<ChildExit>, ProcessSupervisorError>;
}
```

The actor command enum is private: `Spawn`, `Wait`, and `Shutdown`. Each spawn creates one watcher
task that owns `tokio::process::Child`, stdin, and stdout. The actor registry entry is one of
`Running { shutdown, waiter }` or `Exited { status }`. A watcher completion either answers an
already registered waiter or stores `Exited`; a later single waiter consumes it. Shutdown closes
the live command phase, signals and joins every watcher, answers registered waiters, records every
reap, discards tombstones that can no longer be consumed, and returns only after the registry is
empty.

The chaos boundary change is:

```rust
let payload = parse_payload(req.payload.clone())?;
let supported = matches!(req.operation, OperationKind::ProbeFile | OperationKind::HashFile)
    || (req.operation == OperationKind::TranscodeVideo && payload.mode == ChaosMode::Crash);
```

Retain the existing JSON error shape for unsupported operations and do not accept baseline or
other fault modes for transcode.

### TDD steps

1. Link `process_supervisor_test.rs` from the new source with the repository sibling-test pattern.
   Write tests for: readiness success; >4 KiB/no-newline rejection; child exits before readiness;
   natural exit before delayed `wait`; duplicate wait rejection; stay-alive child shutdown;
   caller cancellation after natural exit without wait; and shutdown attempting every registered
   child. Test helper processes use the test executable with a private environment-selected helper
   branch, not a shell command. Run
   `cargo test -p voom-fakes --lib process_supervisor`; expect compilation failure because the
   interfaces do not exist.
2. Implement the minimum bounded actor/watcher state machine. Read readiness with `take(4097)` so
   limit exhaustion is distinguishable, wrap it in five seconds, require exactly
   `BOUND addr=<loopback>\n`, and retain `kill_on_drop(true)` only as a runtime-destruction
   backstop. Run the focused command; expect every supervisor test green.
3. In `chaos_worker_test.rs`, add direct `dispatch_operation` tests using a transcode request with
   `{"mode":"crash","path":"/stress/process-crash"}` and a transcode baseline request. The
   first must select `ChaosResponse::ExitProcess(101)`; the second must remain the existing unknown
   operation response. Run `cargo test -p voom-fakes --bin chaos-worker transcode`; first observe
   the crash test fail, then apply the narrow allowlist change and expect both green.
4. Run `cargo test -p voom-fakes --lib process_supervisor` and
   `cargo test -p voom-fakes --bin chaos-worker`; expect green. Run `just fmt-check`,
   `just check-test-layout`, and `just lint`; expect exit 0.
5. Commit the explicit Task 1 paths with `test: supervise real chaos worker processes`.

### Acceptance

- The actor remains responsive while watchers await children.
- A fast exit remains observable exactly once until terminal shutdown.
- Ordinary errors, timeout, cancellation, and shutdown reap every controlled child.
- Transcode is accepted only for chaos crash mode.

## Task 2: Seed process crashes into the existing conservation harness

This task connects the supervisor to remote lease acquisition, records typed process evidence,
then reuses the existing recovery and conservation implementation.

### Interfaces

Add to `remote_runner.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCrashObservation {
    pub pid: u32,
    pub node_id: NodeId,
    pub worker_id: WorkerId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub exit_code: Option<i32>,
}

impl RemoteSyntheticRunner {
    pub async fn run_once_to_process_crash(
        &self,
        supervisor: &ProcessSupervisor,
        binary: std::path::PathBuf,
        expected_tickets: &std::collections::HashSet<TicketId>,
    ) -> Result<(ExecutionRecord, ProcessCrashObservation), RemoteRunnerError>;
}
```

Preserve `worker_epoch` in `ActiveWorker`. The method activates one worker, generates a fresh
in-memory secret, spawns the child with that activated ID/epoch, marks readiness, acquires exactly
one lease, rejects a ticket outside `expected_tickets` before dispatch, and uses
`ClientHandle::dispatch` with:

```rust
OperationRequest {
    operation: OperationKind::TranscodeVideo,
    lease_id: lease.lease_id,
    payload: json!({"mode":"crash","path":"/stress/process-crash"}),
    heartbeat_deadline_ms: 1_000,
    progress_idle_deadline_ms: 1_000,
}
```

Require dispatch connection termination and a non-success exit within the shared five-second
deadline. Return an abandoned first-attempt execution record and process observation only after
`supervisor.wait` proves reap; never call heartbeat, complete, or fail.

Extend `StressConfig` with `process_crash_percent` and a pure
`process_crash_count(tickets, percent) -> usize`. Extend `SeededWorkload` with
`process_ticket_ids`. `seed_workload` creates that deterministic prefix as ready roots, leaves all
other tickets pending, and a new `release_remaining_workload` adds configured dependencies only
among the remaining tickets before marking them ready/unblocked. Process observations join the
existing execution records as `ExecutionAction::Abandoned`; the existing recovery set and
`assert_conservation` remain the sole oracle.

### TDD steps

1. Add config tests for default zero, `0..=25`, three-way percentage sum, rounding-down with
   non-zero minimum one, and maximum ticket count. Add workload tests proving the selected prefix
   is ready and every higher-priority non-selected ticket remains pending until release. Run
   `cargo test -p voom-fakes --lib stress_process`; expect failure before fields/helpers exist.
2. Implement only configuration and two-phase seeding/release. Run the same focused command;
   expect green. Verify existing zero-process stress configuration snapshots/assertions are
   unchanged.
3. Add remote-runner tests for activated epoch/credentials, mismatched acquired ticket cleanup,
   crash request typed lease/operation, clean-exit rejection, and observation only after explicit
   wait. Use the real loopback control-plane fixture and supervisor test helper where the worker
   boundary matters. Run `cargo test -p voom-fakes --lib process_crash`; expect red before the new
   entry point, then implement the minimum entry point and expect green.
4. Wire `run_stress`: resolve `chaos-worker` with
   `voom_test_support::cargo_bin_or_build("voom-fakes", "chaos-worker")` only when the process
   count is non-zero; register one remote node per selected crash; run crash attempts sequentially;
   require the observed ticket set equals the selected set; release the remaining workload; start
   existing synthetic sessions; and feed abandoned process records into the unchanged recovery and
   conservation paths. Hold the inner harness task separately from `ProcessSupervisor::shutdown`
   so every returned error or join error is combined only after supervisor cleanup.
5. Run a minimal ignored cell:
   `VOOM_STRESS_NODES=1 VOOM_STRESS_RUNNERS_PER_NODE=1 VOOM_STRESS_TICKETS=8 VOOM_STRESS_PROCESS_CRASH_PERCENT=25 VOOM_STRESS_DEPENDENCY_PERCENT=90 VOOM_STRESS_DRAIN_SECONDS=30 just stress`.
   Expect two non-zero process exits, two expired first leases, ten total attempts, eight terminal
   tickets, zero held leases, zero supervised children, and conservation success.
6. Prove the conservation test bites: temporarily append a second non-abandoned record for one
   process ticket before `assert_conservation`, rerun the minimal cell, and require the existing
   duplicate-execution diagnostic. Revert only that controlled fault and rerun the identical cell
   green.
7. Update `just stress` to prebuild `chaos-worker` and keep the ignored library test as its only
   executed test. Update the runbook with the new default/range, evidence fields, and minimal cell.
   Run `just --list`, `cargo test -p voom-fakes`, and the minimal process cell; expect green.
8. Run `just fmt-check`, `just lint`, `just check-test-layout`, `just test`, and `just ci` bare.
   Record observed durations and first real-process findings for the PR body.
9. Commit implementation paths with `test: exercise lease recovery after process crashes`, then
   commit recipe/runbook paths with `docs: document process crash stress coverage`.

### Acceptance

- The requested number of selected first attempts exits in real child processes after acquiring
  typed leases over the API and dispatching over worker sockets.
- Every child observation proves explicit reap; cleanup leaves the supervisor registry empty on
  success and error.
- Recovery expires exactly the selected process-crashed leases; synthetic retries settle them.
- Existing attempt/event/lease/ticket conservation and duplicate-settlement checks pass unchanged.
- Zero percent preserves existing `just stress` behavior; the process arm remains opt-in.

## Durable handoff

- Current phase after this plan: design review, then oathbind scope audit.
- Branch/base: `feat/real-subprocess-crash-recovery-606` / `main`.
- Guardrails: `cargo test -p voom-fakes`; `just stress`; `just fmt-check`; `just lint`;
  `just check-test-layout`; `just test`; `just ci`.
- Design review deferrals: none.
- Open findings: none after the operator-approved confirming spec review.
