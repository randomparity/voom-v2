# Cancelled `BEGIN IMMEDIATE` leaks the write lock — design

Issue: [#592](https://github.com/randomparity/voom-v2/issues/592)
Decision record: [ADR 0087](../../adr/0087-cancellation-safe-begin-immediate.md)

## Goal

Stop a cancelled control-plane request from leaving `SQLite`'s write lock held by
an idle pooled connection, so that graceful node-agent shutdown reaches its
durable `Retired` write instead of exhausting its retry budget against a
deadlocked control plane.

## Problem

`live_agent_fences_prior_incarnation_and_retires_orderly` hangs in the `coverage`
job until its 30s guard expires. Issue #592 established by `gdb` and `/proc/*/io`
that the control plane is deadlocked, not slow: two `SQLite` connections spin in
`btreeBeginTrans(wrflag=1)`, two `sqlx-sqlite-worker` threads are idle in
`flume::recv`, the tokio thread is in `ep_poll`, and there is no read or fsync
traffic. Something holds the write lock while issuing no statements.

### Root cause

`sqlx` 0.8.6 opens a custom-statement transaction in two steps
(`sqlx-sqlite-0.8.6/src/transaction.rs:19-30`):

1. run the statement on the worker thread — `BEGIN IMMEDIATE` takes the write lock;
2. `await conn.lock_handle()` and confirm `in_transaction()`.

The `Transaction` value, whose `Drop` queues the rollback, is constructed only
after step 2. A future dropped between the two leaves the lock held by a
connection that no value owns.

`sqlx-core-0.8.6/src/pool/connection.rs:275-328` then returns that connection to
the pool. It tests the connection with `ping()` at `:314`; it never inspects the
transaction depth and never rolls back. The connection sits in the idle queue
holding the write lock indefinitely. Every subsequent writer waits out the full
30s `busy_timeout` and fails with `DB_UNREACHABLE` → 503, which is precisely the
request log captured on the issue.

The plain-`BEGIN` path does not leak: the worker thread detects that its
acknowledgement could not be delivered and rolls back immediately
(`sqlx-sqlite-0.8.6/src/connection/worker.rs:234-252`). There is no second await
point to vanish in. So the affected openers are exactly
`voom_store::tx::begin_read_then_write` and `begin_serialized_read`
(`crates/voom-store/src/tx.rs:40,105`) — ADR 0086's two `BEGIN IMMEDIATE`
helpers.

### Why cancellation happens in production

`axum`/`hyper` drop a handler future when the client disconnects. The node agent
disconnects on two ordinary paths: when it is fenced and exits, and when
`REQUEST_TIMEOUT` (30s, `crates/voom-node-agent/src/client.rs:32`) fires. This is
not a test-only condition — issue #592 records the production consequence: an
agent that cannot deactivate within any reasonable SIGTERM grace is SIGKILLed and
its incarnation is never retired.

### Reproduction

Deterministic, established during design. Poll `begin_read_then_write` for an
exact number of genuine wakeup-driven polls, drop it, then ask an **independent**
pool on the same file to write:

| polls before drop | begin completed | independent write |
|---:|---|---|
| 1 | no | ok |
| 2 | no | ok |
| 3 | no | **blocked** |
| 4 | yes | ok |

Three polls lands in the window, and it is the only value that does: two is
before the write lock is taken, four is after the `Transaction` is constructed.
No load, no throttle, no elapsed-time assertion. Measured on Fedora / Linux
7.1.8 / rustc 1.95.0, identically in debug and `--release`; the count is a
property of the pinned `sqlx` 0.8.6, `flume`, and `tokio` versions rather than a
universal, which is why the regression test sweeps a range instead of pinning 3.

## Design

One change, in the two openers that can leak.

`crates/voom-store/src/tx.rs` — `begin_read_then_write` and
`begin_serialized_read` drive `pool.begin_with("BEGIN IMMEDIATE")` on a detached
`tokio` task and await its `JoinHandle`:

```rust
async fn begin_detached(
    pool: &SqlitePool,
    statement: &'static str,
    context: &'static str,
) -> Result<Transaction<'static, Sqlite>, VoomError> {
    let pool = pool.clone();
    tokio::spawn(async move { pool.begin_with(statement).await })
        .await
        .map_err(|e| VoomError::database_context(context, e))?
        .map_err(|e| VoomError::database_context(context, e))
}
```

Dropping a `JoinHandle` detaches the task rather than aborting it. A cancelled
caller therefore leaves a task that still constructs the `Transaction` and then
drops it — and `Transaction::drop` queues the `ROLLBACK` the leak was missing.
`voom-store` gains `tokio`'s `rt` feature; every caller is already inside a
runtime.

`begin_write_first` and `begin_read_only` are unchanged. A deferred `BEGIN` takes
no write lock, and the worker's rendezvous acknowledgement
(`sqlx-sqlite-0.8.6/src/connection/worker.rs:81,528-538,234-252`) already rolls
back a cancelled open, so there is no hazard there to wrap.

**Why the openers and not the pool.** `scripts/check-transaction-openers.sh`
already fails a build in which production code opens a pool-level transaction
outside `voom-store/src/tx.rs`, so fixing the openers covers every production
transaction — and it covers them whichever pool they run against. That last part
matters: `rg -n 'SqlitePoolOptions' --type rust` finds five pool construction
sites, four of them test modules, and
`crates/voom-control-plane/src/cases/execution/leases_test.rs:223` hands its own
pool to production `ControlPlane` code. A fix expressed as a pool option would
miss it.

Rejected alternatives — including the pool-level `after_release` guard this
design originally proposed, and the evidence that it destroys `:memory:`
databases — are in ADR 0087.

## Error handling

A cancelled caller has nobody left to return an error to; the detached task's
`Transaction` is simply dropped and rolled back. Nothing in the request path
changes: a writer that was waiting on the leaked lock now proceeds, and a writer
that had already given up returns the same `DB_UNREACHABLE` it does today.

Two new error paths, both mapping to `VoomError::Database` exactly as the current
openers do:

- the spawned task panics — surfaced through `JoinError`;
- the spawn is attempted during runtime shutdown, so the task is cancelled before
  it opens anything — also a `JoinError`, and no transaction to lose.

## Testing

**`crates/voom-store/tests/cancelled_begin_releases_write_lock.rs`** — new
integration test, the regression proof.

- A helper polls a future for exactly *N* wakeup-driven polls and then drops it.
  It does not self-wake, so each poll corresponds to one genuine step of
  progress; that is what makes the count reproducible rather than a timing sweep.
- For *N* in 1..=8 it cancels `begin_read_then_write` at that point, then asks a
  second, independent pool on the same database file to execute a write, bounded
  by a short timeout.
- Every *N* must leave the independent write succeeding. Against the unfixed
  code, *N* = 3 blocks until the timeout; that is the assertion that bites.
- The second pool is what makes the assertion honest: a write issued back through
  the *same* pool can be handed the leaked connection and silently join its open
  transaction.
- **The sweep asserts that it brackets completion** — at least one *N* where the
  open had not finished and at least one where it had. The window is one poll
  wide and its position depends on the pinned `sqlx`, `flume`, and `tokio`
  versions; the bracket is what turns a version bump that moves the window out of
  range into a red test rather than a green test that proves nothing.

The test is bounded by `tokio::time::timeout` only to fail fast; it asserts an
observable state transition (a write lock that can be taken), not an elapsed
duration.

**Unchanged:** `crates/voom-node-agent/tests/lifecycle.rs`. `HANG_GUARD` stays at
30s. That test is the end-to-end signal for this defect and it stays tight, which
is the whole reason it caught this.

**Unchanged:** `crates/voom-node-agent/tests/budget_ladder.rs`. No budget moves.

## Availability boundary

The defect is remotely triggerable, so it is worth stating who can trigger it and
what the change does to that.

- **Boundary.** The control plane's HTTP surface (`voom-api`), where a caller's
  transport-level disconnect propagates into a dropped handler future. This
  design adds no boundary and widens none; it removes a consequence of an
  existing one.
- **Actor.** An authenticated node agent holding a node token
  (`crates/voom-control-plane/src/node_auth.rs` governs admission). Not anonymous
  — a disconnect only reaches a handler that authentication already admitted.
  Every deployment's agents are in this set, and an agent does not have to be
  malicious to trigger it: being fenced, or hitting its own 30s per-attempt
  timeout, is enough.
- **Control.** Today: none — a disconnect inside the window wedges every writer
  against that database until the process restarts, which is a durable denial of
  service from a single well-timed connection drop. After this change: the
  transaction is opened and immediately rolled back, so the cost is bounded at
  one pooled connection held for the duration of one lock wait.
- **Out of scope.** Rate-limiting or authenticating disconnects, and any other
  cancellation-triggered resource exhaustion (file handles, worker processes,
  leases). Not addressed here and not claimed to be.

## Out of scope

- **Re-sizing the control-path budget ladder.** `budget_ladder.rs` records that
  shrinking the server-side budgets so a whole call fits inside one attempt
  "belongs with the #592 fix". It does not land here: with the leak gone, a lock
  wait is the transient contention the 30s `busy_timeout` was sized for, and
  re-sizing the ladder is a separate change against separate evidence. Flagged in
  the pull request.
- **Issue #452.** The same defect seen from the agent side. This change may make
  it resolvable; closing it is its owner's call.
- **The lock-free opener ring buffer** issue #592 proposes as the next
  diagnostic. Its question — which transaction holds the lock — is answered.
- **Reporting the window upstream to `sqlx`.** Worth doing, and not a
  precondition for un-redding `main`.
