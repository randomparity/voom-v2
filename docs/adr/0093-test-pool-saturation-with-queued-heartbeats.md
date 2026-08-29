# 0093 — Test pool saturation with queued heartbeats

## Status

Accepted (2026-08-29)

## Context

The on-disk SQLite pool has eight connections. A `BEGIN IMMEDIATE` writer can occupy one while
seven more write transactions hold the remaining connections waiting for SQLite's write lock;
later callers then wait for a pooled connection. Issue #580 requires a deterministic regression
test for that saturation path and for recovery after the writer releases.

Testing a mix of acquisition, heartbeat, and settlement calls would add unrelated domain fixtures
and outcomes to a test whose load-bearing behavior is connection admission under a held write
lock. Heartbeat already represents a lease holder proving liveness, begins a write-first
transaction, and has an observable durable epoch and deadline.

## Decision

Test saturation in the lease repository with twelve concurrent heartbeats against one held lease.
Hold a `BEGIN IMMEDIATE` transaction open, release all heartbeat tasks together, and wait until the
pool reports all eight connections checked out. Require the caller count to exceed the seven
connections available beside the held writer; together with no completed heartbeat, that proves at
least one caller is waiting for pool admission. Capture any failed observation without asserting,
commit the writer, join every task, and only then report the observation or task failure.
Capture the commit result too: consume the transaction, join every task regardless of that result,
and only then assert that commit succeeded.

Use one fixed domain timestamp before the lease deadline for every heartbeat. After the tasks
finish, assert the lease remains held, its deadline was not shortened, its last-heartbeat time is
the supplied timestamp, and its epoch advanced once per caller. A final uncontended heartbeat
proves the pool and repository converge after saturation.

Keep the test in the default suite and use real Tokio time. Bound only the test's observation of
pool saturation; do not shorten production lock or pool-acquire budgets.

## Consequences

- The test exercises both occupied SQLite connections and callers queued at the pool boundary.
- Failure diagnostics do not strand a held writer or detached heartbeat tasks.
- Lease liveness is decided by the supplied domain timestamp, not by time spent waiting in the
  pool, so transient queueing cannot manufacture expiry.
- The test proves the success arm of issue #580's succeed-or-typed-error contract by releasing the
  writer well inside both production budgets.
- This does not benchmark throughput or cover every lease operation under saturation.

## Considered & rejected

- **Mix claim, heartbeat, and settlement calls in one test.** judgment: their domain conflicts and
  fixture setup would obscure whether a failure came from pool saturation or ordinary lease
  transition rules; heartbeat alone reaches both contention layers and directly covers liveness.
- **Hold the writer until `POOL_ACQUIRE_BUDGET` elapses.** judgment: a 45-second default test would
  violate issue #580's default-suite budget and prove only the already-configured timeout.
- **Lower timeouts through a test-only pool constructor.** judgment: new configuration surface
  would test a different pool from production and is unnecessary for the recovery contract.
- **Use sleeps to infer saturation.** verified: `sqlx::Pool::size` and `Pool::num_idle` expose the
  live connection counts in sqlx 0.8.6 (`Cargo.lock` and the dependency source), so the test can
  observe full checkout directly instead of guessing from elapsed time.
- **Keep the existing lower-cardinality contention tests only.** verified: the on-disk pool selects
  eight connections in `crates/voom-store/src/pool.rs` on `main` at
  `22d61c6680af37c33d57464012a9245811300a3c`, while existing tests do not assert a fully checked-out
  pool; they cannot establish caller queueing at the pool boundary.
