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
queue holding the write lock, and stays there — nothing quarantines it, because
the `test_before_acquire` default is that same `ping` — defaulted to `true` at
`sqlx-core-0.8.6/src/pool/options.rs:149`, fired at
`sqlx-core-0.8.6/src/pool/inner.rs:469-471`.

What later writers see then splits three ways. A writer on another connection
waits out the full 30s `busy_timeout` and fails; that is the path the node
agent's `deactivate` takes, exhausting its five attempts
(`voom-node-agent/src/client.rs:25`) so the incarnation is never marked
`Retired`. A writer *handed the poisoned connection* does not wait at all: a
custom `BEGIN` at depth > 0 is refused with `InvalidSavePointStatement` without
running any statements (`sqlx-sqlite-0.8.6/src/connection/worker.rs:210-222`), so
the two `BEGIN IMMEDIATE` openers fail immediately. The deferred openers do not
fail — they open `SAVEPOINT _sqlx_savepoint_1` inside the abandoned transaction
(`sqlx-core-0.8.6/src/transaction.rs:277-283`), and their `commit` issues
`RELEASE SAVEPOINT` (`:285-289`), which reports success while leaving the work
uncommitted under a transaction no value owns. When that connection is finally
closed, the outer transaction rolls back and an acknowledged write is gone. The
exposure is a stall *and* a silent-lost-write path.

Only the custom-statement path leaks. For a plain `BEGIN`, the acknowledgement is
a rendezvous send (`sqlx-sqlite-0.8.6/src/connection/worker.rs:81,528-538`) that
completes only once the receiver has taken the value — so a caller that vanished
before receiving makes the send fail, and the worker rolls the transaction back on
the spot (`worker.rs:234-252`). There is no second await point for a caller to
vanish in. In this repository the custom-statement path is exactly
[ADR 0086](0086-transaction-openers-are-named-helpers.md)'s `begin_read_then_write`
and `begin_serialized_read`.

Cancellation is ordinary here, not exotic, and one source is systematic rather
than incidental: `bounded_router` puts a `TimeoutLayer` at `request_processing`
on every route (`crates/voom-api/src/server.rs:348-351`,
`crates/voom-api/src/config.rs:12,111` — 30s), and it drops the handler future.
`axum` drops one on client disconnect too, and the node agent disconnects both
when it is fenced and when its own 30s per-attempt timeout fires. The layer fires
hardest under exactly the contention this record is about.

## Decision

**Opening a `BEGIN IMMEDIATE` transaction is cancellation-safe: the open runs to
completion whether or not its caller is still waiting.** The two openers that
issue a custom statement drive `pool.begin_with` on a detached `tokio` task and
take its result over a `oneshot`. A spawned task outlives the future that spawned
it, so a cancelled caller leaves a task that still constructs the `Transaction` —
and then drops it, which is the rollback the leak was missing. The `oneshot`
rather than the `JoinHandle` is what lets the detached side notice it was
orphaned and say so.

The two deferred-`BEGIN` openers are unchanged. They cannot leak the write lock:
a deferred `BEGIN` takes no lock, and the worker's rendezvous acknowledgement
already rolls back a cancelled open. A wrapper there would fire where the hazard
is not, which is the objection ADR 0086 records against rules that fire
everywhere.

Two sub-decisions follow.

**One opener vocabulary, and `init.rs` joins it.**
`scripts/check-transaction-openers.sh` already fails a build in which production
code calls `pool.begin*()` outside `voom-store/src/tx.rs`, which is why the
openers are the right place: the fix reaches every transaction that goes through
them, whichever pool it runs against. Four test modules construct their own
pools, and `crates/voom-control-plane/src/cases/execution/leases_test.rs:223`
hands one to production `ControlPlane` code — a fix expressed as a pool option
would miss it.

That check is not full coverage, and this record does not claim it is. Its rule
constrains the receiver to `(?i)pool` so a savepoint (`tx.begin()`) cannot match,
which also makes an `acquire`-then-`conn.begin_with` open invisible to it.
`crates/voom-store/src/init.rs:54-56` is exactly that shape, on the same
`BEGIN IMMEDIATE` two-step path, and `./scripts/check-transaction-openers.sh
crates` reports `OK (378 files)` with it present. This change routes it through
`begin_read_then_write`, which is also what it always meant — the explicit
`pool.acquire()` exists only so the connection can be returned before
`probe_schema`, and a pool-level `Transaction` returns it on `commit` or `drop`
anyway. That supersedes one sentence of
[ADR 0068](0068-serialize-sqlite-migration-application.md), whose Decision names
`conn.begin_with("BEGIN IMMEDIATE")` as the mechanism; 0068's substance is
untouched — the write lock is still taken up front and still held across the
whole run, and `run_direct`'s per-migration opens still nest as savepoints on the
same connection. Closing the guardrail's blind spot so a *future* site in that shape is
caught is deferred: `docs/debt/0005-connection-level-custom-begins-are-unguarded.md`.

**The regression test carries its own positive control.** Cancelling after
exactly *N* wakeup-driven polls is a deterministic *mechanism*; where it lands
inside the open is not, and depends on the pinned `sqlx`, `flume`, and `tokio`.
The test sweeps *N* from 1 to 8 twice — once through `begin_read_then_write`,
which must leave an independent connection able to take the write lock at every
*N*, and once through a bare `pool.begin_with("BEGIN IMMEDIATE")` written in the
test itself, which must leak at some *N*.

Only the control is host-dependent, and only the control is gated on host
parallelism. The fixed arm runs everywhere: post-fix the caller is a spawn plus a
`oneshot` await, so it is `Pending` on its first poll on any core count. That
split is deliberate — a skip notice from a passing test is invisible under both
of this repo's CI test invocations, so the regression proof must not sit behind
one.

What the control buys is not that the fixed arm's range still brackets anything —
the two arms count different clocks, and the fix is what decouples them, so the
exact poll positions live in the test file's comments where they can be corrected
when a bump moves them. It is that the `sqlx` 0.8.6 window is still present in the
bare shape: that the wrapper is still load-bearing rather than dead weight. A
control that goes green is telling a maintainer to check whether upstream closed
the window, in which case the wrapper and the control both go.

## Consequences

The deadlock's window closes. A cancellation inside it now costs one transaction
opened and immediately rolled back, instead of stalling every writer against the
database until the process restarts.

A cancelled caller's detached task keeps its pooled connection until
`BEGIN IMMEDIATE` returns — up to `LOCK_WAIT_BUDGET` under contention — and then
releases it. For a cancellation *inside* `BEGIN IMMEDIATE` that is not even a
change: an isolated `sqlx` 0.8.6 program cancelling eight opens against an
eight-connection pool whose write lock is held elsewhere behaves identically
before and after (`size=8`, `idle=0`, a live request failing on
`acquire_timeout`), because today's cancelled open cannot return its connection
until the in-flight `BEGIN IMMEDIATE` finishes on the worker thread either.

`Pool::begin_with` is two steps though (`sqlx-core-0.8.6/src/pool/mod.rs:391-400`),
and the other one does change. A caller cancelled while still inside `acquire()`
used to drop the acquire future, release its permit, and request nothing; now the
detached task finishes the acquire and issues `BEGIN IMMEDIATE` on behalf of a
caller that is gone, holding a pool slot for up to `LOCK_WAIT_BUDGET`. Under the contention this ADR
targets, `acquire` is where a request spends its time, and the `TimeoutLayer`
above answers a request at 30s while `POOL_ACQUIRE_BUDGET` is 45s — so a detached
opener routinely outlives the request that spawned it, and one `deactivate` call
can leave up to five of them across its five attempts.

The bound, rather than an adjective, in two clauses because `begin_with` is two
steps: at most `max_connections` detached openers *hold a pooled connection* at
once, each releasing it within `LOCK_WAIT_BUDGET` of acquiring it, since it
either takes the write lock and immediately rolls back or gives up on
`busy_timeout`; an opener still queued inside `acquire()` holds no connection and
is bounded by `POOL_ACQUIRE_BUDGET` on top, so worst-case termination is 75s, not
30s. So the pool drains on its own at a rate that does not depend on arrival rate — it degrades under a burst
and cannot wedge. No measurement of the acquire-cancellation case is offered
here; the argument is the two constants and the fact that a detached open has no
unbounded wait in it. Spawning only after the caller holds the connection would
remove the residual entirely, and that needs `Transaction::begin` and
`MaybePoolConnection` — the same `#[doc(hidden)]` surface the `after_release`
bullets below decline to reach for.

A detached open is otherwise invisible, on precisely the failure an operator will
meet, so the openers pay for one signal: the spawned task hands its result back
over a `oneshot`, warns when the send fails — the caller was cancelled and the
transaction is being rolled back on its behalf — and runs inside the caller's
`tracing` span, since `tokio::spawn` does not inherit one. That turns a pool
stall with no attribution, which is what made #592 cost a `gdb` session, into a
logged event.

Every `begin_read_then_write` and `begin_serialized_read` now costs one task
spawn on top of the channel round trip to the `sqlite` worker it already paid.
`voom-store` gains `tokio`'s `rt` feature, and the openers now require a `tokio`
runtime. That requirement is not new to the pool: `PoolConnection::drop` already
goes through `sqlx-core-0.8.6/src/rt/mod.rs:61-79`, which panics with "this
functionality requires a Tokio context" when `Handle::try_current()` fails. What
is new is where it bites — `tokio::spawn` itself panics both with no runtime and
during thread-local teardown (`tokio-1.53.1/src/task/spawn.rs:211-214`,
`runtime/context/current.rs:41-45`). A panic *inside* the spawned task surfaces
differently: the sender drops, the caller sees `RecvError` mapped to
`VoomError::Database`, and the payload and location reach the panic hook rather
than the call site — the cost of discarding the handle in exchange for the
orphan signal.

The detach discharges the guarantee only while a runtime outlives the task. On a
current-thread runtime whose `block_on` returns while the detached task sits
between the two steps, the task is dropped there and the leak recurs. Several
test harnesses run that shape (`crates/voom-test-support/src/commit_node.rs:349`,
`crates/voom-cli/tests/support/owner_node.rs:102,139`); it is moot there because
the pool dies with the runtime, but the condition belongs on the record.

The deferred openers stay safe by way of `sqlx` worker behaviour this record
cites but does not control. If that behaviour changed, a cancelled deferred open
would return a connection at depth 1 — no write lock, so no deadlock, but a
later `begin` on that connection would issue a `SAVEPOINT`. The regression test
covers the openers this change touches; that residual is stated, not covered.

Two shapes stay outside the invariant. A connection-level
`conn.begin_with("BEGIN IMMEDIATE")` has the same window and no guardrail;
`init.rs` was the only production instance and this change removes it, but
nothing stops the next one — `docs/debt/0005-connection-level-custom-begins-are-unguarded.md`
owns that. And test code may still open a transaction directly — AGENTS.md permits it, and
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
  successfully and then fails the very next statement — the `INSERT` — with
  `SqliteError { code: 1, message: "no such table: t" }`, because the pool's single
  connection is released after the `CREATE TABLE` and the database dies with it
  (Fedora, Linux 7.1.8, rustc 1.95.0, sqlx 0.8.6).
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
- **Detach at the HTTP boundary instead — a tower layer that spawns the handler
  future and awaits its `JoinHandle`.** The same technique one level up, and
  strictly wider: one layer in `crates/voom-api/src/server.rs`'s stack, no `rt`
  feature on `voom-store`, no spawn per opener, and it would cover the
  `acquire`-then-`conn.begin_with` shape, the four test-constructed pools, and
  every other await point in a handler rather than two. judgment: it changes what
  a cancelled request *means*. Today a request answered with 408 stops where it
  stopped; under that layer its whole transaction runs to completion and commits,
  and the work escapes request accounting entirely. The hazard is two openers
  wide; the remedy would be every handler wide.
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
- **Report it upstream to `sqlx` and wait for a release.** judgment: worth
  reporting either way, but `main` is red now and the repository pins 0.8.6.
- **Carry the fix locally with a `[patch.crates-io]` fork of `sqlx-sqlite`.** The
  defect is entirely inside `sqlx-sqlite-0.8.6/src/transaction.rs:19-30`, so a
  patched `SqliteTransactionManager::begin` would cover every call site — including
  `init.rs`, the four test-constructed pools, and any future
  `acquire`-then-`begin_with` shape — with no task spawn per open and no `rt`
  feature on `voom-store`. It is the option that most nearly makes the invariant
  workspace-wide. judgment: it buys that by taking on a forked dependency
  permanently — supply-chain surface, a `just ci` that builds it, and a re-fork at
  every `sqlx` bump — to fix two functions we already own and already funnel every
  production transaction through.
- **Do nothing — the window is small.** verified: it is reached by ordinary client
  disconnects, and issue #592 reproduces it at roughly 1 run in 20–30 under
  `./scripts/run-constrained.sh --load 1 --write-bps 40M -- cargo llvm-cov
  --no-report -p voom-node-agent --test lifecycle --all-features -- --test-threads=1`
  — the same comment records that the `--write-bps` throttle is required and that
  101 unthrottled runs did not reproduce. judgment: its cost is not only stalled
  writers until the process restarts but the silent-lost-write path in Context —
  a deferred opener handed the poisoned connection is told its commit succeeded.
