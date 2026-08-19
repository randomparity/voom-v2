use sqlx::Connection;
use sqlx::SqlitePool;
use sqlx::migrate::Migrate;
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
/// available only when the `test` feature is enabled. Production targets must
/// not enable that feature; use `init(url)` in production code.
#[cfg(any(test, feature = "test"))]
pub async fn init_on(pool: &SqlitePool) -> Result<InitReport, VoomError> {
    run_migrations_on(pool).await
}

/// Run migrations on `pool` behind a held `BEGIN IMMEDIATE` write lock (ADR
/// 0068). Taking `SQLite`'s write lock immediately, rather than deferring it to
/// the first write statement, means a losing peer blocks on lock acquisition
/// (governed by the pool's `busy_timeout`) instead of racing an `apply()`
/// call it cannot win. Once a blocked peer acquires the lock, its own
/// `list_applied_migrations()` read happens inside its now-current
/// transaction and sees every migration the prior lock-holder committed, so
/// `run_direct`'s loop finds nothing left to apply and returns `Ok(())` with
/// zero migrations applied — the race is closed structurally, not
/// out-waited, and no post-failure recovery probe is needed for it.
async fn run_migrations_on(pool: &SqlitePool) -> Result<InitReport, VoomError> {
    let before = probe_schema(pool).await?;
    reject_unmigratable_schema(&before)?;

    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| VoomError::database_context("acquire connection for migration", e))?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| VoomError::database_context("acquire migration write lock", e))?;
    tx.ensure_migrations_table()
        .await
        .map_err(|e| VoomError::database_context("ensure migrations table under lock", e))?;
    let locked_applied = tx
        .list_applied_migrations()
        .await
        .map_err(|e| VoomError::database_context("read applied migrations under lock", e))?;
    let locked_before_count = u32::try_from(locked_applied.len()).unwrap_or(u32::MAX);

    if let Err(e) = MIGRATOR.run_direct(&mut *tx).await {
        drop(tx); // rolls back, releasing the write lock
        drop(conn); // return the connection before probing the pool
        let after = probe_schema(pool).await?;
        return Err(classify_migration_failure(&after, &e));
    }

    tx.commit()
        .await
        .map_err(|e| VoomError::database_context("commit migration transaction", e))?;
    drop(conn); // return the connection before probing the pool

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

    let migrations_applied = migration_count.saturating_sub(locked_before_count);
    let already_initialized = migrations_applied == 0;

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

/// Reject a pre-migration schema state that `run_direct` should never be
/// allowed to touch: a schema ahead of this binary, or one left dirty by a
/// prior failed migration attempt. `Ok(())` for every other state.
fn reject_unmigratable_schema(before: &SchemaState) -> Result<(), VoomError> {
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

    Ok(())
}

/// Classify a `run_direct` failure by re-probing the schema it left behind.
/// Held-lock migration means this is reached only for a genuine failure
/// (bad SQL, disk error, corruption) or a lock-acquisition timeout — never
/// for the routine race a prior design used to recover from here.
fn classify_migration_failure(after: &SchemaState, e: &sqlx::migrate::MigrateError) -> VoomError {
    match after {
        SchemaState::Dirty {
            failed_version,
            applied,
            expected,
        } => VoomError::DirtyMigration(format!(
            "migration failed and left version {failed_version} recorded \
             as failed (success=0) in _sqlx_migrations ({applied}/{expected} \
             successful). sqlx will not retry over a dirty schema. Remove \
             the failed row manually (DELETE FROM _sqlx_migrations WHERE \
             version = {failed_version}) or restore from backup. \
             (underlying error: {e})"
        )),
        SchemaState::TooNew { applied, expected } => VoomError::SchemaTooNew(format!(
            "migration failed and post-probe shows schema is now too new for \
             this binary ({applied}/{expected}). Upgrade the voom binary or \
             roll back the database. (underlying error: {e})"
        )),
        _ => VoomError::Migration(format!("running migrations failed: {e}")),
    }
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
    .map_err(|e| VoomError::database_context("schema.initialized append", e))?;
    Ok(())
}

#[cfg(test)]
#[path = "init_test.rs"]
mod tests;
