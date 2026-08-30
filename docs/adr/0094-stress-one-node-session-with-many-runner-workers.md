# 0094 — Stress one node session with many runner workers

## Status

Accepted (2026-08-29)

## Context

Issue #581 requires K runner workers on each of L registered remote nodes. The existing
`RemoteSyntheticRunner` activates exactly one worker and processes one lease before returning.
Activating K independent runner instances against the same node would repeatedly replace the node
incarnation, fencing earlier workers instead of creating K concurrent workers.

The stress harness must also model a runner crash without terminating its own process. Real
subprocess termination is separately tracked by #606.

## Decision

Represent each registered node as one remote node session. The session activates the node once
with K worker declarations, then runs `max_parallel` polling lanes for each returned worker under
that shared incarnation. Preserve the existing one-worker `run_once_to_completion` entry point as
a compatibility wrapper over the same request primitives.

Model an in-process crash by recording the acquired ticket/lease/worker, abandoning that lease,
and starting a replacement lane. Drive recovery only through
`ControlPlane::remote_recover` at a timestamp beyond the lease deadline. A selected ticket is
abandoned only on its first attempt, so the two-attempt workload can converge. Configure the
stress lease TTL strictly below the registered node heartbeat TTL; the recovery timestamp crosses
the lease deadline but remains before the node deadline. Require the recovery report to contain no
stale nodes while it expires and requeues the abandoned lease. Use one injected `ManualClock` for
the API control plane and harness. Before advancing it across an abandoned deadline, snapshot all
other held leases. A session-wide recovery gate first stops new polls and waits for every in-flight
acquire request to resolve; only then may the coordinator snapshot. After the advance, heartbeat
those healthy leases through HTTP before their current deadlines, advance again to a timestamp
after the abandoned deadlines but before the refreshed healthy deadlines, recover, and reopen
polling. Require the recovery report's expired set to equal the outstanding abandoned set exactly.

Keep execution observations in a harness-owned in-memory log. Compare it with durable ticket,
lease, and event state after drain; do not add a production observation table or schema.

## Consequences

- The stress topology is exactly L node incarnations and K active workers per node, with more than
  one request lane per worker.
- Activation ownership is explicit, so concurrent runner tasks cannot fence siblings accidentally.
- Crash simulation covers durable lease expiry and retry but not process death, socket teardown,
  or child reaping; #606 owns those behaviors.
- Recovery timing cannot accidentally fence the shared node session: its lease-expiry horizon is
  strictly shorter than the node-staleness horizon and the report asserts that distinction.
- A global expiry scan cannot consume contemporaneous healthy work: the coordinator heartbeats
  every observed non-abandoned held lease at the advanced domain time and rejects extra expiries.
- The recovery gate closes the snapshot race: no acquisition can appear between the healthy-lease
  snapshot and the expiry scan.
- Existing single-runner tests and callers remain source-compatible.
- The execution log is independent enough to detect cross-worker duplicate execution but is lost
  if the test process itself exits, which is acceptable for an in-process opt-in test.

## Considered & rejected

- **Activate K independent runners against each node.** verified: activation creates a new
  incarnation and atomically replaces declared workers in
  `crates/voom-control-plane/src/cases/execution/remote_execution/activation.rs`; repeated
  activation would fence earlier runners rather than create the requested topology.
- **Use K total runners distributed over L nodes.** judgment: the operator explicitly selected K
  runners on every node, so this would test a weaker topology than the frozen scope.
- **Call `std::process::exit` from the in-process runner.** verified:
  `crates/voom-fakes/src/bin/chaos_worker.rs:634` terminates the whole hosting process; in the
  library harness that would kill the test before conservation could be checked. Real subprocess
  coverage is tracked by #606.
- **Mark abandoned leases expired with direct SQL.** judgment: bypassing
  `ControlPlane::remote_recover` would test storage mutation rather than the production recovery
  use case and would omit its events.
- **Use equal node and lease TTLs.** verified: `ControlPlane::remote_recover` calls
  `mark_stale_nodes` before `expire_due` in
  `crates/voom-control-plane/src/cases/execution/remote_execution/recover.rs`, so crossing equal
  deadlines can fence the whole shared node session before it requeues the selected lease.
- **Advance recovery time without refreshing other held leases.** verified: `expire_due` scans
  every held lease whose deadline is before the supplied time in
  `crates/voom-store/src/repo/execution/leases.rs`; a global future timestamp is not scoped to the
  intentionally abandoned lease.
- **Snapshot healthy leases while polling continues.** judgment: an acquire completing after the
  snapshot would miss the heartbeat set and become an unintended expiry at the advanced time.
- **Heartbeat healthy leases only after crossing the abandoned deadline.** verified:
  `SqliteLeaseRepo::heartbeat_in_tx` rejects `expires_at <= now` in
  `crates/voom-store/src/repo/execution/leases.rs`, so equal-TTL healthy leases must be refreshed
  before the clock crosses their original deadline.
- **Persist execution observations in SQLite.** judgment: a new schema and production write path
  are unnecessary for test-only independent observation and outside issue #581's surface.
- **Keep only the existing one-lease runner.** verified: `run_once_to_completion` returns after one
  leased outcome in `crates/voom-fakes/src/remote_runner.rs:113-137`, so it cannot exercise a
  sustained K×L topology or a draining backlog.
