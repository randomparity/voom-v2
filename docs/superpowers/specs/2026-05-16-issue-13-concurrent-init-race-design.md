# Fix concurrent_init race in probe_schema (issue #13)

## Problem

`crates/voom-store/tests/init.rs::concurrent_init_on_same_disk_db_is_safe`
flakes intermittently across both `ubuntu-latest` and `macos-latest`. The
loser of a race between two concurrent `init()` calls panics with:

```
config error: database appears to belong to another application: contains
table "schema_meta" but no _sqlx_migrations table. Refusing to migrate.
```

That error is emitted by `crates/voom-store/src/schema.rs:87` — the
foreign-DB guard that fires when a non-`sqlite_%` user table exists but
`_sqlx_migrations` does not.

## Root cause

The guard is implemented as two separate queries in `probe_schema`:

1. `schema.rs:61` — `SELECT COUNT(*) FROM sqlite_master WHERE name='_sqlx_migrations'`
2. `schema.rs:75` — `SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' LIMIT 1`

Each runs through `.fetch_one(pool)` / `.fetch_optional(pool)`, which
acquires a (possibly different) pool connection and runs the query in
its own implicit transaction. There is no snapshot binding the two
queries together — this is a classic TOCTOU.

Verified against sqlx-sqlite 0.8.6 source
(`sqlx-sqlite/src/migrate.rs`), the relevant peer ordering is:

- `Migrate::lock()` is a **no-op** for SQLite — concurrent migrators do
  **not** serialize.
- `ensure_migrations_table` runs `CREATE TABLE IF NOT EXISTS _sqlx_migrations`
  outside any transaction; it auto-commits and is immediately visible
  to every other connection.
- Per-migration `apply()` opens `BEGIN DEFERRED`, executes the user
  migration SQL (creates `schema_meta`), inserts the row, commits.

Race window that produces the observed failure:

1. Task B runs probe query 1 → returns 0 (`_sqlx_migrations` not yet
   created by either peer).
2. Task A races ahead: `ensure_migrations_table` commits (table now
   visible). Then `apply(0001)` commits (`schema_meta` now visible,
   row inserted).
3. Task B runs probe query 2 → sees `schema_meta`. The guard returns
   the Config error even though `_sqlx_migrations` exists by now.

The `LIMIT 1` in query 2 doesn't filter out `_sqlx_migrations` (it only
filters `sqlite_%`), so even after A commits, query 2 might still
return `schema_meta` rather than `_sqlx_migrations` — driving the false
positive.

## Solution

Collapse the two queries into one statement-atomic scan of
`sqlite_master`. A single SELECT in SQLite is guaranteed to see one
consistent snapshot for its full execution, so no TOCTOU is possible.

### Replacement query

```sql
SELECT
  COUNT(CASE WHEN name = '_sqlx_migrations' THEN 1 END)       AS has_migrations,
  MAX(CASE WHEN name != '_sqlx_migrations' THEN name END)      AS sample_foreign_table
FROM sqlite_master
WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
```

- `has_migrations` is `0` or `1`.
- `sample_foreign_table` is `NULL` when no non-`_sqlx_migrations` user
  table exists, otherwise some non-`_sqlx_migrations` table name.
  We only need a sample for the error message; `MAX` returns a stable,
  deterministic-per-snapshot value with no extra cost.

### Updated probe_schema flow

```
let (has_migrations, sample_foreign): (i64, Option<String>) = <atomic query>;

if has_migrations == 0 {
    if let Some(name) = sample_foreign {
        return Err(VoomError::Config(<existing foreign-DB message>));
    }
    return Ok(SchemaState::Uninitialized);
}

// rest unchanged: read _sqlx_migrations rows, classify
```

The rest of `probe_schema` (reading rows from `_sqlx_migrations`,
checksum-validating, classifying as `Partial` / `Current` / `TooNew` /
`Dirty`) is unaffected. Those reads can still observe later peer
commits, but the only outcomes are "see more or fewer rows than
expected" — which map cleanly to existing states and do not produce
false errors.

No changes needed in `init.rs`.

## Why not the originally proposed approaches

| Issue suggestion | Why it loses to this fix |
|---|---|
| Retry probe with backoff | Masks the bug; adds latency to genuine foreign-DB rejections; needs a tuned retry budget; doesn't help the read-side path (`connect()` could race the same way under load). |
| Treat foreign-DB error as advisory on recovery probe | Splits read-side semantics between init and read paths; doesn't fix the pre-MIGRATOR.run probe. |
| Tighten guard with `BEGIN IMMEDIATE` read | More invasive (lifecycle of a held connection/transaction) and conflicts with the existing pool-based `.fetch_one(pool)` pattern. |
| Switch to WAL | Contradicts the Sprint-0 decision documented in `crates/voom-store/src/pool.rs:40-44` and is a much larger change for a narrow bug. |

## Tests

### Existing coverage stays green

Every existing test exercising the guard / classifications continues to
hold (each is a single-process scenario; the new query returns the
same answers):

- `probe_returns_uninitialized_on_fresh_db`
- `probe_refuses_foreign_database_with_no_sqlx_migrations`
- `probe_returns_migration_error_on_malformed_sqlx_migrations_table`
- `probe_returns_too_new_on_renumbered_migration_at_same_count`
- All `init::tests::*`
- Integration test `connect_then_probe_leaves_db_uninitialized`

### Regression test (already exists)

`concurrent_init_on_same_disk_db_is_safe` — this is the test that
flaked. After the fix it should pass deterministically. Verify locally
with a loop:

```bash
for i in $(seq 1 50); do
  cargo test -p voom-store --test init concurrent_init_on_same_disk_db_is_safe \
    || { echo "flake on iteration $i"; break; }
done
```

### New stress test

Add a tightened version that drives more concurrent peers and runs the
race scenario multiple times in one invocation. This makes future
regressions visible in a single CI run rather than depending on
statistical luck.

```rust
// crates/voom-store/tests/init.rs
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_init_stress() {
    for _ in 0..20 {
        let tmp = NamedTempFile::new().unwrap();
        let url = sqlite_url_for(tmp.path());
        voom_store::connect_or_create(&url).await.unwrap();

        let handles: Vec<_> = (0..6)
            .map(|_| {
                let u = url.clone();
                tokio::spawn(async move { init(&u).await })
            })
            .collect();

        let mut reports = Vec::with_capacity(handles.len());
        for h in handles {
            reports.push(h.await.unwrap().unwrap());
        }

        // All peers see the same durable schema_init_at.
        let first = reports[0].schema_init_at;
        assert!(
            reports.iter().all(|r| r.schema_init_at == first),
            "peers disagreed on schema_init_at: {reports:?}"
        );
    }
}
```

Scope is bounded: 20 iterations × 6 peers takes ~1–2 s on developer
machines, fits within the project's existing CI budget.

## Scope / non-goals

- **In scope:** `probe_schema` single-query refactor; new stress test;
  comment in `probe_schema` recording the TOCTOU rationale (so a
  future refactor doesn't reintroduce the two-query form).
- **Out of scope:** changes to `init.rs`; changes to the read-side
  `connect()` flow; switching journal mode; adding retry helpers.

## Risk

Low. The change is one query in one function. The new query is
semantically equivalent for every settled (non-racing) state, and is
strictly more correct under concurrency. The error message format is
preserved, so CLI envelope snapshots are unaffected.

## File touch-list

- `crates/voom-store/src/schema.rs` — replace two queries with one atomic
  scan, update the comment block at lines 68–74 to record the TOCTOU
  fix.
- `crates/voom-store/tests/init.rs` — add `concurrent_init_stress`.
