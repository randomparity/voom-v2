use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::VoomError;

use crate::migrator::MIGRATOR;
use crate::pool::connect_or_create;
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
            SchemaState::Current { schema_init_at, .. } => Ok(InitReport {
                migrations_applied: 0,
                schema_init_at,
                already_initialized: true,
            }),
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

    Ok(InitReport {
        migrations_applied,
        schema_init_at,
        already_initialized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::connect;
    use crate::schema::{expected_migrations, probe_schema};

    #[tokio::test]
    async fn init_in_memory_applies_one_migration() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let report = init_on(&pool).await.unwrap();
        assert!(!report.already_initialized);
        assert_eq!(report.migrations_applied, 1);
    }

    #[tokio::test]
    async fn init_is_idempotent_on_same_pool() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let first = init_on(&pool).await.unwrap();
        let second = init_on(&pool).await.unwrap();
        assert!(!first.already_initialized);
        assert!(second.already_initialized);
        assert_eq!(second.migrations_applied, 0);
    }

    #[tokio::test]
    async fn init_refuses_when_db_schema_is_too_new() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let err = init_on(&pool).await.unwrap_err();
        assert_eq!(err.code(), "DB_SCHEMA_TOO_NEW");
        assert!(format!("{err}").contains("cannot init"));
    }

    #[tokio::test]
    async fn probe_after_init_then_extra_row_returns_too_new() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        match probe_schema(&pool).await.unwrap() {
            SchemaState::TooNew { applied, expected } => {
                assert_eq!(expected, expected_migrations());
                assert!(applied > expected);
            }
            other => panic!("expected TooNew, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_after_init_then_checksum_mutation_returns_too_new() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();
        assert!(matches!(
            probe_schema(&pool).await.unwrap(),
            SchemaState::Current { .. }
        ));

        sqlx::query("UPDATE _sqlx_migrations SET checksum = X'DEADBEEF' WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();

        match probe_schema(&pool).await.unwrap() {
            SchemaState::TooNew { applied, expected } => {
                assert_eq!(applied, expected, "count unchanged; only checksum differs");
            }
            other => panic!("expected TooNew (checksum drift), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_returns_dirty_when_known_version_row_marked_failed() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();

        match probe_schema(&pool).await.unwrap() {
            SchemaState::Dirty {
                failed_version,
                applied,
                expected,
            } => {
                assert_eq!(
                    failed_version, 1,
                    "failed_version must point at the dirty row"
                );
                assert_eq!(applied, 0, "no successful migrations remain");
                assert_eq!(expected, expected_migrations());
            }
            other => panic!("expected Dirty, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn init_refuses_when_schema_is_dirty() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        // Synthesize a dirty state: known version with success=0.
        sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();

        let err = init_on(&pool).await.unwrap_err();
        assert_eq!(err.code(), "DB_DIRTY_MIGRATION");
        let msg = format!("{err}");
        assert!(
            msg.contains("version 1"),
            "error must name the dirty version: {msg}"
        );
        assert!(
            msg.contains("DELETE FROM _sqlx_migrations") || msg.contains("restore"),
            "error must point at manual remediation: {msg}"
        );
    }

    #[tokio::test]
    async fn probe_returns_too_new_when_failed_unknown_version_row_present() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'failed-future', strftime('%s','now'), 0, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        match probe_schema(&pool).await.unwrap() {
            SchemaState::TooNew { applied, .. } => {
                assert_eq!(applied, expected_migrations(), "only successful row counts");
            }
            other => panic!("expected TooNew, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_after_init_then_corrupt_schema_meta_returns_migration_error() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query("DROP TABLE schema_meta")
            .execute(&pool)
            .await
            .unwrap();

        let err = probe_schema(&pool).await.unwrap_err();
        assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
    }

    #[tokio::test]
    async fn probe_after_init_then_corrupt_schema_init_at_value_returns_migration_error() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query(
            "UPDATE schema_meta SET value = 'not-a-timestamp' WHERE key = 'schema_init_at'",
        )
        .execute(&pool)
        .await
        .unwrap();

        let err = probe_schema(&pool).await.unwrap_err();
        assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
    }

    #[tokio::test]
    async fn init_from_partial_state_reports_delta_not_total() {
        // Synthesize a Partial state: _sqlx_migrations exists but has zero
        // success rows. In Sprint 0 the delta equals the total; this pins
        // the delta-counting code path so Sprint 1+ can add a real partial
        // case without rewriting init logic.
        let pool = connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE _sqlx_migrations ( \
             version BIGINT PRIMARY KEY, \
             description TEXT NOT NULL, \
             installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
             success BOOLEAN NOT NULL, \
             checksum BLOB NOT NULL, \
             execution_time BIGINT NOT NULL \
             )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let report = init_on(&pool).await.unwrap();
        assert!(!report.already_initialized);
        assert_eq!(report.migrations_applied, 1);
    }
}
