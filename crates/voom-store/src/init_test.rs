use super::*;
use crate::pool::connect;
use crate::schema::{expected_migrations, probe_schema};

#[tokio::test]
async fn init_in_memory_applies_every_embedded_migration() {
    let pool = connect("sqlite::memory:").await.unwrap();
    let report = init_on(&pool).await.unwrap();
    assert!(!report.already_initialized);
    assert_eq!(report.migrations_applied, expected_migrations());
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
            assert_eq!(
                applied,
                expected_migrations() - 1,
                "only the marked-failed row should be missing from the success count"
            );
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

    sqlx::query("UPDATE schema_meta SET value = 'not-a-timestamp' WHERE key = 'schema_init_at'")
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
    assert_eq!(report.migrations_applied, expected_migrations());
}
