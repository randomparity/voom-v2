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
ping — `options.rs:149` sets its default `true` and `pool/inner.rs:469-471` is
where it fires — so nothing quarantines it either. The connection sits in
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
`REQUEST_TIMEOUT` (30s, `crates/voom-node-agent/src/client.rs:33`) fires. This is
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

Three polls lands in the window **in this fixture**: two is before the write lock
is taken, four is after the `Transaction` is constructed. No load, no throttle,
no elapsed-time assertion.

**"Only 3" is a property of the fixture, not of the code, and an earlier draft of
this design overclaimed it.** Measured on the same 48-core host, same compiled
binary, varying only whether the pool's connection had already been returned to
the idle queue before the cancellation:

| fixture | N at which the write lock leaks (40 sweeps each) |
|---|---|
| warm — connection already idle in the pool | `{3: 40}` — N = 3, every sweep |
| cold — `acquire()` must establish the connection | `{5: 18, 6: 15}`, and **no leaking N at all in 11 of 40** |

Two consequences the "only value that does" reasoning did not predict: the window
is not always one poll wide (4 of the 29 non-empty cold sweeps blocked at both 5
and 6), and roughly a quarter of cold sweeps never enter it. This is why the test
below pins its fixture explicitly and repeats its sweep, rather than resting an
assertion on a poll index.

The count is **not** a version constant, and the first draft of this design said
it was. `conn.worker.begin` awaits a channel fed by a separate OS thread
(`sqlx-sqlite-0.8.6/src/connection/worker.rs:79-82,528-545`), so whether that
await is `Ready` on its first poll depends on whether the worker has already
replied — a scheduling artifact. Measured envelope, same compiled binary
throughout (Fedora / Linux 7.1.8 / rustc 1.95.0 / sqlx 0.8.6 / tokio 1.53.1 /
flume 0.11.1, debug):

| host parallelism | N at which the write lock leaks |
|---|---|
| 48 cores, unloaded, 8 runs | 3, every run |
| 4 cores + one busy loop per core, 8 runs | 3, every run |
| 1 core (`taskset -c 0`), 8 runs | 1, 1, 1, {1,2}, and **no leak at any N in 4 of 8** |

On one core the whole open collapses into one or two polls and there is often no
poll boundary inside the window at all. That is why the test sweeps a range
rather than pinning 3, and why it declares a parallelism precondition rather than
asserting unconditionally.

## Design

Two files: the openers that can leak, and the one production caller that opens the
same shape without going through them.

`crates/voom-store/src/tx.rs` — `begin_read_then_write` and
`begin_serialized_read` drive `pool.begin_with("BEGIN IMMEDIATE")` on a detached
`tokio` task and take its result over a `oneshot`:

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
                match &unsent {
                    Ok(_) => tracing::warn!(
                        context,
                        "transaction open completed after its caller was cancelled; \
                         rolling back"
                    ),
                    Err(error) => tracing::warn!(
                        context,
                        %error,
                        "transaction open failed for a caller that was already cancelled"
                    ),
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
seconds ago is indistinguishable from one held by a live request. Both outcomes
log, including the orphaned-and-failed one — an `acquire_timeout` firing for a
caller answered 45s earlier is exactly the pool-stall symptom worth attributing.
One orphan stays invisible and the design does not claim otherwise: `send` writes
the value before it CASes the state (`tokio-1.53.1/src/sync/oneshot.rs:622-646`),
so a caller dropped after that CAS gets a successful send and its `Transaction`
is dropped inside the channel. The rollback is correct either way; only the
signal is missing. The
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

One new error path, mapping to `VoomError::Database` exactly as the current
openers do:

- the spawned task panics, or is dropped without sending — either way the sender
  drops, `receiver.await` yields `RecvError`, and `database_context` maps it. No
  transaction is lost: the same drop rolls one back if it exists. Note what this
  costs — the `JoinHandle` is discarded, so a panic's payload and location reach
  the panic hook and not the call site, which sees a `DB_UNREACHABLE`-shaped
  error whose source reads "channel closed".

There is a third case, and the two bullets above read as an exhaustive
enumeration without it: if the **tokio runtime is torn down** while a detached
opener is in flight, the spawned task is dropped mid-`pool.begin_with` — the
original leak window again, now with no owner at all. The control plane's
graceful-shutdown grace is 30s (`SHUTDOWN_GRACE_SECONDS`,
`crates/voom-api/src/config.rs:14`), comfortably inside a 75s detached open under
contention. Accepted without machinery: the process is exiting and the file lock
dies with the handle. It has teeth in *tests* rather than in production, which is
why the orphan arm waits for its `warn` instead of asserting and returning.

The residual the detach adds is bounded and stated in ADR 0087, in two clauses
because `Pool::begin_with` is two steps. At most `max_connections` detached
openers *hold a pooled connection* at once, each releasing it within
`LOCK_WAIT_BUDGET` of acquiring it. An opener still queued inside `acquire()`
holds no connection and is additionally bounded by `POOL_ACQUIRE_BUDGET`, so
worst-case termination for a detached opener is `POOL_ACQUIRE_BUDGET +
LOCK_WAIT_BUDGET` — 75s, not 30s. It degrades and drains rather than wedging;
the drain is just slower than the first draft claimed.

Against the real numbers: file pools carry `max_connections = 8`
(`crates/voom-store/src/pool.rs:81`), and one node-agent call retries five times
across a 153.75s budget (`crates/voom-node-agent/tests/budget_ladder.rs`), so a
single `deactivate` under contention can leave up to five detached openers, and
eight concurrent ones saturate the pool. A `:memory:` pool has one connection, so
one detached opener blocks it for the window. That is accepted, on the ground
that every detached opener terminates within its own budget with no unbounded
wait in it, so occupancy decays regardless of arrival rate.

State the interaction that bound does *not* cover, because it is unstated
elsewhere: 75s exceeds the node agent's 30s per-attempt timeout
(`crates/voom-node-agent/src/client.rs:33`) and is a large fraction of its
153.75s five-attempt budget. A request that queues behind a saturated pool can
exceed the client's per-attempt timeout before it ever asks for the lock, and
that attempt's own cancellation adds another detached opener. "Occupancy decays"
is asymptotic; the criterion is a finite window.

It is accepted, and the honest statement of what it is accepted *on* is an
assumption rather than a consequence: that **no other sustained source of pool
occupancy coincides with a cancellation burst** — I/O throttling, a long
migration, a slow query. Saying instead that "saturation needs sustained lock
contention, which this change removes" would be circular, and this document
supplies its own counterexample: the acceptance sweep below deliberately creates
one, running under `--write-bps 40M`, an fsync throttle that #592 records as
*required* to reproduce. Two agents deactivating concurrently against a throttled
disk are exactly the shape that could reach the eight-opener saturation. The 75s
termination bound still holds there; what does not hold is any claim that the
condition cannot arise. No machinery is added for it: bounding the detached open
with a timeout would drop the detached future inside the leak window and undo the
fix. The assumption is exercised by the sweep, not asserted by a test.

No test asserts the bound itself:
the property follows from two constants rather than from a race, and a test that
tried to observe it would need sustained contention and a timing assertion —
the shape this design exists to avoid. The `run-constrained.sh` acceptance sweep
below is where real contention is exercised.

## Testing

**`crates/voom-store/tests/cancelled_begin_releases_write_lock.rs`** — new
integration test, the regression proof.

- A helper polls a future for exactly *N* wakeup-driven polls and then drops it.
  It does not self-wake, so each poll corresponds to one genuine step of
  progress; that is what makes the count reproducible rather than a timing sweep.
- For *N* in 1..=8 it cancels the open at that point, then asks an **independent
  connection** on the same database file to execute a write. Independent is what
  makes the assertion honest: a write issued back through the *same* pool can be
  handed the leaked connection and silently join its open transaction.
- **The fixture is pinned, and it is load-bearing.** Every *N*, in every arm, gets
  a **freshly created temporary database file, its own pool, and its own observer
  connection**; no arm shares a pool or a file with another. Two contamination
  paths make this a correctness requirement rather than hygiene. An abandoned lock
  is held until the process exits, so within a shared file the control arm's first
  leaking *N* poisons every later *N* — and the fixed arm too, if they share —
  turning the regression proof red for a reason unrelated to the fix. And the
  poisoned connection goes back to the idle pool, so a later `BEGIN IMMEDIATE`
  handed that connection fails at once with `InvalidSavePointStatement`
  (`worker.rs:210-222`) **without taking any lock**, making the observer report
  "not blocked" for a reason that has nothing to do with the window. The pool is
  built by `voom_store::connect` (`max_connections = 8`), and the sweep performs
  its warm-up — one completed transaction through the pool — before the
  cancellation, because the measured N table only reproduces in the warm shape.
- **The observer sets `busy_timeout(0)`**, built from a raw
  `SqliteConnectOptions` in the test rather than through `voom_store::connect`
  — which sets `busy_timeout = LOCK_WAIT_BUDGET` (30s,
  `crates/voom-store/src/pool.rs:20,60`). With the repo's value SQLite does not
  return on a held lock, it sleeps, so "blocked" would be observable only as a
  wall-clock expiry and would be indistinguishable from a slow host. At 0 a held
  lock returns `SQLITE_BUSY` immediately: each attempt is an observable,
  attributable error. The observer then retries on `SQLITE_BUSY` and reports
  whether the lock ever became takeable, bounded at **5s in the fixed and orphan
  arms** and **1s in the control arm**. The control's is shorter because at a
  leaking *N* it necessarily burns its whole ceiling, and this test runs in the
  serialized instrumented `coverage` job whose duration is this issue's subject;
  the bound is not what discriminates there, the repeated `SQLITE_BUSY` is.
- **Two named constants, and what makes each safe.** A **100ms settle** between
  the cancellation and the first observer attempt, and the **5s** retry bound
  above. The settle is what stops the observer from taking and releasing the lock
  before the cancelled `BEGIN IMMEDIATE` ever asks for it. Its adequacy is
  established **per arm**, because the three arms have different timelines and an
  argument from one does not transfer to another:

  - *control arm*: the settle is validated by the arm having to go red. The
    unfixed open runs inline on the caller's task, so a settle too short to let
    the lock be taken would make the control go green and fail the test.
  - *orphan arm*: no settle is needed, but not for the reason an earlier draft
    gave — the observer *can* slip in front of the detached opener, and measured
    20/20 does. The arm does not need a settle because it does not rely on
    timing at all: it waits for the orphan `warn` before probing the lock.
  - *fixed arm*: the settle is **not** validated by the control, and an earlier
    draft claimed it was. The post-fix timeline is strictly longer and
    structurally different — task spawn, `pool.acquire()`, worker round trip —
    and no arm measures it. Stated honestly: the fixed arm's settle is sized
    against the measured latency of a detached open on the pinned toolchain, and
    if it were short the arm would go green without proving the rollback. The
    orphan arm is what covers that gap, which is a further reason it exists.

  The 5s bound only has to exceed the detached rollback's latency, which is one
  queued statement on a worker thread, not a lock wait; it does not discriminate
  between the arms, because the control's failure is now `SQLITE_BUSY` on every
  attempt rather than the bound expiring.
- **Two sweeps, each repeated.** Through `begin_read_then_write`, the independent
  write must succeed at every *N*. Through a bare `pool.begin_with("BEGIN
  IMMEDIATE")` written in the test itself, it must fail at some *N* — that is the
  positive control, and it is what keeps the first sweep honest. Without it the
  test is green whether or not the sweep still straddles a vulnerable window, so a
  dependency bump that moved the window outside 1..=8 would leave an assertion
  that passes while proving nothing.
- **The control asserts across repeats, not within one sweep.** A single sweep is
  not a reliable observation: in the cold fixture measured above, 11 of 40 sweeps
  contained no leaking *N* at all. Asserting "fails at some *N*" on one sweep
  would therefore go red about a quarter of the time for a reason unrelated to
  upstream — and would send a reader to exactly the wrong place, since this spec
  reads a red control as "check whether upstream closed the window". So the
  control runs the sweep **5 times and requires at least one blocked observation
  across the repeats**: at the worst measured per-sweep miss rate that is
  0.275^5 = 0.0016. The fixed arm runs the same 5 repeats and requires **no**
  blocked observation in any of them.
- Note what the control does *not* buy: the two arms count different clocks, so a
  red control means "check whether upstream closed the window", not "the fixed arm
  stopped covering".
- **The poll sweep cannot cancel a post-fix open, and the fixed arm does not
  claim to.** Post-fix the caller's only await is `receiver.await`, and its only
  wakeup source is the `oneshot` send — which happens *after* `pool.begin_with`
  has returned. So at *N* = 1 the helper is re-entered by the completion wakeup
  itself and drops a future whose value is already in the channel; at *N* >= 2 the
  open completed on an earlier poll. At every *N* in the sweep the transaction is
  fully open before the caller goes away. An earlier draft of this spec asserted
  "at least one *N* actually cancelled" as an anti-vacuity guard; that assertion
  is unsatisfiable in the sense intended and reports true in exactly the
  degenerate case it was meant to catch, so it is **removed**. It was also a flake
  risk in the other direction: on a many-core host the detached task can finish
  before the parent's first poll of the receiver, making *N* = 1 resolve `Ready`
  immediately and turning the guard spuriously red.

  What the fixed arm is, then, is a **regression** assertion and nothing more: it
  is red on the unfixed code at *N* = 3 and green after the fix, which is
  completion criterion 3. Cancellation *during* an open is covered by the orphan
  arm below, deterministically, instead of being hoped for here.
- **Orphan arm — deterministic, no poll counting, no host dependence.** This is
  the arm that exercises a caller vanishing while the open is genuinely in
  flight, and it gets its determinism from SQLite's lock rather than from timing:

  1. A holder connection takes `BEGIN IMMEDIATE` and keeps the write lock.
  2. The caller runs `begin_read_then_write(&pool, …)` under a short
     `tokio::time::timeout`. While the holder has the lock, the detached
     `BEGIN IMMEDIATE` **cannot** return — so the timeout is guaranteed to fire
     and drop the caller mid-open. This is a physical guarantee from SQLite, not
     a race that happens to land.
  3. The holder rolls back, releasing the lock.
  4. **Wait for the orphan `warn` to fire**, with a `tracing-subscriber` capture
     layer installed by the test, bounded by the 5s ceiling. This is the step that
     makes the arm mean anything, and it replaces a claim that was measured false
     (below).
  5. Only then: an independent `busy_timeout(0)` connection must be able to take
     the write lock.

  Steps 4 and 5 are what discriminate. The `warn` proves the detached open
  *completed and found no receiver* — the `sender.send` error branch ran. The
  lock probe after it proves the resulting drop *rolled back*. Neither alone is
  enough, and asserting the lock probe alone is what the earlier draft did.

  **Why step 4 is not optional — a measured refutation.** The earlier draft
  claimed the holder's lock meant "the observer cannot slip in front of the
  detached opener by construction". That is false. While the holder owns the lock
  the detached opener is parked in SQLite's busy handler, which *sleeps* between
  retries in an increasing delay sequence up to 100ms; a freshly opened observer
  asking for the lock inside that sleep window simply gets it. Measured against
  this design's own `begin_detached`, 20 runs: the observer took the lock on its
  **first** attempt in 20/20, 0.36–0.67ms after the holder's rollback, with the
  `sender.send` error branch still not executed at assertion time in 20/20 (it ran
  within a further 500ms). So the arm as first written passed without the detached
  open ever having taken or released the lock — it would have passed equally
  against a `begin_detached` whose spawned task was aborted and rolled nothing
  back. What the holder's lock *does* still guarantee is that the timeout in
  step 2 fires; that half of the argument survives.

  This is the only coverage the `sender.send` error branch will ever get, and
  that branch is the whole point of the design. It also needs the wait for a
  second reason: a `#[tokio::test]` runtime torn down immediately after its last
  assertion can kill the detached task before it rolls back, so a test that does
  not wait for the branch does not reliably execute it either.

  Note **which** red it gives on the unfixed code, because it is not the useful
  one. Cancelling inside step 1 of `SqliteTransactionManager::begin` lands in the
  window the worker already self-heals (`worker.rs:234-252`), so the lock probe
  in step 5 would pass pre-fix. But step 4 fails there: the unfixed opener has no
  spawned task and emits no `warn`, so the arm burns its 5s ceiling and goes red
  on the missing event. A reader who reverts the fix, sees red, and concludes
  "the arm bites" will never have exercised step 5 — which is the assertion the
  arm is for. So this stays a **coverage** arm rather than a regression arm, and
  the controlled fault described in the plan, not a revert, is what proves step 5
  works.

  Cost: `tracing-subscriber` joins `crates/voom-store/Cargo.toml` as a
  **dev-dependency**, at the version the workspace already pins for `voom-api`,
  `voom-control-plane`, and `voom-cli`. No production dependency changes.
- **The parallelism precondition gates the control arm only.** The **fixed and
  orphan arms run on every host.** The fixed arm's post-fix outcome does not
  depend on where the sqlx window falls — the open completes on the detached task
  at every *N*, on one core as on forty-eight — and the orphan arm's determinism
  comes from a held lock, not from scheduling. Only the control arm asks where
  the window is, so only the control arm is host-dependent. The gate is a silent
  early return: `libtest` hides a passing test's output under both CI
  invocations anyway, and a skip notice would cost a `clippy::print_stderr`
  expectation for a line no CI reader sees.

  The **control** arm is skipped when `std::thread::available_parallelism()`
  reports fewer than 4. On one core the unfixed open collapses into one or two
  polls and the window frequently has no poll boundary in it at all (table
  above), which would make the control red on a 1-vCPU container for a reason
  that has nothing to do with the defect. Four is the lowest parallelism
  *sampled* to reproduce deterministically, including with a busy loop per core —
  a sampled point, not a located boundary — measured under
  `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`, which the test
  carries, so the gate and the measurement are about the same thing.

  This split is what keeps the skip from mattering. `libtest` captures a passing
  test's output and prints it only under `--nocapture` / `--show-output`, and
  neither CI invocation passes either — `just ci` runs
  `cargo test --workspace --all-features` (`justfile:52`), and the coverage job
  runs `cargo llvm-cov … -- --test-threads=1` (`.github/workflows/ci.yml:83`). So
  a skip notice is *invisible* in both, and a design that put the regression
  proof behind it would let CI go green with zero coverage of the fix and no
  visible signal. It does not, because neither the proof nor the orphan coverage
  is behind the gate. What is behind it is the meta-check.
- **Which CI legs clear the gate is an external property, and it is sourced
  rather than asserted.** `randomparity/voom-v2` is public
  (`gh repo view --json visibility` → `PUBLIC`), and GitHub's documented
  hosted-runner specs for public repositories give `ubuntu-latest` 4 vCPU and the
  arm64 `macos-latest` 3. So `test (ubuntu-latest)` and `coverage` clear the gate
  and `test (macos-latest)` skips the control permanently — but that is GitHub's
  fact, not this repository's, and it moves without notice. **If the repository
  were made private, the 2-vCPU default would skip the control on every CI leg**,
  silently, which is the failure mode this whole paragraph is about. The exposure
  is bounded rather than eliminated: the regression and orphan arms still run
  everywhere, so what is lost in that case is the meta-check, not the coverage.
  The first CI run on this branch should have its observed
  `available_parallelism()` recorded here in place of the documented figures.
- **Which arm establishes what.** The fixed arm proves the lock is not held
  indefinitely after a cancellation, and is red against the unfixed opener — that
  is completion criterion 3, and nothing else discharges it. It does *not*
  distinguish "opened and rolled back" from "never opened", and the poll sweep
  cannot be made to. **The orphan arm is the sole home of "a cancelled open is
  rolled back"**, which is the entire behavioural change this design introduces
  and the premise the Error-handling residual is derived from; it establishes it
  through the `warn`-then-probe pair in steps 4–5, on every host. The control arm
  establishes only that the *unfixed* window is still real. Stated this way
  because an earlier draft assigned the rollback discrimination to an arm whose
  assertion did not observe it, leaving the design's central behaviour unproven by
  any arm.
- Test files are exempt from `check-transaction-openers.sh`
  (`! -name '*_test.rs' ! -path '*/tests/*'`), so the control's raw
  `pool.begin_with` is allowed where it lives.
- Measured on the pinned toolchain: the unfixed opener leaks at *N* = 3 and only
  at 3; the fixed one completes from *N* = 2. Both counts are properties of the
  pinned `sqlx` 0.8.6, `flume`, and `tokio`, which is why the sweep is a range
  and the control is what asserts the range is still the right one.

Both arms assert an observable state transition rather than an elapsed duration:
with `busy_timeout(0)` the observer gets `SQLITE_BUSY` or a completed write on
every attempt, never a silent sleep. The fixed arm's 5s retry bound is a
fail-fast ceiling on a rollback that takes one statement. The control arm's
assertion is honestly a **bounded wait** — an abandoned lock is held until the
process exits, so "held" can only ever be observed as "still `SQLITE_BUSY` after
a bound". Naming it that way is the point: the bound is not what discriminates,
the repeated `SQLITE_BUSY` is.

**`crates/voom-store/tests/`** existing init/migration coverage exercises the
`init.rs` change; no new test is written for it, since the behaviour is
unchanged and the shape is what moved.

**Acceptance run for completion criterion 5.** The mechanism test proves the
window is closed; it does not discharge "the `coverage` job's serialized
instrumented run no longer reproduces the hang", because #592 reproduces at
roughly 1 run in 20–30 and a single green `just ci` is indistinguishable from a
run that never entered the window. The run that discharges it is #592's own
recipe, repeated:

```
./scripts/run-constrained.sh --load 1 --write-bps 40M -- \
  cargo llvm-cov --no-report -p voom-node-agent --test lifecycle \
  --all-features -- --test-threads=1
```

**at least 90 times.** 60 is not enough and the first draft's arithmetic read the
range backwards: 1-in-30 is the *lower* failure probability, so a clean 60-run
sweep still has probability (29/30)^60 = 0.13 of proving nothing. At 90 runs it
is (29/30)^90 = 0.047.

**That 0.047 is conditional, and the condition is not established by this
protocol.** It assumes *p* >= 1/30 **on the host running the sweep**, and #592's
rate was measured elsewhere. The pre-fix control below stops at the first
reproduction, which establishes only that *p* > 0 there. If the sweep host's true
rate were 1/200 — still reproducing, just rarer under different disk latency or
core count, and this document already documents how configuration-sensitive the
reproduction is — then 90 green runs carry a false-negative probability of
(199/200)^90 = 0.64, not 0.047. So report the figure as conditional and report
the **pre-fix reproduction index** (which run number first reproduced) as the
host-local evidence it is; that index is what a later reader needs to judge
whether the rate transferred. Making 0.047 load-bearing instead would mean
running the pre-fix arm to the full 90 and deriving the bound from the
reproduction count actually observed — more than this criterion is worth, and
stated here so the choice is visible rather than silent.

**The pre-fix control gates the sweep.** Reproduction is configuration-sensitive,
not merely rare — #592 records that 101 unthrottled runs did not reproduce and
that the `--write-bps` throttle is required — so 90 green post-fix runs cannot be
told apart from a host that would never have entered the window that day. Run the
unfixed opener under the identical invocation on the same host, stopping at the
first reproduction or **at 90 runs, whichever comes first** — the same bound and
the same stated confidence as the post-fix arm. "Until it reproduces" is not a
protocol and "~25 runs" is not a bound: 25 is roughly the mean of a geometric
with *p* = 1/30, at which a false negative still has probability
(29/30)^25 = 0.43. At 90 it is 0.047, matching the other arm. If 90 unfixed runs
do not reproduce, criterion 5 is reported as **not discharged** at that
confidence, not as a green sweep.

**Cost up front:** the full protocol is up to ~180 instrumented, throttled,
`--test-threads=1` lifecycle runs. Report both counts either way; anything short
of the full protocol is reported as what it is.

**Unchanged:** `crates/voom-node-agent/tests/lifecycle.rs`. `HANG_GUARD` stays at
30s. That test is the end-to-end signal for this defect and it stays tight, which
is the whole reason it caught this.

**What discharges completion criterion 2** — graceful shutdown reaching the
durable `Retired` write under write contention rather than exhausting the retry
budget — is `live_agent_fences_prior_incarnation_and_retires_orderly`, unchanged,
running inside the acceptance sweep above. It therefore inherits that sweep's
probabilistic confidence and everything said about it: criterion 2 is discharged
at the same conditional figure, not deterministically. A deterministic
use-case-level contention test in ADR 0085's shape (barrier-released claimers
against a real on-disk WAL database) is **not** added, because the contention
this defect needs is a cancellation landing in a one-poll window rather than a
race between claimers — the orphan arm constructs that directly and at the store
layer, where it is deterministic, instead of trying to provoke it through the
use case.

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

- **Re-sizing the control-path budget ladder.** This exclusion overrides a
  repository record, so quote what it overrides rather than paraphrasing it.
  `crates/voom-node-agent/tests/budget_ladder.rs:31-33`: *"Shrinking the
  server-side budgets so a whole call fits inside one attempt is the better
  long-term answer; it collides with the `busy_timeout >= 30s` floor in
  `voom-store/src/pool_test.rs` and belongs with the #592 fix."*

  It does not land here. With the leak gone, a lock wait is the transient
  contention the 30s `busy_timeout` was sized for; and the resize collides with a
  floor asserted in another crate's tests, which is separate work against
  separate evidence. But the exclusion is taken on **this run's** authority
  against that record, not on the operator's, so it is flagged in the pull request
  in those terms — and the residual it leaves sits on criterion 2's own path
  (Error handling, above). The acceptance sweep's orphan-`warn` counts are the
  evidence that decides whether this stays a clean exclusion or becomes a
  follow-up issue; if they show sustained multi-opener occupancy, that is a
  **split**, not something to absorb here.
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
