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

    let before_count: u32 = match &before {
        SchemaState::Uninitialized => 0,
        SchemaState::Partial { applied, .. } | SchemaState::TooNew { applied, .. } => *applied,
        SchemaState::Current {
            migration_count, ..
        } => *migration_count,
    };
    let already_initialized = matches!(before, SchemaState::Current { .. });

    MIGRATOR
        .run(pool)
        .await
        .map_err(|e| VoomError::Migration(format!("running migrations failed: {e}")))?;

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
    async fn probe_returns_partial_when_known_version_row_marked_failed() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();

        match probe_schema(&pool).await.unwrap() {
            SchemaState::Partial { applied, expected } => {
                assert_eq!(applied, 0, "no successful migrations remain");
                assert_eq!(expected, expected_migrations());
            }
            other => panic!("expected Partial, got {other:?}"),
        }
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
