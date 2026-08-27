# 0087 — Opening a `BEGIN IMMEDIATE` transaction is cancellation-safe

## Status

Accepted (2026-08-27)

## Context

Issue #592: `main`'s `coverage` job goes red intermittently because
`live_agent_fences_prior_incarnation_and_retires_orderly` hangs until its 30s
guard expires. The investigation recorded on that issue traced it to a
control-plane write-lock deadlock — `gdb` against a live hang shows two `SQLite`
connections spinning in `btreeBeginTrans(wrflag=1)`, two `sqlx-sqlite-worker`
threads **idle** in `flume::recv`, the tokio thread in `ep_poll`, and
`read_bytes: 0`. Something holds the write lock while issuing no statements. The
remaining unknown was which transaction that is.

It is not a transaction. It is a connection sitting idle **in the pool** with a
`BEGIN IMMEDIATE` still open on it.

`sqlx` 0.8.6 opens a custom-statement transaction in two steps
(`sqlx-sqlite-0.8.6/src/transaction.rs:19-30`): it runs the statement on the
worker thread, then awaits `conn.lock_handle()` to confirm the connection really
entered a transaction. The write lock is taken at the end of the first step, and
the `Transaction` value — the thing whose `Drop` queues the rollback — is
constructed only after the second (`sqlx-core-0.8.6/src/transaction.rs:105-112`).
Between them the future has an await point. Drop it there and the lock is held by
a connection that no value owns.

The pool does not recover it. `sqlx-core-0.8.6/src/pool/connection.rs:275-328`
returns a connection by testing it with `ping()` at `:314`; it never inspects the
transaction depth and never rolls back. So the connection goes back into the idle
queue holding the write lock, and stays there. Every later writer burns the full
30s `busy_timeout` and fails; the node agent's `deactivate` exhausts its five
attempts (`voom-node-agent/src/client.rs:25`) and the incarnation is never marked
`Retired`.

Only the custom-statement path leaks. For a plain `BEGIN`, the acknowledgement is
a rendezvous send (`sqlx-sqlite-0.8.6/src/connection/worker.rs:81,528-538`) that
completes only once the receiver has taken the value — so a caller that vanished
before receiving makes the send fail, and the worker rolls the transaction back on
the spot (`worker.rs:234-252`). There is no second await point for a caller to
vanish in. In this repository the custom-statement path is exactly
[ADR 0086](0086-transaction-openers-are-named-helpers.md)'s `begin_read_then_write`
and `begin_serialized_read`.

Cancellation is ordinary here, not exotic. `axum` drops a handler future when the
client disconnects, and the node agent disconnects both when it is fenced and
when its 30s per-attempt timeout fires.

## Decision

**Opening a `BEGIN IMMEDIATE` transaction is cancellation-safe: the open runs to
completion whether or not its caller is still waiting.** The two openers that
issue a custom statement drive `pool.begin_with` on a detached `tokio` task and
await its `JoinHandle`. Dropping a `JoinHandle` detaches the task rather than
aborting it, so a cancelled caller leaves a task that still constructs the
`Transaction` — and then drops it, which is the rollback the leak was missing.

The two deferred-`BEGIN` openers are unchanged. They cannot leak the write lock:
a deferred `BEGIN` takes no lock, and the worker's rendezvous acknowledgement
already rolls back a cancelled open. A wrapper there would fire where the hazard
is not, which is the objection ADR 0086 records against rules that fire
everywhere.

Two sub-decisions follow.

**The choke point is ADR 0086's, reused.** `scripts/check-transaction-openers.sh`
already fails a build in which production code opens a pool-level transaction
anywhere but `voom-store/src/tx.rs`, so fixing the openers covers every
production transaction without a second guardrail. It covers them independently
of which pool they run against, which matters because four test modules construct
their own pools and one of them
(`crates/voom-control-plane/src/cases/execution/leases_test.rs:223`) hands one to
production `ControlPlane` code.

**The regression test sweeps the window rather than pinning it.** Cancelling the
opener after exactly *N* wakeup-driven polls is deterministic, but which *N*
lands between the two steps is a property of the pinned `sqlx`, `flume`, and
`tokio` versions — measured here as 3 unfixed, and one poll wide. So the test
cancels at every *N* from 1 to 8 and requires an independent pool to keep taking
the write lock at all of them, and it asserts that the sweep **brackets**
completion: at least one *N* where the open had not finished and at least one
where it had. A version bump that shifts the window keeps it inside the sweep; a
bump that shifts it outside fails the bracket assertion instead of leaving a
green test that proves nothing.

## Consequences

The deadlock's window closes. A cancellation inside it now costs one transaction
opened and immediately rolled back, instead of stalling every writer against the
database until the process restarts.

A cancelled caller's detached task keeps its pooled connection until
`BEGIN IMMEDIATE` returns — up to `LOCK_WAIT_BUDGET` under contention — and then
releases it. That is not a regression: before this change the connection was
released earlier but poisoned, which is the defect. It does mean a burst of
cancellations can hold connections it previously discarded, bounded by
`max_connections` and the same lock wait every writer already pays.

Every `begin_read_then_write` and `begin_serialized_read` now costs one task
spawn on top of the channel round trip to the `sqlite` worker it already paid.
`voom-store` gains `tokio`'s `rt` feature, and the openers now require a `tokio`
runtime — every caller is already inside one, and a spawn attempted during
runtime shutdown surfaces as `VoomError::Database` rather than a lost
transaction.

The deferred openers stay safe by way of `sqlx` worker behaviour this record
cites but does not control. If that behaviour changed, a cancelled deferred open
would return a connection at depth 1 — no write lock, so no deadlock, but a
later `begin` on that connection would issue a `SAVEPOINT`. The regression test
covers the openers this change touches; that residual is stated, not covered.

Test code may still open a transaction directly — AGENTS.md permits it, and
`check-transaction-openers.sh` scopes its boundary to production code — so a test
that calls `pool.begin_with` itself is outside this invariant.

## Considered & rejected

- **Close any connection released at a non-zero transaction depth, via the pool's
  `after_release` hook.** The first decision written here, and it does fix the
  leak. verified: it destroys `:memory:` databases. `crates/voom-store/src/pool.rs`
  builds memory pools with `shared_cache(true)`, `max_connections(1)` and
  `min_connections(1)`; a shared-cache in-memory database exists only while a
  connection to it is open. An isolated `sqlx` 0.8.6 program mirroring those
  options, with an `after_release` returning `Ok(false)` once, runs `CREATE TABLE t`
  and `INSERT` successfully and then fails the next `SELECT count(*) FROM t` with
  `SqliteError { code: 1, message: "no such table: t" }` — the pool refilled with a
  different, empty database (Fedora, Linux 7.1.8, rustc 1.95.0, sqlx 0.8.6).
  verified: it also reads the depth through `sqlx`'s `TransactionManager`, which
  `sqlx-0.8.6/src/lib.rs:33` re-exports but `sqlx-core-0.8.6/src/transaction.rs:11-15`
  marks `#[doc(hidden)]` with "This trait should not be used, except when
  implementing `Connection`" — a minor-version exposure, not a major-version one.
  verified: `rg -n 'SqlitePoolOptions' --type rust` finds five construction sites,
  not one, so a pool-option hook covers production code but not the four
  test-constructed pools — including `leases_test.rs:223`, which hands its pool to
  production `ControlPlane` code. judgment: `after_release` is a single-slot
  option, and `crates/voom-control-plane/src/scan/sessions_test.rs:1228` already
  uses it for something else, so a future hook would silently displace the guard
  rather than compose with it.
- **Roll the transaction back in `after_release` and keep the connection.** Avoids
  the memory-database consequence above. verified: it still reads depth through the
  `#[doc(hidden)]` `TransactionManager` trait, and still misses the four
  test-constructed pools, for the reasons cited in the previous bullet. judgment:
  it buys nothing the opener fix does not, at a lower-level dependency surface.
- **Wrap all four openers, not the two that can leak.** judgment: ADR 0086's own
  ground — a rule that fires everywhere carries no information about where the
  hazard is — and the deferred path's safety is cited above rather than assumed.
- **Raise the test's `HANG_GUARD`, or the client, `busy_timeout`, or pool
  budgets.** verified: #452 measured the same expiry rate at 10s, 60s and 150s
  guards (60s: 67.6/67.6/67.7s, 3 of 64; 150s: 156.2/157.8s, 2 of 32), with failing
  runs consuming exactly the budget each time; the bound moves and the hang does
  not. judgment: it is the bargain that made the `expire_due` contention tests
  false-green (#552).
- **Build the lock-free ring buffer of opener events that issue #592 specifies as
  the next diagnostic.** verified: it was aimed at finding which transaction holds
  the lock, and the answer — no transaction; a pooled connection — is established
  from the `sqlx` sources cited above and reproduced deterministically at a fixed
  poll count. judgment: an instrument whose question is answered is scope, not
  evidence.
- **Fix it upstream in `sqlx` and wait.** judgment: worth reporting, but `main` is
  red now and the repository pins 0.8.6; an invariant we hold in our own openers
  does not expire when the dependency moves.
- **Do nothing — the window is small.** verified: it is reached by ordinary client
  disconnects, and issue #592 reproduces it at roughly 1 run in 20–30 under
  `./scripts/run-constrained.sh --load 1 --write-bps 40M -- cargo llvm-cov
  --no-report -p voom-node-agent --test lifecycle --all-features -- --test-threads=1`
  — the same comment records that the `--write-bps` throttle is required and that
  101 unthrottled runs did not reproduce. judgment: its cost is every writer
  against that database until the process restarts.
