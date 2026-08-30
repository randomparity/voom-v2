# Distributed stress-harness design

## Scope and goal

Issue #581 requires an opt-in test that runs the distributed execution design at realistic shape:
many tickets, K runner workers on each of L remote nodes, real HTTP, one API server, and one
on-disk SQLite database. The test must drain a mixed backlog and prove conservation across durable
tickets, leases, events, and an independent execution log. It is a correctness stress test, not a
throughput benchmark.

The governing topology and recovery decision is
[ADR 0094](../../adr/0094-stress-one-node-session-with-many-runner-workers.md).

Production ticket, lease, API, schema, and protocol behavior do not change. Scheduled CI belongs
to #582, ENOSPC belongs to #583, and real subprocess termination belongs to #606.

## Configuration

`just stress` runs one ignored `voom-fakes` library test. The harness reads these environment
variables once, validates them before creating the database, and reports invalid values with the
variable name and accepted range:

| Variable | Default | Range / meaning |
|---|---:|---|
| `VOOM_STRESS_NODES` | 4 | 1–32 registered remote nodes |
| `VOOM_STRESS_RUNNERS_PER_NODE` | 8 | 1–32 worker declarations per node |
| `VOOM_STRESS_MAX_PARALLEL` | 2 | 2–16 concurrent lanes per worker |
| `VOOM_STRESS_TICKETS` | 1000 | 1–10000 seeded tickets |
| `VOOM_STRESS_DEPENDENCY_PERCENT` | 20 | 0–90; tickets after the first may depend on an earlier ticket |
| `VOOM_STRESS_STALL_PERCENT` | 0 | 0–25; selected executions pause before settlement |
| `VOOM_STRESS_CRASH_PERCENT` | 0 | 0–25; selected first attempts abandon their lease |
| `VOOM_STRESS_SEED` | 581 | deterministic workload/fault selection seed |
| `VOOM_STRESS_DRAIN_SECONDS` | 120 | 1–600 wall-clock deadline |

The two fault percentages must sum to at most 25. The default exercises contention without faults;
operators opt into recovery behavior explicitly. Test configuration is echoed to stderr so a failed
run can be repeated exactly.

## Components

### Remote node session

Extend `RemoteSyntheticRunner` with a many-worker node-session entry point while preserving
`run_once_to_completion` for existing callers. One session performs exactly one node activation
whose declaration contains K uniquely named workers. The activation response supplies the K
worker IDs. Each worker starts `max_parallel` independent polling lanes using the same node
incarnation and worker identity, fresh idempotency keys per request, and the existing HTTP methods.

An execution record contains `ticket_id`, `lease_id`, `worker_id`, a harness-owned acquisition
ordinal, and one observed action:
completed, failed, stalled-then-completed, or abandoned. Records are appended to the shared
execution log only after acquisition and before the selected action, so an abandoned lease is still
independently visible. One `Arc<Mutex<ExecutionState>>` is shared across all L node sessions. Its
single critical section increments an ordinal keyed by `TicketId`, selects the fault from that
ordinal, and appends the record. The public HTTP acquire payload supplies `ticket_id` but not a
durable attempt count, so the harness never claims to decode one. This log is test observation
only; it is never persisted into VOOM.

### Deterministic fault policy

Fault selection is a pure function of the seed, ticket ID, and harness acquisition ordinal. Stall sleeps for a bounded
duration shorter than the lease TTL, heartbeats once after the stall, and settles normally. Crash
is applied only to ordinal 1 of a selected ticket: the lane records `abandoned` and returns without
heartbeat or settlement. Its supervisor immediately starts a replacement lane for the same worker.
Stress leases use a one-second TTL while registered nodes use a 60-second heartbeat TTL. The
harness and API share an injected `ManualClock`; Tokio time remains real. When abandonment is
observed, the coordinator closes a session-wide recovery gate. The gate is a Tokio asynchronous
read/write lock: a lane holds a read permit across each HTTP request that mutates lease state —
acquire, lease heartbeat, complete, and fail — while local fake-provider dispatch runs without a
permit. Recovery takes and holds the write permit through the snapshot, heartbeat refresh, and
`remote_recover`. Acquiring the write permit atomically blocks new mutations and waits for every
admitted mutation to resolve before the coordinator snapshots every other held execution. For
one-second leases acquired at `T0`, the coordinator advances to
`T0 + 500ms`, heartbeats healthy leases so their deadlines become `T0 + 1500ms`, then advances to
`T0 + 1250ms`, calls `ControlPlane::remote_recover`, and reopens the gate. The abandoned leases
are overdue and the refreshed leases are not;
both times remain before the node deadline. Every healthy heartbeat must succeed before recovery.
The report must contain no stale nodes and its expired-lease set must equal the outstanding
abandoned set exactly. A focused test holds one healthy lease beside one abandoned lease,
deliberately blocks another acquire during recovery preparation, and proves the gate waits for it,
the healthy heartbeat succeeds, and only the abandoned lease expires. A second admission test
holds the recovery write permit first and proves a lane cannot begin its HTTP acquire until the
permit is released. A third interleaving finishes local dispatch after the snapshot and proves its
completion request waits until recovery releases the write permit.

The harness therefore tests the same durable recovery path as a lost process while deliberately
excluding OS process and socket teardown, which #606 owns.

### Workload generator

Create `VOOM_STRESS_TICKETS` transcode-video tickets using payloads already accepted by the fake
provider. Priorities cycle through a fixed mixed set. For each ticket after the first, the seeded
generator optionally adds one dependency on an earlier ticket. After all edges exist, call
`mark_ready_if_unblocked` for every ticket; roots become ready and dependents remain pending until
their prerequisite succeeds. Every ticket uses `max_attempts = 2`, permitting exactly one recovery
after an injected abandonment.

### Drain coordinator

Start the API server before registering nodes. Register L remote nodes, create one session per node,
and start all sessions together behind a barrier. The coordinator periodically checks typed ticket
state and invokes remote recovery after abandoned leases have crossed their domain deadline. Drain
finishes only when every seeded ticket is terminal and no lease is held. A wall-clock deadline
aborts all session tasks, joins them, shuts down the server, and reports state counts plus the last
recovery report.

All task handles are retained. Assertions run only after sessions are stopped, every task is joined,
and the server handle is aborted and awaited, so a failing assertion cannot leak background work.

## Conservation contract

At drain, one `assert_conservation` function receives the seeded ticket IDs, typed ticket rows,
typed lease rows, typed event pages, and merged execution records. It accumulates all mismatches
and reports them together.

It requires:

1. Every seeded ticket exists once and is `succeeded` or `failed`; no seeded ticket remains pending,
   ready, or leased.
2. Each ticket has exactly one terminal ticket event matching its durable terminal state.
3. No lease remains held. Each execution record names a durable lease for the same ticket and
   worker.
4. Each ticket's durable `attempt` equals its number of `lease.acquired` events, its distinct
   execution-log lease count, and the highest contiguous harness acquisition ordinal. The sum of
   ticket attempts equals all three global counts.
5. A ticket may have multiple attempts but cannot have more than one non-abandoned execution, and
   no lease may appear under two workers. This distinguishes a legitimate retry from concurrent
   duplicate execution.
6. Every acquired lease has exactly one terminal lease event: released for a completed/failed
   action or expired for an abandoned action.
7. For every configured dependency edge, the prerequisite's `ticket.succeeded` event ID precedes
   the dependent's first `lease.acquired` event ID. A final-state check also requires the
   prerequisite to be succeeded. A synthetic observation test swaps those two event IDs and must
   fail with a dependency-order diagnostic.

Event reads paginate until `next_cursor` is absent; a fixed page size must not silently truncate
the evidence. Numeric conversions from SQLite-backed types remain checked through typed repository
decoders.

## Failure contract

- Invalid configuration fails before server/database setup.
- A node activation, HTTP request, runner task, recovery call, or repository read failure stops
  new polling, joins all tasks, cleans up the server, and returns the operation plus identity.
- The wall-clock deadline reports ticket-state counts, held leases, execution-record count, and
  configured seed.
- Conservation mismatches are deterministic for a fixed seed and sorted by ticket/lease identity.
- The harness uses real Tokio time around SQLite. Domain expiry is advanced only by the explicit
  timestamp passed to `remote_recover`.

## Verification

1. Add focused unit tests for configuration bounds, deterministic fault selection, and
   conservation on a small synthetic observation set.
2. Add an ignored real-socket test and prove it red before the many-worker node-session API exists.
3. Run a small no-fault stress cell, then the documented default, then a faulted cell with both
   stall and crash enabled.
4. Prove the conservation assertion bites by temporarily duplicating one execution record before
   `assert_conservation`; require the test to fail with the duplicate-execution diagnostic, revert
   the fault, and rerun green.
5. Run `just fmt-check`, `just lint`, `just check-test-layout`, and the focused `voom-fakes` tests,
   followed by `just ci` before delivery.

The first real default and faulted runs, their durations, configuration, state counts, retry count,
and any findings are recorded in the pull-request body.

## Durable workflow checkpoint

- Branch: `feat/stress-harness-581`
- Base branch: `main`
- Scope token: `q581-0dae3cc1`
- Architecture: host x86_64; no target architecture declared; relationship
  `no-target-declared`
- Guardrails: `cargo test -p voom-fakes`; `just stress`; `just fmt-check`; `just lint`;
  `just check-test-layout`; `just test`; `just ci`
- Open findings and deferrals: none. Real subprocess termination is tracked by #606.
