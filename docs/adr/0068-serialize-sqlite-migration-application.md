# ADR 0068: Serialize SQLite migration application with a held write lock

## Status

Accepted

## Context

`voom_store::init()` lets multiple peers (concurrent CLI invocations, or
concurrent test tasks against one on-disk database) call `MIGRATOR.run(pool)`
against the same SQLite file at once. Issue #505 reported
`concurrent_init_stress` intermittently failing under full-workspace test
contention with `table events already exists`, after `probe_after_failure`'s
30-second recovery budget was exhausted.

`sqlx`'s `Migrator` exposes a `locking` flag specifically to serialize
concurrent migrators, but for the SQLite backend `SqliteConnection::lock()`
and `unlock()` are no-ops (verified directly against the vendored
`sqlx-sqlite-0.8.6` source: `impl Migrate for SqliteConnection` returns
`Ok(())` from both without touching the database). So today there is no real
mutual exclusion during migration — every "losing" peer racing another init()
is expected, not exceptional, and `probe_after_failure`'s retry loop is the
only thing standing between that race and a hard failure.

This is not this project's first pass at this problem. Commit `72381557`
added `probe_after_failure` when the M2 migration set doubled the SQL applied
per init and first exposed the race; commit `4a98b16a` later raised its
budget from ~775ms to the current 30 seconds because "under load that can
take seconds even when each individual transaction is short." Issue #505 is
the same failure mode recurring a third time as the migration set has grown
to 36 files. Reproducing #505 under two concurrent full-workspace
`cargo test --workspace --quiet` runs (18-core host) confirmed the pattern
directly: 49–53 losing-peer recoveries per run, none ever stuck, but a
successful (non-racing) `MIGRATOR.run` already took up to 8–9 seconds under
that contention — roughly 30% of the existing budget consumed by ordinary,
non-pathological migration work.

`probe_after_failure`'s own doc comment also states that sqlx applies a
migration's DDL and its `_sqlx_migrations` row insert "as a separate
statement" (implying two transactions), which was true for the sqlx version
in place when `72381557` was written. It is not true of the currently
vendored `sqlx-sqlite-0.8.6`: `SqliteConnection::apply()` wraps the migration
SQL and the bookkeeping insert in one transaction (upstream fix for
`launchbadge/sqlx#1966`). The recovery loop still works — it tolerates any
non-terminal state, regardless of cause — but its own justification has been
stale since a sqlx upgrade nobody re-derived it against.

Design doc: [`docs/specs/concurrent-init-migration-locking-505.md`](../specs/concurrent-init-migration-locking-505.md).

## Decision

`run_migrations_on` acquires one pooled connection and opens it with
`conn.begin_with("BEGIN IMMEDIATE")` before calling
`MIGRATOR.run_direct(&mut *tx)`, then commits that transaction. `BEGIN
IMMEDIATE` takes SQLite's write lock immediately rather than deferring it to
the first write statement, so a losing peer blocks on lock acquisition
(governed by the pool's existing 30-second `busy_timeout`) instead of running
`list_applied_migrations()` against a stale snapshot and racing an `apply()`
call it cannot win. `apply()`'s own per-migration `self.begin()` calls become
nested `SAVEPOINT`s inside the held transaction — ordinary sqlx behavior for
`begin()` on an already-transacting connection — so the loop inside
`run_direct` needs no changes.

Once a blocked peer acquires the lock, its own `list_applied_migrations()`
read happens inside its now-current transaction, sees every migration the
prior lock-holder committed, and `run_direct`'s loop finds nothing left to
apply. It returns `Ok(())` with zero migrations applied and never reaches the
`table X already exists` error path at all. The race is closed structurally,
not out-waited.

With real locking in place, `probe_after_failure`'s retry-with-backoff loop
loses its justification and is replaced by a single-shot re-probe. The loop
existed to wait for a *different, concurrently-running* peer to finish
migrating so its now-visible terminal state could be observed. Once
migration application is exclusive, that scenario cannot occur: a failure
reaching this branch happened while *this* peer held the only write lock, so
no other peer could have been concurrently applying migrations that would
later resolve the state. Re-probing after such a failure returns the same
stable, already-rolled-back state on the first attempt every time; retrying
it for up to 30 seconds only delays surfacing a genuine, likely-deterministic
failure (bad migration SQL, disk error, corruption) by up to 30 seconds for
no benefit. Lock-acquisition contention itself (a peer that cannot get
`BEGIN IMMEDIATE` within the pool's `busy_timeout`) is a separate, already-handled
case: SQLite's own busy handler retries internally for the configured
`busy_timeout` duration before `begin_with` returns an error, so no
additional application-level retry loop is needed there either. The
`MIGRATION_RACE_RECOVERY_BUDGET`/`MIGRATION_RACE_INITIAL_DELAY`/`MIGRATION_RACE_MAX_DELAY`
constants and the backoff loop are removed; the single re-probe reuses the
existing `Current`/`Dirty`/`TooNew`/generic-`Migration` classification.

## Consequences

- The common case (N peers calling `init()` concurrently against one file)
  no longer produces any migration error; N-1 peers simply block on the
  write lock and then no-op through an already-current schema.
- `run_migrations_on`'s error branch becomes reachable only by a genuine,
  non-race migration failure while holding the lock, or by lock-acquisition
  timeout — never by the routine race. It reports either case immediately
  instead of after a 30-second wait. The two are distinguishable: a
  lock-acquisition timeout maps to `VoomError::Database` via
  `VoomError::database_context`, the same convention this crate's
  `commit_safety_gate::begin_gate_tx` already uses for a failed
  `begin_with("BEGIN IMMEDIATE")` (`crates/voom-store/src/repo/media/commit_safety_gate.rs`),
  and is never mistaken for `DirtyMigration`/`SchemaTooNew`/generic
  `Migration`, which remain reserved for a failure classified from
  `probe_schema` after the lock was actually held.
- `init()`'s worst-case latency changes shape: instead of every peer racing
  to fail-and-recover in parallel, N-1 peers serialize behind the lock
  holder. Aggregate wall-clock work is comparable (SQLite was already a
  single writer under the hood); peers no longer pay for a doomed `apply()`
  attempt and its subsequent probe-and-backoff cycle.
- Wrapping the whole batch in one outer transaction changes failure
  atomicity. Today, each migration's `apply()` commits independently (a
  real `BEGIN`/`COMMIT` per migration), so a failure at migration N leaves
  migrations 1..N-1 durably applied. Under this decision, `apply()`'s
  per-migration `begin()`/commit becomes a nested `SAVEPOINT`/`RELEASE` —
  provisional until the single outer `COMMIT` — so a genuine failure at
  migration N rolls back migrations 1..N-1 from the same run too; the next
  `init()` reapplies the whole batch from the pre-run state. This is a
  stronger, not weaker, guarantee (no schema is ever left half-applied
  across a process boundary) at the cost of redoing deterministic, cheap
  DDL on retry. One concrete effect: `SchemaState::Dirty` becomes
  effectively unreachable for a *freshly-introduced* failure under this
  path — `apply()` never writes a `success = false` row (confirmed in the
  vendored source), and an all-or-nothing rollback returns the DB to its
  pre-run `Partial`/`Uninitialized` state rather than a dirty one, so a
  fresh failure now surfaces through the generic `Migration` error instead.
  `DirtyMigration`'s remediation path remains reachable only for a dirty
  row left over from before this fix, or from direct manual tampering with
  `_sqlx_migrations`.
- The fix is local to `run_migrations_on`'s migration invocation. `probe_schema`,
  `emit_schema_initialized_if_missing`, and every caller-facing type
  (`InitReport`, `SchemaState`) are unchanged.
- `connect()`'s no-create, no-migrate contract (ADR 0003) is unaffected —
  the lock is only ever taken from inside `init()`'s migration path.

## Considered & rejected

- **Do nothing beyond fixing the stale doc comment.** The reproduction in
  Context recovered every one of ~100 losing-peer events across both runs
  within budget (worst case 8.75s of 30s), so it demonstrates the race
  occurring and recovering, not the reported exhaustion itself. Rejected
  anyway: the migration count driving `MIGRATOR.run`'s duration has gone
  4 (commit `72381557`, 2026-05-17) → 25 (commit `4a98b16a`, 2026-07-27) →
  36 (today, 2026-08-19) — accelerating, not merely growing, over roughly
  three months. At today's count a successful run alone already consumes
  ~30% of the fixed budget under twice-the-reproduction contention; the
  next migration-set doubling, on the same trend, plausibly exhausts it the
  same way the second bump was needed once the first one's headroom ran
  out. Leaving the budget as the only defense repeats a pattern that has
  already failed twice.
- **Raise `MIGRATION_RACE_RECOVERY_BUDGET` again** (e.g. 30s → 90s). This is
  the same move `4a98b16a` already made once (775ms → 30s) for the same
  underlying reason; the reproduction here shows the pattern recurring as the
  migration set grows, not a one-time fluke. A bigger fixed budget still
  doesn't serialize anything — it just widens the window in which every peer
  independently races, fails, and recovers, and needs re-tuning again the
  next time the migration set grows or CI gets busier.
- **Tighten the `probe_schema` read side with a locked read instead.** This
  is the alternative the #13 design doc
  (`docs/superpowers/specs/2026-05-16-issue-13-concurrent-init-race-design.md`)
  already rejected as "more invasive... conflicts with the existing
  pool-based `.fetch_one(pool)` pattern." That rejection was scoped to
  `probe_schema`'s read-only TOCTOU guard, a different problem from this
  one (serializing the write-side migration application), so it doesn't
  settle this decision — but the same objection would apply if this fix
  tried to add locking to the read path instead of the write path it
  actually belongs to.
- **A dedicated lock table plus manual `SELECT ... FOR UPDATE`-style
  polling.** SQLite has no row-level locking primitive to make that
  meaningful; it would reimplement `BEGIN IMMEDIATE`'s effect with more code
  and a new migration for no benefit.
- **Keep `probe_after_failure`'s retry-with-backoff loop as a defense-in-depth
  fallback.** Rejected: once migration application is exclusive, no failure
  inside the lock can be caused by a concurrent peer completing later,
  because no other peer could have been concurrently applying migrations.
  Retrying only delays a deterministic failure's surfacing by up to 30
  seconds. The classification step itself (`Current`/`Dirty`/`TooNew`/generic
  `Migration`) is still needed and is kept — only the retry loop and its
  budget constants are removed, as a single probe already returns a stable
  answer.
