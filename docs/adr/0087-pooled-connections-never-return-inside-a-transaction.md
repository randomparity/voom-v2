# 0087 — Pooled connections never return inside a transaction

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
constructed only after the second. Between them the future has an await point.
Drop it there and the lock is held by a connection that no value owns.

The pool does not recover it. `sqlx-core-0.8.6/src/pool/connection.rs:275-325`
returns a connection by testing it with `ping()`; it never inspects the
transaction depth and never rolls back. So the connection goes back into the
idle queue holding the write lock, and stays there. Every later writer burns the
full 30s `busy_timeout` and fails; the node agent's `deactivate` exhausts its
five attempts and the incarnation is never marked `Retired`.

Only the custom-statement path leaks. For a plain `BEGIN`, the worker thread
notices the caller is gone — its acknowledgement send fails — and rolls back on
the spot (`sqlx-sqlite-0.8.6/src/connection/worker.rs:234-252`). There is no
second await point for a caller to vanish in. In this repository the
custom-statement path is exactly [ADR 0086](0086-transaction-openers-are-named-helpers.md)'s
`begin_read_then_write` and `begin_serialized_read`.

Cancellation is ordinary here, not exotic. `axum` drops a handler future when the
client disconnects, and the node agent disconnects both when it is fenced and
when its 30s per-attempt timeout fires.

## Decision

**A connection may not re-enter the pool while a transaction is open on it.**
`voom-store`'s single pool constructor enforces that with `after_release`: a
connection whose transaction depth is non-zero is logged and closed instead of
reused.

The invariant is stated at the pool boundary rather than at each opener, so it
holds for all four of ADR 0086's helpers, for savepoints, and for any future
`sqlx` path with a window we have not found. Nothing else in the workspace
constructs a pool.

Two sub-decisions follow.

**Closed, not rolled back.** `after_release`'s return value is a claim that the
connection is still usable, and the honest answer for a connection whose owning
future disappeared mid-operation is no. Closing releases the lock unambiguously;
the pool refills on demand.

**The occurrence is logged at `warn`.** Reaching this branch means a cancellation
landed inside the window — rare, and worth seeing. Silent recovery would turn a
concurrency defect into an invisible connection churn.

**The regression test does not race.** It polls `begin_read_then_write` for an
exact number of wakeup-driven polls, drops it, and then asks an independent pool
on the same file to write. Cancelling at exactly three polls lands in the window
every time, on any host, with no load, no throttle, and no elapsed-time
assertion. That is what lets it be a `just ci` test rather than a repetition
harness, and it is why [ADR 0085](0085-contention-tests-at-the-use-case-level.md)'s
rule about racing at the use-case level does not apply: there is no race to site.

## Consequences

The deadlock's window closes. A cancellation inside it now costs one connection
and one `warn` line instead of stalling every writer against the database until
the process restarts.

Nothing changes on the healthy path: a committed or rolled-back transaction
leaves depth at zero, so the hook returns the connection as before. The cost is
one atomic load per release.

The check reads `sqlx`'s transaction depth through the public
`TransactionManager` trait. That is a supported API, but it is a lower-level one
than the rest of `voom-store` uses, and a future `sqlx` major version could move
it. The regression test fails if the mechanism stops working, which is the
guardrail that matters.

The hook cannot make the window unreachable — a cancelled caller still holds the
write lock for as long as `PoolConnection::drop` takes to schedule the return
task. That is bounded by the runtime scheduling one task, not by `busy_timeout`.

`voom-node-agent/tests/budget_ladder.rs` observes that shrinking the server-side
budgets so a whole call fits inside one attempt "belongs with the #592 fix". It
does not land here. With the leak gone a lock wait is genuinely transient
contention, which is the case the 30s `busy_timeout` was sized for; re-sizing the
ladder is a separate change against separate evidence.

## Considered & rejected

- **Make each opener cancellation-safe by driving it on a detached task.**
  `tokio::spawn(async move { pool.begin_with("BEGIN IMMEDIATE").await })` survives
  its caller, so the `Transaction` is always constructed and always dropped.
  verified: it closes only the opener's own window — the pool's return path
  (`sqlx-core-0.8.6/src/pool/connection.rs:314`) still pings rather than resets,
  so any other path that returns a connection mid-transaction leaks exactly as
  before. judgment: it also puts a task spawn on every one of the ~186
  transaction opens ADR 0086 counts, to cover a window the pool-level guard
  covers for free.
- **Roll the transaction back in `after_release` and keep the connection.**
  `TransactionManager::rollback` is public and resets the depth correctly.
  judgment: it trades an unambiguous answer for a warm connection, on a path that
  by construction fires only when something already went wrong.
- **Guard in `before_acquire` instead.** verified: `before_acquire` runs when a
  connection is handed out, so the leaked lock would be held for the whole idle
  period first — which is the deadlock, unchanged, until the next acquirer
  happens to draw that connection.
- **Raise the test's `HANG_GUARD`, or the client, `busy_timeout`, or pool
  budgets.** verified: #452 measured the same expiry rate at 10s, 60s, and 150s
  guards, with failing runs consuming exactly the budget each time; the bound
  moves and the hang does not. judgment: it is the bargain that made the
  `expire_due` contention tests false-green (#552).
- **Build the lock-free ring buffer of opener events that issue #592 specifies as
  the next diagnostic.** verified: it was aimed at finding which transaction holds
  the lock, and the answer — no transaction; a pooled connection — is now
  established from the `sqlx` sources named above and reproduced deterministically.
  judgment: an instrument whose question is answered is scope, not evidence.
- **Fix it upstream in `sqlx` and wait.** judgment: worth reporting, but `main`
  is red now and the repository pins 0.8.6; a pool-level invariant we state
  ourselves does not expire when the dependency moves.
- **Do nothing — the window is small.** verified: it is reached by ordinary
  client disconnects and reproduces at roughly 1 run in 20–30 under
  `run-constrained.sh --write-bps 40M` (issue #592), and its cost is every writer
  against that database until the process restarts.
