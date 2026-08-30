# Distributed stress-harness implementation plan

## Goal

Add an opt-in, configurable real-HTTP stress harness that runs K worker runners on each of L
registered nodes and proves ticket conservation after a mixed backlog drains.

ADR 0094 makes one node session own each activation and all K worker declarations. Each worker
runs concurrent lanes; a shared read/write gate serializes lease mutations against explicit
abandoned-lease recovery. The harness compares an independent execution log with typed durable
ticket, lease, and event reads.

Tech stack: Rust, Tokio multi-thread tests and synchronization, axum real sockets, reqwest,
`voom-control-plane`, `voom-store`, `voom-events`, and the existing fake-provider dispatcher.

## Global constraints

- Branch: `feat/stress-harness-581`; base: `main`; scope token: `q581-0dae3cc1`.
- Host architecture: x86_64; target architectures: none declared; relationship:
  `no-target-declared`.
- Preserve `RemoteSyntheticRunner::new(RemoteRunnerConfig)` and
  `run_once_to_completion(&self) -> Result<RemoteRunnerSummary, RemoteRunnerError>`.
- One activation per node declares exactly K workers; each worker has `max_parallel >= 2` lanes.
- Hold a recovery-gate read permit across acquire, lease-heartbeat, complete, and fail HTTP
  requests. Dispatch does not hold it. Recovery holds the write permit from held-lease snapshot
  through healthy heartbeat refresh and `remote_recover`.
- Use real Tokio time around SQLite and an injected `ManualClock` for domain timestamps.
- Stress lease TTL is one second; node heartbeat TTL is 60 seconds. Refresh healthy leases at
  `T0 + 500ms`; recover at `T0 + 1250ms`.
- No new dependencies, schema, migration, production API, or durable payload contract.
- Follow sibling unit-test layout. The new ignored test is linked from `remote_runner.rs` as a
  second sibling test module.
- Guardrails: `cargo test -p voom-fakes --lib remote_session`; `cargo test -p voom-fakes --lib
  stress_conservation`; `just stress`; `just fmt-check`; `just lint`; `just check-test-layout`;
  `just check-paused-time-db`; `just check-transaction-openers`; `just test`; `just ci`.

## File map

- Modify `crates/voom-fakes/src/remote_runner.rs`: expose reusable node-session, execution-log,
  fault-policy, and recovery-gate interfaces while preserving the one-shot runner.
- Modify `crates/voom-fakes/src/remote_runner_test.rs`: preserve existing tests and add focused
  node-session/gate unit tests.
- Create `crates/voom-fakes/src/remote_stress_test.rs`: configuration, workload generation,
  ignored real-socket harness, conservation implementation, focused assertion tests.
- Modify `crates/voom-fakes/Cargo.toml`: add `voom-events` as a dev dependency if typed event kinds
  are referenced directly by the stress test.
- Modify `justfile`: add the opt-in `stress` recipe only; do not add it to `ci`.
- Create `docs/runbooks/distributed-stress-harness.md`: environment knobs, examples, output, and
  failure reproduction.
- Modify `docs/workflow/specs/2026-08-29-distributed-stress-harness-design.md` only if
  implementation evidence corrects a factual statement; record first-run results in the PR, not
  the committed design.

## Task 1 — Run many workers in one remote node session

Files:

- Modify/test `crates/voom-fakes/src/remote_runner.rs`.
- Modify/test `crates/voom-fakes/src/remote_runner_test.rs`.

Interfaces:

```rust
#[derive(Debug, Clone)]
pub struct RemoteWorkerConfig {
    pub logical_name: String,
    pub operations: Vec<ControlPlaneOperationKind>,
    pub artifact_access: Vec<String>,
    pub max_parallel: u32,
}

#[derive(Debug, Clone)]
pub struct RemoteNodeSessionConfig {
    pub base_url: String,
    pub node_id: NodeId,
    pub token: SecretString,
    pub workers: Vec<RemoteWorkerConfig>,
    pub max_polls: u32,
    pub idle_timeout: Duration,
    pub poll_interval: Duration,
    pub lease_ttl_seconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionAction { Completed, Failed, StalledThenCompleted, Abandoned }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRecord {
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub worker_id: WorkerId,
    pub acquisition_ordinal: u32,
    pub action: ExecutionAction,
}

pub trait RemoteFaultPolicy: Send + Sync {
    fn action(&self, ticket_id: TicketId, acquisition_ordinal: u32) -> ExecutionAction;
}

pub struct RemoteExecutionState { /* private ordinals + records + fault policy */ }

impl RemoteExecutionState {
    pub fn new(faults: Arc<dyn RemoteFaultPolicy>) -> Self;
    pub async fn record_acquisition(
        &self,
        ticket_id: TicketId,
        lease_id: LeaseId,
        worker_id: WorkerId,
    ) -> ExecutionRecord;
    pub async fn records(&self) -> Vec<ExecutionRecord>;
}

pub struct RemoteNodeSession { /* private request state, shared execution state, recovery gate */ }

impl RemoteNodeSession {
    pub fn new(config: RemoteNodeSessionConfig, executions: Arc<RemoteExecutionState>) -> Self;
    pub async fn run_until_cancelled(
        &self,
        stop: CancellationToken,
    ) -> Result<RemoteRunnerSummary, RemoteRunnerError>;
    pub async fn with_recovery_gate<T>(
        &self,
        operation: impl Future<Output = Result<T, RemoteRunnerError>>,
    ) -> Result<T, RemoteRunnerError>;
    pub async fn execution_log(&self) -> Vec<ExecutionRecord>;
}
```

Use the already installed Tokio primitives; if `CancellationToken` is unavailable without
`tokio-util`, replace it with `tokio::sync::watch::Receiver<bool>` rather than adding a dependency.
The manifest currently has Tokio `sync`, so the expected implementation uses `watch`.

Steps:

1. Write `node_session_activates_all_workers_once` in `remote_runner_test.rs`. Register one node,
   configure three workers with `max_parallel = 2`, start the session, and query typed workers to
   require one active incarnation and three ready workers. Run
   `cargo test -p voom-fakes --lib node_session_activates_all_workers_once`; expect compilation to
   fail because `RemoteNodeSession` is absent.
2. Generalize the private activation JSON from one declaration to `workers.iter()`, validate the
   response has exactly the same logical-name set, mark each returned worker ready, and spawn
   `max_parallel` lane futures per returned worker. Keep node heartbeat owned once by the session,
   not repeated independently by every worker lane. Run the focused test; expect one pass.
3. Write `recovery_gate_blocks_every_lease_mutation` using barriers around acquire and completion.
   It must prove a held write permit blocks acquire, heartbeat, complete, and fail read permits,
   while local dispatch does not need one. Run the focused test and expect failure until each
   request method acquires `RwLock::read_owned`; then expect one pass.
4. Add `ticket_id` to the private `RemoteLeaseDispatch` response decoder, matching the public API
   payload. Do not add `attempt`: the HTTP contract does not carry it. Share one
   `RemoteExecutionState` across every node session. In one mutex critical section keyed by
   `TicketId`, increment the acquisition ordinal, select the action, and append the
   `ExecutionRecord` before applying that action. Add a cross-session concurrency test that calls
   `record_acquisition` concurrently for one ticket and requires ordinals `[1, 2]` with exactly one
   record eligible for first-ordinal abandonment. An
   `Abandoned` action returns the lane to polling without heartbeat or settlement; `StalledThenCompleted`
   sleeps for the configured bounded duration, heartbeats, then completes. Completed/failed retain
   current dispatch classification.
5. Re-implement `run_once_to_completion` as the existing one-worker behavior over the shared
   activation/request helpers, not over the long-running session loop; existing semantics still
   return after one terminal lease or the idle budget. Run `cargo test -p voom-fakes --lib
   remote_runner`; expect all existing and new tests green.
6. Commit explicit runner paths with `feat: run many synthetic workers per remote node`.

Acceptance:

- One activation creates K ready workers and no sibling activation fences them.
- Exactly K×`max_parallel` lane tasks start per node session.
- All lease mutations participate in the recovery gate; dispatch does not.
- Every acquired lease produces one independent execution record, including abandonment.
- Existing one-shot runner tests remain unchanged and green.

## Task 2 — Generate, drain, and conserve a distributed backlog

Files:

- Create/test `crates/voom-fakes/src/remote_stress_test.rs`.
- Modify `crates/voom-fakes/src/remote_runner.rs` only to add the sibling module declaration:
  `#[cfg(test)] #[path = "remote_stress_test.rs"] mod stress_tests;`.
- Modify `crates/voom-fakes/Cargo.toml` only if the test needs the existing `voom-events` crate.

Interfaces local to the sibling test:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct StressConfig { nodes: usize, runners_per_node: usize, max_parallel: u32,
    tickets: usize, dependency_percent: u8, stall_percent: u8, crash_percent: u8,
    seed: u64, drain_timeout: Duration }

#[derive(Debug)]
struct SeededWorkload { ticket_ids: Vec<TicketId>, dependencies: Vec<(TicketId, TicketId)> }

#[derive(Debug)]
struct ConservationInput { tickets: Vec<Ticket>, leases: Vec<Lease>,
    events: Vec<EventRow>, executions: Vec<ExecutionRecord>,
    dependencies: Vec<(TicketId, TicketId)> }

fn stress_config_from_env() -> Result<StressConfig, String>;
fn select_fault(seed: u64, ticket_id: TicketId, acquisition_ordinal: u32,
    stall_percent: u8, crash_percent: u8) -> ExecutionAction;
fn assert_conservation(input: &ConservationInput) -> Result<(), String>;
```

Steps:

1. Write table tests for every environment bound and the combined fault-percentage cap. Isolate
   environment mutation with the test module's single mutex and restore every prior value. Run
   `cargo test -p voom-fakes --lib stress_config`; expect failure before the parser exists, then
   implement direct `std::env::var` parsing with named errors and rerun green.
2. Rename the final parameter to `acquisition_ordinal`. Write deterministic selection tests proving
   ordinal >1 never abandons, identical seed/ticket
   yields identical action, and configured zeroes produce completed actions. Implement selection
   with the installed `blake3` crate over seed/ticket/ordinal bytes; map the first digest byte into
   0–99. Run `cargo test -p voom-fakes --lib stress_fault`; expect green.
3. Write synthetic conservation tests for: valid success; duplicate non-abandoned execution;
   attempt/log/event mismatch; leaked held lease; mismatched terminal event; dependency acquisition
   before prerequisite success; and truncated event pagination fixture. Each negative test must
   assert its actionable diagnostic. Implement one accumulator that sorts diagnostics before
   joining them. Run `cargo test -p voom-fakes --lib stress_conservation`; expect all cases green.
4. Write ignored `distributed_stress_conserves_every_ticket`. Create a pinned `TempDatabase`, open
   the pool, inject `Arc<ManualClock>` through `ControlPlane::open_with_pool`, start one real axum
   listener, register L nodes with 60-second heartbeat TTL, and seed tickets/dependencies through
   public control-plane/store APIs. Start one `RemoteNodeSession` per node behind a barrier.
   Run `cargo test -p voom-fakes --lib distributed_stress_conserves_every_ticket -- --ignored
   --nocapture`; expect red before drain/recovery orchestration is complete.
5. Implement drain/recovery. When abandoned records appear, take every session write permit in
   deterministic node order, advance `ManualClock` to +500ms, heartbeat every non-abandoned held
   lease through its owning session, advance to +1250ms, call `remote_recover`, require no stale
   nodes and exact abandoned expiry, then release gates. Poll typed ticket counts until all seeded
   tickets are terminal and no held lease remains or the wall deadline expires. Stop and join every
   session before assertions; abort and await the server on every path.
6. Paginate `SqliteEventRepo::list` until `next_cursor` is absent, collect all ticket and lease rows
   with typed repositories, merge execution logs, and call `assert_conservation`. Run a small cell:
   `VOOM_STRESS_NODES=2 VOOM_STRESS_RUNNERS_PER_NODE=2 VOOM_STRESS_TICKETS=40
   VOOM_STRESS_DRAIN_SECONDS=30 cargo test -p voom-fakes --lib
   distributed_stress_conserves_every_ticket -- --ignored --nocapture`; expect one pass.
7. Run a faulted small cell with `VOOM_STRESS_STALL_PERCENT=5` and
   `VOOM_STRESS_CRASH_PERCENT=5`; expect one pass, nonzero retry count, and exact abandoned expiry.
8. Bite proof: use `apply_patch` to duplicate the first execution record immediately before
   conservation, run the same small cell, and require failure containing `duplicate non-abandoned
   execution`. Revert only that controlled line with `apply_patch`, rerun the small cell, and require
   green. Do not commit the fault.
9. Commit explicit harness/manifest paths with `test: add distributed ticket conservation stress`.

Acceptance:

- The ignored harness uses one real socket server and one on-disk SQLite database.
- It creates L nodes, K workers per node, and `max_parallel > 1` lanes per worker.
- Mixed priorities and deterministic dependencies drain.
- Optional stall and abandonment faults recover without extra expiry or node staleness.
- Conservation checks terminal state, attempts, lease ownership, duplicate execution, terminal
  lease/ticket events, and dependency event order.
- Every task/server resource is joined or awaited before assertion.

## Task 3 — Expose and document the opt-in lane

Files:

- Modify `justfile`.
- Create `docs/runbooks/distributed-stress-harness.md`.

Interfaces:

```just
# Run the opt-in real-HTTP distributed stress harness.
stress:
    cargo test -p voom-fakes --lib distributed_stress_conserves_every_ticket -- --ignored --nocapture
```

Steps:

1. Add the exact recipe above without adding it to `ci`. Run `just --list`; expect `stress` to be
   listed. Run `just stress` with defaults; expect one ignored test selected and passed.
2. Document every variable from the spec with default/range, a default command, a 2×2×40 quick
   command, a faulted command, deterministic seed reproduction, expected summary fields, timeout
   diagnostics, and the statement that this lane is local opt-in rather than CI.
3. Run the default `just stress` and one faulted cell. Preserve configuration, duration, terminal
   counts, retry count, and findings for the PR body.
4. Run `just fmt-check`, `just lint`, `just check-test-layout`, `just check-paused-time-db`,
   `just check-transaction-openers`, and `cargo test -p voom-fakes`; expect every command exit zero.
5. Commit explicit recipe/runbook paths with `docs: document distributed stress harness`.

Acceptance:

- `just stress` is discoverable, opt-in, and runs the default harness.
- Operators can reproduce a run from its printed seed/configuration.
- The runbook distinguishes in-process abandonment from #606's real subprocess crash scope.

## Final verification

1. Run the controlled-fault bite proof once more if implementation changes touched conservation
   after Task 2.
2. Run `just stress` default and the documented faulted cell; record first-run findings.
3. Run `just ci` bare and require exit zero. Update the observed guardrail duration in the durable
   handoff.
4. Review `git diff main...HEAD` for scope, naming, warnings, cleanup, and exact documentation.

## Durable workflow checkpoint

- Current phase: implementation plan awaiting adversarial review.
- Branch: `feat/stress-harness-581`; base branch: `main`; scope token: `q581-0dae3cc1`.
- ADR review: approved after two passes. Spec review: approved on the operator-authorized
  confirming pass after the five-pass budget stop.
- Suppressions disclosed by spec review: ADR 0094 settles sibling activation fencing and the
  exclusion of real subprocess teardown (#606).
- Open findings and deferrals: none.
