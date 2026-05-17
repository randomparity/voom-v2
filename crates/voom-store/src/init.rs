use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_events::{EventKind, SubjectType, payload::SchemaInitializedPayload};

use crate::migrator::MIGRATOR;
use crate::pool::connect_or_create;
use crate::repo::common::iso8601;
use crate::schema::{SchemaState, probe_schema};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    pub migrations_applied: u32,
    pub schema_init_at: OffsetDateTime,
    pub already_initialized: bool,
}

/// Open the pool (creating the database file and parent dirs if necessary) and
/// apply any pending migrations. Idempotent. This is the **only** production
/// entry point allowed to create filesystem state or mutate schema.
pub async fn init(url: &str) -> Result<InitReport, VoomError> {
    let pool = connect_or_create(url).await?;
    run_migrations_on(&pool).await
}

/// Run migrations on an already-open pool. **Test-only public surface** —
/// gated behind the `test-support` feature so production crates cannot reach
/// it. Use `init(url)` in production code.
#[cfg(any(test, feature = "test-support"))]
pub async fn init_on(pool: &SqlitePool) -> Result<InitReport, VoomError> {
    run_migrations_on(pool).await
}

async fn run_migrations_on(pool: &SqlitePool) -> Result<InitReport, VoomError> {
    let before = probe_schema(pool).await?;

    // Defensive: never run migrations against a DB whose schema is ahead of
    // this binary.
    if let SchemaState::TooNew { applied, expected } = before {
        return Err(VoomError::SchemaTooNew(format!(
            "cannot init: database has {applied} migrations applied but this binary ships \
             {expected}; upgrade the voom binary or roll back the database"
        )));
    }

    // Dirty migration rows require manual cleanup — sqlx refuses to migrate
    // over them, so a generic `voom init` rerun would just fail again. Surface
    // a precise pointer and remediation path instead.
    if let SchemaState::Dirty {
        failed_version,
        applied,
        expected,
    } = before
    {
        return Err(VoomError::DirtyMigration(format!(
            "cannot init: migration version {failed_version} is recorded as failed \
             (success=0) in _sqlx_migrations ({applied}/{expected} successful); sqlx \
             will not run further migrations over a dirty schema. Remove the failed \
             row manually (e.g. `DELETE FROM _sqlx_migrations WHERE version = \
             {failed_version}`) or restore from backup before re-running voom init"
        )));
    }

    let before_count: u32 = match &before {
        SchemaState::Uninitialized => 0,
        SchemaState::Partial { applied, .. }
        | SchemaState::TooNew { applied, .. }
        | SchemaState::Dirty { applied, .. } => *applied,
        SchemaState::Current {
            migration_count, ..
        } => *migration_count,
    };
    let already_initialized = matches!(before, SchemaState::Current { .. });

    let migrate_result = MIGRATOR.run(pool).await;

    if let Err(e) = migrate_result {
        // Re-probe and classify by the post-error state, not the raw sqlx
        // error. This handles three distinct scenarios that all surface as
        // a `MigrateError` from sqlx but mean different things to operators:
        //
        // * `Current`  — race recovery. Between our pre-init probe and the
        //                migration run, another process applied the same
        //                migrations. Treat as idempotent success.
        // * `Dirty`    — a migration ran far enough to insert a success=0
        //                row in `_sqlx_migrations`, then failed. sqlx will
        //                refuse to retry; surface as DB_DIRTY_MIGRATION so
        //                operators perform manual cleanup instead of just
        //                re-running init.
        // * `TooNew`   — schema is now ahead of this binary (rare after a
        //                run-time failure, but possible if a concurrent
        //                peer migrated past us). Surface as
        //                DB_SCHEMA_TOO_NEW so operators upgrade the binary.
        // * otherwise  — propagate the original sqlx error as a generic
        //                Migration (DB_PARTIAL_SCHEMA) so the message
        //                surfaces verbatim.
        let after = probe_schema(pool).await?;
        return match after {
            SchemaState::Current {
                schema_init_at,
                migration_count,
            } => {
                // Race recovery doesn't tell us whether the other process
                // also emitted `schema.initialized` — they may have applied
                // migrations and then crashed before the event append. Run
                // the same atomic emit-if-missing as the happy path so the
                // row is present regardless. The statement is a no-op when
                // the row already exists, so the cost of always running it
                // is one indexed lookup.
                emit_schema_initialized_if_missing(pool, migration_count, schema_init_at).await?;
                Ok(InitReport {
                    migrations_applied: 0,
                    schema_init_at,
                    already_initialized: true,
                })
            }
            SchemaState::Dirty {
                failed_version,
                applied,
                expected,
            } => Err(VoomError::DirtyMigration(format!(
                "migration failed and left version {failed_version} recorded \
                 as failed (success=0) in _sqlx_migrations ({applied}/{expected} \
                 successful). sqlx will not retry over a dirty schema. Remove \
                 the failed row manually (DELETE FROM _sqlx_migrations WHERE \
                 version = {failed_version}) or restore from backup. \
                 (underlying error: {e})"
            ))),
            SchemaState::TooNew { applied, expected } => Err(VoomError::SchemaTooNew(format!(
                "migration failed and post-probe shows schema is now too new for \
                 this binary ({applied}/{expected}). Upgrade the voom binary or \
                 roll back the database. (underlying error: {e})"
            ))),
            _ => Err(VoomError::Migration(format!(
                "running migrations failed: {e}"
            ))),
        };
    }

    let after = probe_schema(pool).await?;
    let SchemaState::Current {
        migration_count,
        schema_init_at,
    } = after
    else {
        return Err(VoomError::Migration(format!(
            "post-init schema state is not Current: {after:?}"
        )));
    };

    let migrations_applied = migration_count.saturating_sub(before_count);

    // Recovery-safe emit: a single INSERT ... WHERE NOT EXISTS statement is
    // atomic under SQLite's single-writer locking, so the existence check
    // and the insert cannot race against a concurrent init. If a prior
    // call applied migrations but failed (or crashed) before the event was
    // durably appended, the next call re-emits the missing row; if two
    // calls run simultaneously, the first one inserts and the second sees
    // the row already there. Exactly one row regardless of races or
    // partial-failure retries. The `events` table has no UNIQUE constraint
    // on `kind`, so this statement is the only thing keeping the
    // single-row invariant.
    //
    // The payload's `migrations_applied` is the absolute `migration_count`
    // at emit time so the recovery write carries the same snapshot value
    // a fresh init would have produced (on a fresh init these are equal;
    // on recovery the per-call delta is zero and useless).
    emit_schema_initialized_if_missing(pool, migration_count, schema_init_at).await?;

    Ok(InitReport {
        migrations_applied,
        schema_init_at,
        already_initialized,
    })
}

async fn emit_schema_initialized_if_missing(
    pool: &SqlitePool,
    migrations_applied: u32,
    schema_init_at: OffsetDateTime,
) -> Result<(), VoomError> {
    // `SchemaInitializedPayload` serializes directly to the inner-payload
    // shape the events table stores; `kind` lives in its own column, so
    // we deliberately bypass the `Event` tag wrapper here. The `events`
    // table column order is (occurred_at, kind, subject_type, subject_id,
    // trace_id, payload).
    let payload_json = serde_json::to_string(&SchemaInitializedPayload {
        migrations_applied,
        schema_init_at,
    })
    .map_err(|e| VoomError::Internal(format!("payload serialize: {e}")))?;
    let occurred = iso8601(schema_init_at)?;

    sqlx::query(
        "INSERT INTO events (occurred_at, kind, subject_type, subject_id, trace_id, payload) \
         SELECT ?, ?, ?, NULL, NULL, ? \
         WHERE NOT EXISTS (SELECT 1 FROM events WHERE kind = ?)",
    )
    .bind(occurred)
    .bind(EventKind::SchemaInitialized.as_str())
    .bind(SubjectType::System.as_str())
    .bind(payload_json)
    .bind(EventKind::SchemaInitialized.as_str())
    .execute(pool)
    .await
    .map_err(|e| VoomError::Database(format!("schema.initialized append: {e}")))?;
    Ok(())
}

#[cfg(test)]
#[path = "init_test.rs"]
mod tests;
