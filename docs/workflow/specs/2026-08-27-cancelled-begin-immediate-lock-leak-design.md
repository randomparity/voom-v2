# Cancelled `BEGIN IMMEDIATE` leaks the write lock — design

Issue: [#592](https://github.com/randomparity/voom-v2/issues/592)
Decision record: [ADR 0087](../../adr/0087-pooled-connections-never-return-inside-a-transaction.md)

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

`sqlx-core-0.8.6/src/pool/connection.rs:275-325` then returns that connection to
the pool. It tests the connection with `ping()`; it never inspects the
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

Three polls lands in the window every time. No load, no throttle, no elapsed-time
assertion.

## Design

One change, in the one place that constructs a pool.

`crates/voom-store/src/pool.rs` `connect_inner` gains an `after_release` hook.
A connection whose `sqlx` transaction depth is non-zero when it is released is
logged at `warn` and closed rather than returned to the idle queue. Closing
releases the write lock; the pool refills on demand.

```rust
.after_release(|conn, _meta| {
    Box::pin(async move {
        let depth = <<Sqlite as Database>::TransactionManager as TransactionManager>
            ::get_transaction_depth(conn);
        if depth > 0 {
            tracing::warn!(depth, "…");
            return Ok(false); // close, do not reuse
        }
        Ok(true)
    })
})
```

The invariant it states — **a connection may not re-enter the pool while a
transaction is open on it** — lives at the pool boundary rather than at each
opener, so it holds for all four of ADR 0086's helpers, for savepoints, and for
any other `sqlx` path with a window not yet found. `connect_inner` is the only
pool constructor in the workspace (`rg SqlitePoolOptions crates` returns one
site), so the hook covers every pool: `ControlPlane`, `HealthPlane`, `init`, and
every test fixture.

Rejected alternatives, with the evidence that sank them, are in ADR 0087.

## Error handling

Reaching the hook's non-zero branch means a cancellation landed inside the
window. It is recovery, not a failure to propagate: there is no caller left to
return an error to. The `warn` line is the report. Nothing else in the request
path changes — a writer that was waiting on the leaked lock proceeds normally
once it is released, and a writer that had already given up returns the same
`DB_UNREACHABLE` it does today.

## Testing

**`crates/voom-store/tests/cancelled_begin_releases_write_lock.rs`** — new
integration test, the regression proof.

- A helper polls a future for exactly *N* wakeup-driven polls and then drops it.
  It does not self-wake: each poll corresponds to one genuine step of progress,
  which is what makes the count reproducible rather than a timing sweep.
- For *N* in 1..=4 it cancels `begin_read_then_write` at that point, then asks a
  second, independent pool on the same database file to execute a write, bounded
  by a short timeout.
- Every *N* must leave the independent write succeeding. Against the unfixed
  code, *N* = 3 blocks until the timeout; that is the assertion that bites.
- The second pool is what makes the assertion honest: a write issued back through
  the *same* pool can be handed the leaked connection and silently join its open
  transaction.

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
  against that database until the process restarts, which is a durable
  denial of service from a single well-timed connection drop. After this change:
  the connection is discarded on release, so the cost is bounded at one
  connection and one `warn` line.
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
