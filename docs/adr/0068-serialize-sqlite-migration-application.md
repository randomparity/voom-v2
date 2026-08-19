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

### Discovered during implementation: PRAGMA foreign_keys is incompatible with any nesting depth

Implementing the held-lock design against the real 36-migration set surfaced a
second, independent defect this ADR's original text did not anticipate. Four
migrations (`0012`, `0013`, `0027`, `0034`) rebuild a table that other tables
hold live foreign-key rows against — required because SQLite has no `ALTER
TABLE` form for changing a `CHECK` constraint, so a changed `CHECK` can only be
applied by creating a new table and copying the data across. That rebuild
needs `PRAGMA foreign_keys = OFF` first (confirmed empirically: `DROP TABLE`
or `ALTER TABLE ... RENAME` on a table with live incoming FK rows fails
immediately with `FOREIGN KEY constraint failed` under `foreign_keys = ON`,
regardless of rename order), and SQLite refuses to toggle that PRAGMA while
*any* transaction is open, at *any* nesting depth — not just the sqlx-per-migration
transaction these migrations were originally written to step outside of, but
also the outer `BEGIN IMMEDIATE` this ADR now holds across the whole run. That
outer transaction's own held lock made the incompatibility worse than before:
reproducing it directly showed one of these migrations' inline `COMMIT;`
statement committing the *entire* outer transaction — silently releasing the
write lock mid-run — before sqlx's own bookkeeping `RELEASE SAVEPOINT` later
fails against a savepoint that no longer exists (`no such savepoint:
_sqlx_savepoint_1`).

No rewording of those four migrations' SQL resolves this: as long as any
migration touches an FK-referenced table with a changed `CHECK` constraint, it
needs `foreign_keys = OFF`, and that PRAGMA can never run inside this ADR's
held transaction. The resolution adopted here is a migration-history squash
(tracked under issue #505 alongside this ADR, since the codebase is
pre-release with no deployed data to preserve): the 36 sequential migration
files are replaced by a single `migrations/0001_schema.sql` that creates every
table once, directly in its final shape, generated from a one-time manual
diff of its applied result against the pre-squash 36-migration chain (schema
and seed data appeared byte-identical at the time). That comparison is not
retained as a repo artifact or regression test, and the pre-squash migration
files it was diffed against are deleted by this same change, so it cannot be
independently re-checked after merge — treat it as a one-time verification,
not an ongoing guarantee; `crates/voom-store/src/schema.rs`'s existing
CHECK/FK/vocabulary tests are what actually guard the new schema going
forward. With no rebuild step anywhere in migration history,
no migration needs to leave the transaction, and the held-lock design in
**Decision** below applies without exception. This also forecloses the same
failure recurring for any *future* migration that only ever needs
`CREATE TABLE`/`ALTER TABLE ADD COLUMN`-shaped changes.

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
- The 36 sequential migration files are squashed into one
  (`migrations/0001_schema.sql`); `expected_migrations()` (derived from
  `MIGRATOR.iter().count()`, no hand-maintained constant) now reports 1
  instead of 36 with no code changes required at its call sites. Any
  documentation or runbook referencing a specific historical migration number
  no longer corresponds to a file on disk.
- `crates/voom-store/tests/migration_inventory.rs` (1973 lines) is deleted.
  It tested that individual historical migrations correctly rewrote
  legacy-shaped data left by the migration before them (e.g. that migration
  0016 rewrote a pre-0016 `worker_grants.max_parallel` shape into the current
  one), using a `migrator_through(N)` helper that ran only the first `N` of
  the 36 files. With one migration file there is no "schema as of migration
  N" to seed against, and the squashed file contains no rewrite steps at all
  — only final table shapes — so the behavior these tests exercised no longer
  exists in the codebase to test.
- Anyone with a pre-existing local `voom.db` built against the pre-squash
  36-migration history (a developer's persistent database, a manual-test
  environment) will hit `SchemaState::TooNew { applied: 36, expected: 1 }` on
  the first `voom init` after pulling this change — its `_sqlx_migrations`
  table has rows for versions 2..36 that no longer exist in `MIGRATOR`. The
  resulting error message ("upgrade the voom binary or roll back the
  database") does not fit this cause: the binary is not behind, and there is
  no rollback path back to a 36-file history that no longer exists on disk.
  **Delete the local database file and let `voom init` recreate it** — this
  is the only real remedy, and it is safe since no such database is expected
  to hold anything but disposable local/test data.

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
- **Redesign the lock instead of squashing the migrations** (e.g. a separate
  advisory-lock mechanism held outside the migration transaction, letting
  `MIGRATOR` keep running un-nested via its normal `MIGRATOR.run(pool)` path).
  Rejected: this ADR's central claim — that holding one transaction across the
  whole migration run closes the race structurally — is simplest and most
  directly verifiable when migration application genuinely never leaves that
  transaction. An advisory lock taken on a separate connection reopens the
  window this ADR exists to close (a peer could observe the advisory lock
  released and the migration transaction not yet committed, or vice versa,
  depending on exactly when each connection's state becomes visible), trading
  a proven-shut race for a new one to re-verify from scratch.
- **Rewrite only the 4 offending migrations to avoid `PRAGMA
  legacy_alter_table`, keeping the rest of the 36-migration history.**
  Rejected: reordering the rename mechanics (create the replacement table
  under a temporary name, copy, drop the old one, rename into place) removes
  the need for `legacy_alter_table`, but not for `foreign_keys = OFF` — that
  PRAGMA is unavoidable for rebuilding a table other tables hold live FK rows
  against, confirmed empirically, and is equally forbidden inside any
  transaction depth. A partial rewrite would still fail nested under the held
  lock; a full squash was the smaller total change once foreign_keys was
  understood to be the real blocker rather than legacy_alter_table alone.
