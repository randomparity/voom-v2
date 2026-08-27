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
transaction depth and never rolls back, and `test_before_acquire` is that same
ping (`options.rs:149`), so nothing quarantines it either. The connection sits in
the idle queue holding the write lock indefinitely.

Writers then split three ways. On another connection: wait out the full 30s
`busy_timeout`, fail with `DB_UNREACHABLE` → 503 — precisely the request log
captured on the issue. Handed the poisoned connection, a `BEGIN IMMEDIATE` opener
fails at once with `InvalidSavePointStatement`
(`sqlx-sqlite-0.8.6/src/connection/worker.rs:210-222`). Handed it, a *deferred*
opener succeeds — it opens `SAVEPOINT _sqlx_savepoint_1` inside the abandoned
transaction and its commit issues `RELEASE SAVEPOINT`
(`sqlx-core-0.8.6/src/transaction.rs:277-289`), so the caller is told the write
committed while it stays inside a transaction nobody will ever commit. Closing
that connection rolls it back. The defect is a stall **and** a silent-lost-write
path — worth stating on the issue, because an operator who hit the hang may need
to audit rather than just restart.

The plain-`BEGIN` path does not leak: the worker thread detects that its
acknowledgement could not be delivered and rolls back immediately
(`sqlx-sqlite-0.8.6/src/connection/worker.rs:234-252`). There is no second await
point to vanish in. So the affected openers are exactly
`voom_store::tx::begin_read_then_write` and `begin_serialized_read`
(`crates/voom-store/src/tx.rs:40,105`) — ADR 0086's two `BEGIN IMMEDIATE`
helpers.

### Why cancellation happens in production

One source is systematic rather than incidental: `bounded_router` installs a
`TimeoutLayer` at `request_processing` on every route
(`crates/voom-api/src/server.rs:348-351`, `crates/voom-api/src/config.rs:12,111`
— 30s), and it drops the handler future. `axum`/`hyper` drop one on client
disconnect too, and the node agent disconnects when it is fenced and when
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

Two files: the openers that can leak, and the one production caller that opens the
same shape without going through them.

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
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(
        async move {
            let opened = pool.begin_with(statement).await;
            if let Err(unsent) = sender.send(opened) {
                if unsent.is_ok() {
                    tracing::warn!(
                        context,
                        "transaction open completed after its caller was cancelled; \
                         rolling back"
                    );
                }
                // Dropping `unsent` drops the Transaction, which queues the
                // ROLLBACK that releases the write lock.
            }
        }
        .instrument(tracing::Span::current()),
    );
    receiver
        .await
        .map_err(|e| VoomError::database_context(context, e))?
        .map_err(|e| VoomError::database_context(context, e))
}
```

A spawned task outlives the future that spawned it, so a cancelled caller leaves
a task that still constructs the `Transaction` and then drops it — and
`Transaction::drop` queues the `ROLLBACK` the leak was missing. The `oneshot` is
what makes that visible: a failed `send` is the only place the detached path can
tell it is orphaned, and without it a pool slot held by a request answered thirty
seconds ago is indistinguishable from one held by a live request. The
`Instrument` keeps the open inside the caller's span, which `tokio::spawn` does
not inherit.

`voom-store` gains `tokio`'s `rt` feature. The openers now require a `tokio`
runtime — `tokio::spawn` panics without one, and during thread-local teardown
(`tokio-1.53.1/src/task/spawn.rs:211-214`). That is not a new requirement for the
pool: `PoolConnection::drop` already routes through
`sqlx-core-0.8.6/src/rt/mod.rs:61-79`, which panics with "this functionality
requires a Tokio context" when `Handle::try_current()` fails.

`begin_write_first` and `begin_read_only` are unchanged. A deferred `BEGIN` takes
no write lock, and the worker's rendezvous acknowledgement
(`sqlx-sqlite-0.8.6/src/connection/worker.rs:81,528-538,234-252`) already rolls
back a cancelled open, so there is no hazard there to wrap.

`crates/voom-store/src/init.rs` — this supersedes one sentence of ADR 0068,
whose Decision names `conn.begin_with("BEGIN IMMEDIATE")` as the mechanism; its
substance (write lock up front, held across the whole run, per-migration opens
nesting as savepoints) is unchanged. `run_migrations_on` currently does
`pool.acquire()` and then `conn.begin_with("BEGIN IMMEDIATE")` at `:54-56`, which
is the same two-step path with the same window. It moves to
`begin_read_then_write(pool, "acquire migration write lock")`. The explicit
`pool.acquire()` exists only so the connection can be dropped before
`probe_schema` runs against the pool; a pool-level `Transaction<'static>` owns
its connection and returns it on `commit` or `drop`, so both `drop(conn)` lines
go away with it. `MIGRATOR.run_direct(&mut *tx)` is unchanged.

**Why the openers and not the pool.** `scripts/check-transaction-openers.sh`
already fails a build in which production code calls `pool.begin*()` outside
`voom-store/src/tx.rs`, so the openers reach every transaction that goes through
them, whichever pool it runs against. That last part matters:
`rg -n 'SqlitePoolOptions' --type rust` finds five pool construction sites, four
of them test modules, and
`crates/voom-control-plane/src/cases/execution/leases_test.rs:223` hands its own
pool to production `ControlPlane` code. A fix expressed as a pool option would
miss it.

That check is not full coverage. Its rule constrains the receiver to `(?i)pool`
so a savepoint cannot match, which also makes `conn.begin_with` invisible —
`./scripts/check-transaction-openers.sh crates` reports `OK (378 files)` with
`init.rs:55` present. This change removes the only production instance; closing
the guardrail gap so a future one is caught is deferred to
`docs/debt/0005-connection-level-custom-begins-are-unguarded.md`.

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
- the spawned task is dropped without sending — the `oneshot` receiver returns
  `RecvError`, and there is no transaction to lose because the same drop rolls
  one back if it exists.

The residual the detach adds is bounded and stated in ADR 0087: at most
`max_connections` detached openers at once, each terminating within
`LOCK_WAIT_BUDGET`, so a burst degrades and drains rather than wedging.

## Testing

**`crates/voom-store/tests/cancelled_begin_releases_write_lock.rs`** — new
integration test, the regression proof.

- A helper polls a future for exactly *N* wakeup-driven polls and then drops it.
  It does not self-wake, so each poll corresponds to one genuine step of
  progress; that is what makes the count reproducible rather than a timing sweep.
- For *N* in 1..=8 it cancels the open at that point, then asks a second,
  independent pool on the same database file to execute a write, bounded by a
  short timeout. The second pool is what makes the assertion honest: a write
  issued back through the *same* pool can be handed the leaked connection and
  silently join its open transaction.
- **Two sweeps.** Through `begin_read_then_write`, the independent write must
  succeed at every *N*. Through a bare `pool.begin_with("BEGIN IMMEDIATE")`
  written in the test itself, it must fail at some *N* — that is the positive
  control, and it is what keeps the first sweep honest. Without it the test is
  green whether or not the sweep still straddles a vulnerable window, so a
  dependency bump that moved the window outside 1..=8 would leave an assertion
  that passes while proving nothing.
- Test files are exempt from `check-transaction-openers.sh`
  (`! -name '*_test.rs' ! -path '*/tests/*'`), so the control's raw
  `pool.begin_with` is allowed where it lives.
- Measured on the pinned toolchain: the unfixed opener leaks at *N* = 3 and only
  at 3; the fixed one completes from *N* = 2. Both counts are properties of the
  pinned `sqlx` 0.8.6, `flume`, and `tokio`, which is why the sweep is a range
  and the control is what asserts the range is still the right one.

The test is bounded by `tokio::time::timeout` only to fail fast; it asserts an
observable state transition (a write lock that can be taken), not an elapsed
duration.

**`crates/voom-store/tests/`** existing init/migration coverage exercises the
`init.rs` change; no new test is written for it, since the behaviour is
unchanged and the shape is what moved.

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
  precondition for un-redding `main`. Carrying a `[patch.crates-io]` fork instead
  is rejected in ADR 0087.
- **Extending `check-transaction-openers.sh` to catch connection-level custom
  begins.** `scripts/` is outside the frozen surface, and the rule change is not
  a one-liner. Deferred with an owner:
  `docs/debt/0005-connection-level-custom-begins-are-unguarded.md`.
