# Concurrent init migration locking

Status: draft
Date: 2026-08-19
Base: `main` at `683a81d0`

## Context

`crates/voom-store/tests/init.rs::concurrent_init_stress` races six peers'
`init()` calls against one on-disk SQLite file, twenty times, and
intermittently fails under full-workspace test contention with
`migration error: running migrations failed: while executing migration 2:
error returned from database: (code: 1) table events already exists` (issue
#505), after `probe_after_failure`'s 30-second recovery budget is exhausted.

`sqlx`'s SQLite backend does not implement real migration locking:
`SqliteConnection::lock()`/`unlock()` are no-ops (verified against vendored
`sqlx-sqlite-0.8.6`). Every concurrent `init()` call independently reads
`list_applied_migrations()` and may attempt `apply()` for a migration another
peer is about to commit or has just committed; the loser hits a hard SQL
error and falls into `probe_after_failure`'s retry-with-backoff loop, which
polls `probe_schema` until it observes a terminal state or its budget
expires.

This is the third time this exact failure mode has surfaced as the migration
set has grown (see ADR 0068's Context for the two prior recovery-budget
increases). Reproducing #505 under two concurrent full-workspace
`cargo test --workspace --quiet` runs on an 18-core host confirmed the
mechanism directly: 49–53 losing-peer recoveries per run (all eventually
succeeded), and a *successful, non-racing* `MIGRATOR.run` already took up to
8–9 seconds under that contention — this crate's own migration set has grown
large enough (36 files, ~3,638 lines of DDL) that simply applying it can
consume a meaningful fraction of the existing 30-second budget before any
race-recovery overhead is added.

[ADR 0068](../adr/0068-serialize-sqlite-migration-application.md) records the
decision: serialize the migration-application phase with a real held write
lock (`BEGIN IMMEDIATE`) instead of raising the recovery budget a third time.

## Decision

`run_migrations_on` (`crates/voom-store/src/init.rs`) changes how it invokes
the migrator. Today it calls `MIGRATOR.run(pool)`, which lets sqlx acquire
its own pool connection internally. The new flow acquires the connection
itself, explicitly locks it, and hands the locked connection to
`MIGRATOR.run_direct`:

```rust
let mut conn = pool.acquire().await.map_err(|e| {
    VoomError::database_context("acquire connection for migration", e)
})?;
let mut tx = conn.begin_with("BEGIN IMMEDIATE").await.map_err(|e| {
    VoomError::database_context("acquire migration write lock", e)
})?;
tx.ensure_migrations_table().await.map_err(|e| {
    VoomError::database_context("ensure migrations table under lock", e)
})?;
let locked_applied = tx.list_applied_migrations().await.map_err(|e| {
    VoomError::database_context("read applied migrations under lock", e)
})?;
let locked_before_count = locked_applied.len() as u32;
let migrate_result = MIGRATOR.run_direct(&mut *tx).await;
```

`tx.ensure_migrations_table()` is the same idempotent `CREATE TABLE IF NOT
EXISTS _sqlx_migrations` call `run_direct` makes internally as its own first
step; calling it here, before `list_applied_migrations()`, is required —
against a genuinely fresh database (no prior `init()` has ever run;
`concurrent_init_stress`'s setup and every real first `voom init` both start
this way) `_sqlx_migrations` does not exist yet, and `list_applied_migrations()`
is a raw `SELECT` with no existence guard that hard-errors against a missing
table. Calling `ensure_migrations_table()` first guarantees the table exists
before that `SELECT` runs. `run_direct`'s own subsequent internal call to
`ensure_migrations_table()` is idempotent, so this adds one no-op statement on
every call after the first and changes no other behavior.

`tx.list_applied_migrations()` (the same `Migrate`-trait method `run_direct`'s
own loop uses internally, called here once up front, after the table is
guaranteed to exist) reads the applied-migration count from inside the
transaction that just took the write lock — the authoritative view of what
this peer's own serialized turn actually starts from, as opposed to a
separate, unlocked, pre-lock snapshot. `locked_before_count` replaces the
pre-lock `before`-probe's count for `InitReport`'s
`migrations_applied`/`already_initialized` bookkeeping (see Invariants and
boundaries); the pre-lock probe is kept only for its original `TooNew`/`Dirty`
fast-fail short-circuit, which must still run before any connection or lock is
taken.

`begin_with("BEGIN IMMEDIATE")` takes SQLite's write lock immediately
(subject to the pool's existing 30-second `busy_timeout`), rather than
`BEGIN`'s default deferred behavior, which only takes the lock at the first
write statement and can observe a stale snapshot up to that point. A peer
that loses the race for the lock blocks until the winner's transaction ends;
when it acquires the lock, its `list_applied_migrations()` read (executed by
`run_direct`, unchanged) runs inside its now-current transaction and sees
every migration the winner committed. `run_direct`'s existing loop finds
nothing left to apply for each already-known version (matching checksums)
and returns `Ok(())`. No peer reaches `apply()` for a migration another peer
already committed, so the `table X already exists` error path becomes
unreachable for this race.

This is exactly the scenario `locked_before_count` exists to handle correctly.
A peer whose pre-lock `before` probe observed `Uninitialized`/`Partial` (because
it ran before any peer had migrated) but then blocks on `BEGIN IMMEDIATE` while
another peer fully migrates now returns `Ok(())` from the *success* path, not
the error path — so it must not report `migrations_applied` as the full
migration count it never touched, or `already_initialized: false` for a
database another peer just finished initializing. Using `locked_before_count`
(read after this peer's own lock acquisition, once the winner's transaction has
already committed) instead of the pre-lock `before_count` makes both values
correct: `migrations_applied` becomes `0` for the blocked-then-no-op peer (it
applied nothing — `run_direct` found nothing left to do), and
`already_initialized` becomes `true` (this peer's own transaction, once
serialized, observed a database another peer had already migrated).

`apply()`'s own per-migration `self.begin()` calls (unchanged, internal to
`sqlx-sqlite`) become nested `SAVEPOINT`s because the connection is already
inside our outer transaction — this is `sqlx`'s ordinary, existing behavior
for `Connection::begin()` on an already-transacting connection and requires
no change on our side.

On success, the outer transaction commits, and the success-path `InitReport`
is built from `locked_before_count` rather than the pre-lock `before_count`,
replacing both of today's corresponding expressions
(`migration_count.saturating_sub(before_count)` and
`matches!(before, SchemaState::Current { .. })`):

```rust
if let Ok(()) = migrate_result {
    tx.commit().await.map_err(|e| {
        VoomError::database_context("commit migration transaction", e)
    })?;
}
let after = probe_schema(pool).await?;
let SchemaState::Current { migration_count, schema_init_at } = after else {
    return Err(VoomError::Migration(format!(
        "post-init schema state is not Current: {after:?}"
    )));
};
let migrations_applied = migration_count.saturating_sub(locked_before_count);
let already_initialized = migrations_applied == 0;
```

This single formula covers both callers of the success path: a genuine
first-time winner has `locked_before_count == 0`, so `migrations_applied ==
migration_count` (every migration this call actually applied) and
`already_initialized == false`; a blocked-then-no-op racing peer has
`locked_before_count == migration_count` already (the winner it waited on
already committed everything), so `migrations_applied == 0` and
`already_initialized == true` — the two field values Failure behavior's
"Ordinary race" bullet and the paragraph above already describe in prose, now
pinned to one derivation instead of two illustrative examples.

On error, the transaction is dropped (sqlx rolls back an uncommitted
`Transaction` on `Drop`, releasing the write lock), and the code falls
through to a single `probe_schema(pool).await?` call feeding the *same*
`match` block `run_migrations_on` already has today (`Current` →
race-recovery success shape; `Dirty` → `VoomError::DirtyMigration`; `TooNew`
→ `VoomError::SchemaTooNew`; anything else → the generic `VoomError::Migration`).
That classification is unchanged and stays reachable for causes unrelated to
peer racing (see Failure behavior). What is removed is `probe_after_failure`'s
retry-with-backoff loop and its three budget constants
(`MIGRATION_RACE_RECOVERY_BUDGET`, `MIGRATION_RACE_INITIAL_DELAY`,
`MIGRATION_RACE_MAX_DELAY`) — the loop existed only to wait for a
concurrently-racing peer, which can no longer happen (ADR 0068's Decision).
The single probe call replaces the whole loop; the match arms it feeds are
untouched.

## Invariants and boundaries

- `connect()`'s no-create, no-migrate contract (ADR 0003) is unaffected: the
  new lock is acquired only inside `init()`'s migration path, never from
  `connect()`.
- `probe_schema`'s classification order and every `SchemaState` variant are
  unchanged.
- The pre-migration `before = probe_schema(pool).await?` check still runs
  against the pool directly, before the migration connection is acquired, and
  still short-circuits `TooNew`/`Dirty` before any lock is taken. It no longer
  feeds `InitReport`'s `already_initialized`/`migrations_applied` bookkeeping
  on the success path — that bookkeeping now comes from `locked_before_count`
  (see Decision), read from inside the transaction that holds the write lock,
  because the pre-lock probe can go stale for a peer that blocks on the lock
  while another peer migrates. The pre-lock probe's role is now `TooNew`/`Dirty`
  short-circuit only.
- `emit_schema_initialized_if_missing` is unchanged and still runs against
  the pool after the migration path (success or recovered) completes.
- `MIGRATOR`'s migration set, versions, and checksums (`crates/voom-store/src/migrator.rs`)
  are unchanged.

## Failure behavior

- **Ordinary race (the case this fix targets):** a losing peer blocks on
  `BEGIN IMMEDIATE`, then no-ops through `run_direct` once it acquires the
  lock. No error, no re-probe call. `migrations_applied: 0` and
  `already_initialized: true`, derived from `locked_before_count` (see
  Decision) rather than the pre-lock probe, so the report accurately reflects
  that this peer applied nothing.
- **Busy-timeout exhaustion:** if `BEGIN IMMEDIATE` itself cannot acquire the
  lock within the pool's 30-second `busy_timeout`, `begin_with` returns a
  `sqlx::Error`, wrapped as `VoomError::database_context`. This is a new,
  distinct failure point from today's `MIGRATOR.run` error branch — it is
  surfaced directly, with no re-probe, because there is no partial migration
  state to reconcile: this peer never started applying anything. SQLite's own
  busy handler already retried lock acquisition internally for the full
  `busy_timeout` window before returning this error, so no additional
  application-level retry is added on top.
- **A migration genuinely fails after the lock is held** (bad SQL, disk
  error, corruption): `migrate_result` is `Err`, the transaction is dropped
  (rolled back, releasing the lock), and a single `probe_schema(pool).await?`
  call classifies the state (`Dirty`/`TooNew`/generic `Migration`) and
  returns immediately. No peer collision could have caused this failure —
  this peer held the only write lock — so there is nothing to wait for, and
  the fix removes the retry-with-backoff loop that used to poll here (see
  ADR 0068). This failure now rolls back **every** migration applied earlier
  in the same run, not just the failing one — each earlier `apply()` became
  a nested `SAVEPOINT`/`RELEASE` inside the still-open outer transaction,
  provisional until the final `COMMIT` — so the DB returns to its exact
  pre-run state and the next `init()` reapplies the whole batch. A fresh
  failure therefore surfaces through the generic `Migration` error, not
  `Dirty` (`apply()` never writes a `success = false` row); `DirtyMigration`
  remains reachable only for a row left dirty before this fix shipped, or
  from direct `_sqlx_migrations` tampering. See ADR 0068's Consequences.
- A process killed mid-migration leaves no `_sqlx_migrations` row for the
  in-flight version (the whole transaction, including the row insert, rolls
  back with the connection), so the next `init()` caller sees a clean
  `Partial` state and reapplies from where the crashed peer left off — no
  new crash-recovery behavior is introduced.

## Compatibility and rollback

This is an internal change to how `init()` invokes the existing migrator; it
adds no migration, changes no schema, and changes no public type
(`InitReport`, `SchemaState`, error codes). `voom init` and `ControlPlane::open`
callers are unaffected. No rollback procedure changes. `InitReport`'s field
*values* for a racing peer become more accurate under this change (see
Decision's `locked_before_count` discussion) — `voom-cli`'s `system init`
command (`crates/voom-cli/src/commands/system/init.rs`) forwards
`migrations_applied`/`already_initialized` verbatim into its JSON output, so
this is a user-visible correctness fix, not just an internal bookkeeping
detail.

## Test strategy and acceptance criteria

- `concurrent_init_stress` (existing) passes reliably: 50 consecutive
  standalone runs, plus two concurrent full-workspace `cargo test --workspace --quiet`
  invocations against the same checkout — the same reproduction methodology
  the Context section already used to surface #505 (49–53 losing-peer
  recoveries per run on an 18-core host). A single full-workspace run does
  not reliably generate the lock contention needed to exercise this fix's
  own new failure point (busy-timeout exhaustion on `BEGIN IMMEDIATE`, see
  Failure behavior), so it is not sufficient acceptance evidence on its own.
  Full-workspace contention is evidence that the ordinary-race path holds up
  under load, not a targeted check that the busy-timeout branch was hit or
  classified correctly — `concurrent_init_stress` only asserts per-peer
  success and final `migration_count`, so a pass is consistent with the
  busy-timeout branch never having been reached at all. See the dedicated
  unit test below for that branch specifically.
- A new unit test in `crates/voom-store/src/init_test.rs` forces busy-timeout
  exhaustion deterministically and directly, independent of incidental
  full-workspace contention and with **no real-time wait at all** — the same
  zero-wait pattern already proven in
  `crates/voom-store/src/repo/media/commit_safety_gate_test.rs`
  (`begin_gate_tx_emits_begin_immediate_and_takes_reserved_lock`): hold
  `BEGIN IMMEDIATE` open on one connection, then on a second connection run
  `PRAGMA busy_timeout = 0` before its own `begin_with("BEGIN IMMEDIATE")`
  call, so the lock contention surfaces as an immediate `SQLITE_BUSY` error
  with no sleep and no timing race. Assert that call returns a
  `VoomError::Database` wrapping the `sqlx::Error` from `database_context`,
  distinct from every other `VoomError` variant this path can produce
  (`DirtyMigration`, `SchemaTooNew`, generic `Migration`). A sleep-based
  short-`busy_timeout`-plus-real-wait design is explicitly rejected here: it
  would make the test's outcome depend on real elapsed time and task
  scheduling, reintroducing under CPU-starved CI the exact class of
  timing-dependent flakiness this fix exists to eliminate (`detect-curse`:
  "waiting on conditions, not on time" applies to the test as much as to the
  production path it verifies).
- `crates/voom-store/src/init_test.rs::migration_race_recovery_waits_for_slow_winner`
  (existing) is removed. It unit-tests `probe_after_failure`'s
  retry-with-backoff behavior directly — spawning a task that runs
  `MIGRATOR.run` a second later and asserting the probe waits for it — which
  is exactly the capability ADR 0068 concludes is no longer needed:
  `run_migrations_on`'s own call path never leaves a window where a
  concurrently-running peer could still be applying migrations, so there is
  nothing left for a probe to wait for. Removing the test is a direct
  consequence of removing the capability it tests, not an untested deletion.
- A new unit test in `crates/voom-store/src/init_test.rs` proves the
  single-shot replacement's contract: given a database already left in a
  non-terminal state (e.g. `_sqlx_migrations` created but no rows), the
  replacement classifies and returns on the first probe — no polling,
  observable via a bounded wall-clock assertion (e.g. completes in
  well under `MIGRATION_RACE_INITIAL_DELAY`'s old 25ms, since there is no
  sleep at all).
- A new integration test in `crates/voom-store/tests/init.rs` proves a peer
  that starts after another has fully migrated observes `Current`
  immediately with no calls into `apply()` — the direct assertion that the
  race is closed structurally, not just tolerated faster. `apply()` itself
  is sqlx-internal and unobservable through `voom-store`'s public API, so
  the test asserts it through two proxies instead: `InitReport.migrations_applied
  == 0` (a safe stand-in here specifically because this scenario is
  sequential — the second peer starts strictly after the first fully
  migrated, not racing it) and a bounded wall-clock assertion analogous to
  the unit test above, ruling out a hidden retry/backoff path.
- `concurrent_init_stress` (existing, extended) or a new dedicated integration
  test asserts `migrations_applied == 0` and `already_initialized == true` for
  a peer that genuinely raced another (started before the winner committed,
  then blocked on the lock) — distinct from the sequential-peer test above,
  which starts strictly after. This is the regression guard for
  `locked_before_count` (see Decision): without it, a blocked-then-no-op
  racing peer would report having applied every migration it never touched.
- A new unit test proves the all-or-nothing atomicity consequence directly:
  against a small, test-local `Migrator` (not the real 36-migration
  `MIGRATOR`) with two migrations where the second deliberately fails, run
  the same acquire-lock-then-`run_direct` pattern and assert the *first*
  migration's table is also absent afterward (not just that the call
  returns `Err`) — proving migration 1 was never durably committed on its
  own, only released as a `SAVEPOINT` inside the still-open outer
  transaction. This is the regression guard for ADR 0068's Consequences
  disclosure, not a test of the real production migration set.
- Existing single-peer `init()` tests (`init_on_disk_creates_schema_meta`,
  `second_init_against_same_disk_db_is_noop`, `init_is_idempotent_on_same_pool`,
  and the full existing `crates/voom-store` suite) continue to pass
  unchanged — the locked transaction is transparent to the non-concurrent
  path.
- `run_migrations_on`'s doc comments are updated to drop the stale "separate
  transactions" premise (ADR 0068's Context) and describe the new
  lock-then-migrate flow and its narrowed failure classification.
- `just ci` passes with zero failures and zero warnings.

## Dependencies and exclusions

In scope: `crates/voom-store/src/init.rs`'s migration-invocation path and its
doc comments; new/updated tests in `crates/voom-store/tests/init.rs` and/or
`crates/voom-store/src/init_test.rs`.

Excluded: `crates/voom-store/src/schema.rs` (no change needed — `probe_schema`'s
existing single-statement-atomic scan already closed the read-side TOCTOU in
issue #13, and its `Current`/`Dirty`/`TooNew`/`Partial`/`Uninitialized`
classification logic is reused unchanged, only called once instead of in a
loop); `crates/voom-store/src/migrator.rs` and `migrations/*.sql` (no new
migration or lock table); `crates/voom-api/src/server_test.rs` and anything
in the already-merged PR #506; raising `MIGRATION_RACE_RECOVERY_BUDGET` (ADR
0068 rejects this as the primary fix — the constant is removed entirely
along with the retry loop it bounded, not raised).
