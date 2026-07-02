use super::*;
use crate::pool::connect;
use crate::test_support::fresh_initialized_pool_at;

/// SQL that creates an empty `_sqlx_migrations` table matching sqlx's
/// schema. Tests use this to simulate post-init states without depending
/// on Task 11's `init_on` (which doesn't exist yet at this checkpoint).
const CREATE_MIGRATIONS_TABLE: &str = "\
    CREATE TABLE _sqlx_migrations ( \
        version BIGINT PRIMARY KEY, \
        description TEXT NOT NULL, \
        installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
        success BOOLEAN NOT NULL, \
        checksum BLOB NOT NULL, \
        execution_time BIGINT NOT NULL \
    )";

#[tokio::test]
async fn probe_returns_uninitialized_on_fresh_db() {
    let pool = connect("sqlite::memory:").await.unwrap();
    assert_eq!(
        probe_schema(&pool).await.unwrap(),
        SchemaState::Uninitialized
    );
}

#[tokio::test]
async fn expected_migrations_matches_embedded_count() {
    // review whenever a migration is added/removed.
    assert_eq!(expected_migrations(), 19);
}

async fn fresh_pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

#[tokio::test]
async fn nodes_schema_preserves_registry_constraints_and_worker_link() {
    let (pool, _tmp) = fresh_pool().await;

    let nodes_sql: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'nodes'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(nodes_sql.contains("CHECK (kind IN ('local','remote','synthetic'))"));
    assert!(nodes_sql.contains("CHECK (status IN ('registered','active','stale','retired'))"));
    assert!(nodes_sql.contains("CHECK (json_valid(metadata))"));
    assert!(nodes_sql.contains("CHECK (heartbeat_ttl_seconds > 0)"));

    let worker_node_col: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('workers') WHERE name = 'node_id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(worker_node_col, 1);

    let fk_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('workers') WHERE \"table\" = 'nodes'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fk_count, 1);
}

#[tokio::test]
async fn backups_schema_enforces_status_vocab_and_verified_key() {
    let (pool, _tmp) = fresh_pool().await;

    let backups_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'backups'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(backups_sql.contains("CHECK (status IN ('pending', 'verified', 'failed'))"));

    let verified_key_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'index' AND name = 'backups_verified_key'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(verified_key_sql.contains("WHERE status = 'verified'"));

    let fk_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('backups') \
         WHERE \"table\" IN ('file_versions', 'jobs', 'tickets')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fk_count, 3);
}

#[tokio::test]
async fn remote_execution_schema_contains_idempotency_and_artifact_access_tables() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = crate::test_support::sqlite_url_for(tmp.path());
    crate::init(&url).await.unwrap();
    let pool = crate::connect(&url).await.unwrap();

    let idem_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'remote_idempotency_keys'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(idem_sql.contains("node_id"));
    assert!(idem_sql.contains("route_key"));
    assert!(idem_sql.contains("request_hash"));
    assert!(idem_sql.contains("response_json"));
    assert!(idem_sql.contains("worker_scope_id"));
    assert!(idem_sql.contains("UNIQUE (node_id, route_key, worker_scope_id, idempotency_key)"));
    assert!(idem_sql.contains("worker_id IS NOT NULL"));

    let plan_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'artifact_access_plans'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(plan_sql.contains("lease_id"));
    assert!(plan_sql.contains("ticket_id"));
    assert!(plan_sql.contains("worker_id"));
    assert!(plan_sql.contains("node_id"));
    assert!(plan_sql.contains("selected_access_mode"));
    assert!(plan_sql.contains("CHECK (status IN ('selected','consumed','rejected','failed'))"));
    assert!(plan_sql.contains("CHECK (json_valid(input_handles))"));
    assert!(plan_sql.contains("CHECK (json_valid(output_handles))"));
    assert!(plan_sql.contains("CHECK (json_valid(evidence))"));

    for index_name in [
        "remote_idempotency_by_node_created",
        "artifact_access_plans_by_ticket",
        "artifact_access_plans_by_worker",
        "artifact_access_plans_by_node",
        "artifact_access_plans_by_mode_status",
    ] {
        let index_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = ?",
        )
        .bind(index_name)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(index_count, 1, "missing index {index_name}");
    }
}

#[tokio::test]
async fn workflow_summary_schema_links_grains_to_jobs_and_artifacts() {
    let (pool, _tmp) = fresh_pool().await;

    for table in [
        "workflow_summaries",
        "workflow_phase_summaries",
        "workflow_file_phase_summaries",
    ] {
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(table_count, 1, "missing table {table}");
    }

    let phase_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'workflow_phase_summaries'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        phase_sql.contains(
            "CHECK (outcome IN ('completed', 'partially-committed', 'skipped', 'blocked'))"
        )
    );
    assert!(phase_sql.contains("report_id IS NULL AND report IS NULL"));

    let file_phase_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' \
         AND name = 'workflow_file_phase_summaries'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(file_phase_sql.contains("CHECK (outcome IN ('committed', 'skipped', 'blocked'))"));
    assert!(file_phase_sql.contains("CHECK (json_valid(ticket_ids))"));

    // The grandchild references jobs plus the four produced-artifact tables.
    let mut fk_tables: Vec<String> = sqlx::query_scalar(
        "SELECT \"table\" FROM pragma_foreign_key_list('workflow_file_phase_summaries')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    fk_tables.sort();
    assert_eq!(
        fk_tables,
        vec![
            "artifact_handles".to_owned(),
            "file_locations".to_owned(),
            "file_versions".to_owned(),
            "jobs".to_owned(),
            "media_snapshots".to_owned(),
        ]
    );
}

#[tokio::test]
async fn nodes_reject_invalid_registry_values_at_the_database_boundary() {
    let (pool, _tmp) = fresh_pool().await;

    sqlx::query(
        "INSERT INTO nodes (
             name, kind, status, registered_at, last_seen_at,
             heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata
         ) VALUES (
             'valid-node', 'local', 'registered', '2026-05-23T00:00:00Z',
             '2026-05-23T00:00:00Z', 60, 'hash', 'hint', '{}'
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert_node_insert_rejected(
        &pool,
        "INSERT INTO nodes (
             name, kind, status, registered_at, last_seen_at,
             heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata
         ) VALUES (
             'bad-metadata', 'local', 'registered', '2026-05-23T00:00:00Z',
             '2026-05-23T00:00:00Z', 60, 'hash', 'hint', '{not-json'
         )",
    )
    .await;
    assert_node_insert_rejected(
        &pool,
        "INSERT INTO nodes (
             name, kind, status, registered_at, last_seen_at,
             heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata
         ) VALUES (
             'bad-ttl', 'local', 'registered', '2026-05-23T00:00:00Z',
             '2026-05-23T00:00:00Z', 0, 'hash', 'hint', '{}'
         )",
    )
    .await;
    assert_node_insert_rejected(
        &pool,
        "INSERT INTO nodes (
             name, kind, status, registered_at, last_seen_at,
             heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata
         ) VALUES (
             'bad-kind', 'edge', 'registered', '2026-05-23T00:00:00Z',
             '2026-05-23T00:00:00Z', 60, 'hash', 'hint', '{}'
         )",
    )
    .await;
    assert_node_insert_rejected(
        &pool,
        "INSERT INTO nodes (
             name, kind, status, registered_at, last_seen_at,
             heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata
         ) VALUES (
             'bad-status', 'local', 'unknown', '2026-05-23T00:00:00Z',
             '2026-05-23T00:00:00Z', 60, 'hash', 'hint', '{}'
         )",
    )
    .await;
}

async fn assert_node_insert_rejected(pool: &sqlx::SqlitePool, sql: &str) {
    let err = sqlx::query(sql).execute(pool).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("CHECK constraint failed"),
        "expected SQLite CHECK constraint to reject invalid node row, got {err:?}"
    );
}

#[tokio::test]
async fn probe_refuses_foreign_database_with_no_sqlx_migrations() {
    // An existing SQLite DB that has unrelated user tables but lacks
    // `_sqlx_migrations` belongs to someone else. probe_schema must
    // refuse rather than report Uninitialized — otherwise voom init
    // would happily add VOOM tables to a foreign DB after a typo'd
    // --database-url.
    let pool = connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE someone_elses_data (id INTEGER PRIMARY KEY, payload TEXT)")
        .execute(&pool)
        .await
        .unwrap();

    let err = probe_schema(&pool).await.unwrap_err();
    assert_eq!(err.code(), "CONFIG_INVALID");
    let msg = format!("{err}");
    assert!(
        msg.contains("someone_elses_data") || msg.contains("another application"),
        "error must identify the foreign table or surface the wrong-DB diagnosis: {msg}"
    );

    // And: the DB was NOT mutated — the foreign table is still alone.
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(table_count, 1, "probe must not have created any tables");
}

#[tokio::test]
async fn probe_returns_migration_error_on_malformed_sqlx_migrations_table() {
    // The _sqlx_migrations table exists but its shape doesn't match what
    // sqlx (and probe_schema) expect. This is corrupted/incompatible
    // metadata — not a connection failure — so the error must surface as
    // Migration (DB_PARTIAL_SCHEMA) rather than Database (DB_UNREACHABLE).
    let pool = connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE _sqlx_migrations (wrong_column TEXT)")
        .execute(&pool)
        .await
        .unwrap();

    let err = probe_schema(&pool).await.unwrap_err();
    assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
    let msg = format!("{err}");
    assert!(
        msg.contains("_sqlx_migrations"),
        "error message must reference the offending table: {msg}"
    );
}

#[tokio::test]
async fn probe_returns_too_new_on_renumbered_migration_at_same_count() {
    // Pathological case: count matches expectation but the *versions* are
    // not in the embedded MIGRATOR. Seed migrations table by hand — no
    // dependency on init_on (which lands in Task 11). We insert one
    // renumbered row per embedded migration so `applied == expected` and
    // probe must classify on version mismatch alone, not on count drift.
    let pool = connect("sqlite::memory:").await.unwrap();
    sqlx::query(CREATE_MIGRATIONS_TABLE)
        .execute(&pool)
        .await
        .unwrap();
    for offset in 0..expected_migrations() {
        let synthetic_version = 1_000 + i64::from(offset);
        sqlx::query(&format!(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES ({synthetic_version}, 'renumbered', strftime('%s','now'), 1, X'00', 0)"
        ))
        .execute(&pool)
        .await
        .unwrap();
    }

    let state = probe_schema(&pool).await.unwrap();
    match state {
        SchemaState::TooNew { applied, expected } => {
            assert_eq!(applied, expected, "count matches but version is unknown");
        }
        other => panic!("expected TooNew (version not in MIGRATOR), got {other:?}"),
    }
}
