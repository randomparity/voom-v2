# Runbook: Migration Rollback

VOOM migrations are **up-only**. The embedded `MIGRATOR` in `voom-store` ships
only `MigrationType::Simple` migrations — there are no down steps, and sqlx's
`migrate revert` is not available. Rolling back a schema change means restoring
the database from a backup taken before the upgrade, then running the older binary
against it.

## Upgrade ordering

**Always upgrade the binary before the database.**

- A new binary reading old rows tolerates absent optional fields (additive
  evolution under `#[serde(deny_unknown_fields)]`).
- An old binary reading rows written by a new binary will reject fields it does
  not recognize and fail loudly.
- A rollback across a payload-shape change (rename, remove, or retype a field)
  requires restoring the pre-upgrade database snapshot. The older binary will
  intentionally reject rows the newer binary wrote (ADR 0013).

This ordering is the same for upgrades and rollbacks:
**swap the binary first, then handle the database.**

## When to use this runbook

Use this runbook when you need to roll a VOOM installation back to a prior
release — typically because:

- The new binary introduced a regression and the fix is not yet available.
- A payload-shape change was deployed and the old binary cannot read the new rows.
- `voom health` reports `DB_SCHEMA_TOO_NEW` after a binary downgrade, confirming
  the schema is ahead of the binary.

## Procedure

### 1. Stop the binary

Stop all VOOM processes (CLI, daemon, workers) that hold open connections to the
database. No migration or restore step is safe while writers are active.

### 2. Confirm the current schema state

```bash
voom health
```

The response envelope includes `schema_state`. After a binary downgrade the
expected states are:

| `schema_state` | Meaning |
|---|---|
| `DB_SCHEMA_TOO_NEW` | Database has migrations the downgraded binary does not know. Restore required. |
| `current` | Schema matches the binary. No schema action needed. |
| `DB_DIRTY_MIGRATION` | A migration aborted mid-flight. See [Dirty migration recovery](#dirty-migration-recovery) below. |

If the state is already `current` after the binary swap, skip to step 5.

### 3. Restore the pre-upgrade database snapshot

Replace the database file with the backup taken before the upgrade:

```bash
# Stop all VOOM processes first (step 1).
cp /path/to/backup/voom.db.pre-upgrade /var/lib/voom/voom.db
```

If you use WAL mode (the default for a `voom init` database), copy both the
database file and any WAL/SHM sidecar files, or use a backup tool that produces
a consistent snapshot (e.g., `sqlite3 voom.db ".backup /path/to/backup.db"`).

Verify the restored file is intact:

```bash
sqlite3 /var/lib/voom/voom.db "PRAGMA integrity_check;"
# expected output: ok
```

### 4. Verify the schema matches the downgraded binary

Run `voom health` again with the downgraded binary against the restored database.
The response should show `schema_state: current`. If it shows `DB_PARTIAL_SCHEMA`,
the backup predates the version the binary expects — choose an older backup or
run `voom init` to apply the missing migrations forward.

### 5. Resume normal operation

Start VOOM processes normally. `connect()` opens the database without migrating;
only `voom init` applies migrations (ADR 0003). Do not run `voom init` unless you
intend to advance the schema.

## Dirty migration recovery

A `DB_DIRTY_MIGRATION` state means a migration ran far enough to insert a
`success=0` row in `_sqlx_migrations` and then aborted. sqlx refuses to run
further migrations over a dirty row. Two options:

**Option A — restore from backup (preferred).**
Follow steps 3–5 above to replace the database with a pre-upgrade snapshot.

**Option B — remove the failed row manually.**
Use this only if you have confirmed the migration left no partial schema changes
(e.g., the migration failed before any DDL executed). The error envelope names
the failed version:

```bash
sqlite3 /var/lib/voom/voom.db \
  "DELETE FROM _sqlx_migrations WHERE version = <failed_version>;"
voom init
```

After the delete, `voom init` retries the failed migration from scratch.

## No-backup scenario

If no backup exists, the database cannot be rolled back to an earlier schema
version. Options:

1. **Forward-fix:** ship a new binary version that is compatible with the current
   schema.
2. **Wipe and reinitialize:** delete the database file, run `voom init`, and
   reload data from source. Appropriate only for non-production environments.

## Backup recommendations

Take a SQLite backup immediately before every upgrade:

```bash
sqlite3 /var/lib/voom/voom.db \
  ".backup /path/to/backup/voom.db.$(date +%Y%m%dT%H%M%S)"
```

The `sqlite3 .backup` command produces a consistent snapshot even on a live
database by using SQLite's online backup API. Store the snapshot outside the
database directory so a filesystem issue does not affect both.
