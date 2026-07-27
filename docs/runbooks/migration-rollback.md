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

Run the downgraded binary with the same database configuration that production
uses. Capture stdout and the exit code separately; piping `voom health`
directly into `jq` would replace the CLI exit code with `jq`'s exit code.

```bash
set -euo pipefail

health_json="$(mktemp -t voom-health.XXXXXX)"
trap 'rm -f "$health_json"' EXIT

health_exit=0
voom health >"$health_json" || health_exit=$?

case "$health_exit" in
  0)
    if ! jq -e '
.schema_version == "0" and
.command == "health" and
.status == "ok" and
.data.db.status == "current" and
.error == null
' "$health_json" >/dev/null; then
      echo "voom health returned exit 0 with an unexpected envelope" >&2
      exit 2
    fi
    health_state="current"
    ;;
  2)
    health_state="$(jq -er '.error.code | select(type == "string")' "$health_json")"
    case "$health_state" in
      DB_SCHEMA_TOO_NEW | DB_PARTIAL_SCHEMA | DB_DIRTY_MIGRATION) ;;
      *)
        echo "voom health returned an unexpected error code: $health_state" >&2
        exit 2
        ;;
    esac
    if ! jq -e --arg code "$health_state" '
.schema_version == "0" and
.command == "health" and
.status == "error" and
.data == null and
.error.code == $code
' "$health_json" >/dev/null; then
      echo "voom health returned exit 2 with an unexpected envelope" >&2
      exit 2
    fi
    ;;
  *)
    echo "voom health returned unexpected exit code $health_exit" >&2
    exit 2
    ;;
esac

printf 'rollback health state: %s\n' "$health_state"
```

The accepted results and next actions are:

| Exit | JSON predicate | Meaning and next action |
|---|---|---|
| `0` | `.status == "ok"` and `.data.db.status == "current"` | Schema matches the selected binary. Skip to step 5. |
| `2` | `.status == "error"` and `.error.code == "DB_SCHEMA_TOO_NEW"` | Database is ahead of the selected binary. Continue to step 3. |
| `2` | `.status == "error"` and `.error.code == "DB_PARTIAL_SCHEMA"` | Database is behind the selected binary or its migration metadata is damaged. Do **not** choose an older backup; follow [Partial schema recovery](#partial-schema-recovery). |
| `2` | `.status == "error"` and `.error.code == "DB_DIRTY_MIGRATION"` | A migration aborted mid-flight. Follow [Dirty migration recovery](#dirty-migration-recovery). |

If the state is already `current` after the binary swap, skip to step 5.

### 3. Restore one pre-upgrade database snapshot

`DB_SCHEMA_TOO_NEW` establishes that the active database is ahead of the
selected binary. Replace it with the newest snapshot taken before the
incompatible upgrade:

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

Run the complete diagnosis block from step 2 again with the downgraded binary
against the restored database:

- `current`: the candidate is compatible; continue to step 5.
- `DB_SCHEMA_TOO_NEW`: this candidate is still ahead of the binary. Try the
  next earlier pre-upgrade snapshot, then repeat the integrity check and
  diagnosis.
- `DB_PARTIAL_SCHEMA`: stop moving backward. The candidate is behind the
  binary or damaged; an even older snapshot cannot make it compatible. Follow
  [Partial schema recovery](#partial-schema-recovery).
- `DB_DIRTY_MIGRATION`: do not infer a safe direction from snapshot age.
  Follow [Dirty migration recovery](#dirty-migration-recovery), or reject this
  candidate and diagnose a separately known-consistent snapshot from the
  beginning.

This brackets the compatible schema: moving to earlier snapshots is allowed
only while the result remains `DB_SCHEMA_TOO_NEW`. Never continue to
progressively older snapshots after `DB_PARTIAL_SCHEMA`.

### 5. Resume normal operation

Start VOOM processes normally. `connect()` opens the database without migrating;
only `voom init` applies migrations (ADR 0003). Do not run `voom init` unless you
intend to advance the schema.

## Partial schema recovery

`DB_PARTIAL_SCHEMA` has two forms. Inspect the emitted diagnostic:

```bash
jq -r '.error.message, (.error.hint // "")' "$health_json"
```

- If the hint explicitly says `Run voom init against the current binary`, the
  snapshot has a clean older migration set. Keep an untouched copy of the
  snapshot, make a working copy, run `voom init` with the selected downgraded
  binary to apply its missing migrations forward, and then repeat the
  integrity check and step 2 diagnosis. Resume only after `current`.
- If the message or hint reports corrupted or incompatible migration metadata
  (for example, a missing or malformed `schema_meta` table), `voom init` is not
  a repair path. Restore a newer known-consistent pre-upgrade snapshot or
  perform a deliberate metadata repair. Diagnose the result again.

Do not choose an older snapshot in response to `DB_PARTIAL_SCHEMA`; that moves
away from the selected binary's required schema.

## Dirty migration recovery

A `DB_DIRTY_MIGRATION` state means a migration ran far enough to insert a
`success=0` row in `_sqlx_migrations` and then aborted. sqlx refuses to run
further migrations over a dirty row. Two options:

**Option A — restore from backup (preferred).**
Replace the database with a known-consistent pre-upgrade snapshot, then run its
integrity check and the complete step 2 diagnosis. Snapshot age alone is not
evidence of consistency. Resume only after the selected binary reports
`current`.

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
