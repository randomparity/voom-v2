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
    assert_eq!(expected_migrations(), 36);
}

#[tokio::test]
async fn scan_session_schema_enforces_lifecycle_and_provenance_backstops() {
    let (pool, _tmp) = fresh_pool().await;

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' \
         AND name IN ('scan_sessions', 'scan_observation_batches', 'scan_observations')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(tables, 3, "migration 0036 must create all scan ledgers");

    let session_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'scan_sessions'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(session_sql.ends_with("STRICT"));
    assert!(session_sql.contains(
        "status IN ('requested', 'running', 'succeeded', 'failed', 'cancelled', 'stale')"
    ));
    assert!(session_sql.contains("length(CAST(terminal_reason AS BLOB)) BETWEEN 1 AND 1024"));
    assert!(session_sql.contains("status = 'succeeded' OR retired_location_count = 0"));
    assert!(
        session_sql.contains("char(9) || char(10) || char(11) || char(12) || char(13) || char(32)")
    );

    let active_index: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'index' \
         AND name = 'scan_sessions_one_active_per_root'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(active_index.contains("WHERE status IN ('requested', 'running')"));

    let observation_index: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'index' \
         AND name = 'scan_observations_one_locator_per_session'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(observation_index.contains("scan_session_id, provider_relative_locator"));

    let predecessor_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_observation_batches') \
         WHERE name = 'previous_sequence'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(predecessor_column, 1);

    let immutable_triggers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' AND name IN (\
         'scan_observation_batches_validate_insert', \
         'scan_observation_batches_no_update', \
         'scan_observation_batches_no_delete')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(immutable_triggers, 3);

    let batch_self_foreign_key: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('scan_observation_batches') \
         WHERE \"table\" = 'scan_observation_batches' \
         AND \"from\" IN ('scan_session_id', 'previous_sequence')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(batch_self_foreign_key, 2);

    let owner_incarnation_foreign_key: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('scan_sessions') \
         WHERE \"table\" = 'node_incarnations' \
         AND \"from\" IN ('owner_incarnation_id', 'owner_node_id')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owner_incarnation_foreign_key, 2);

    let reconciliation_index: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'index' \
         AND name = 'file_locations_by_retired_scan_session'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let reconciliation_index = reconciliation_index
        .expect("file_locations reconciliation index")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        reconciliation_index,
        "CREATE INDEX file_locations_by_retired_scan_session \
         ON file_locations(retired_by_scan_session_id, id)"
    );

    let root_pointer: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('library_roots') WHERE name = 'last_scan_session_id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(root_pointer, 1);

    let location_pointer: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('file_locations') \
         WHERE name = 'retired_by_scan_session_id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(location_pointer, 1);
}

#[tokio::test]
async fn scan_batch_schema_enforces_append_only_predecessor_arithmetic() {
    let (pool, _tmp) = fresh_pool().await;
    insert_active_incarnation(&pool, "batch-chain-incarnation").await;
    let session = insert_running_scan_session(&pool, 9_000_001, "batch-chain-incarnation").await;

    sqlx::query(
        "INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
         request_hash, observation_count, accepted_at, cumulative_observation_count) \
         VALUES (?, 0, NULL, ?, 2, '1970-01-01T00:00:01Z', 2)",
    )
    .bind(session)
    .bind("a".repeat(64))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
         request_hash, observation_count, accepted_at, cumulative_observation_count) \
         VALUES (?, 1, 0, ?, 3, '1970-01-01T00:00:02Z', 5)",
    )
    .bind(session)
    .bind("b".repeat(64))
    .execute(&pool)
    .await
    .unwrap();

    for sql in [
        "INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
         request_hash, observation_count, accepted_at, cumulative_observation_count) \
         VALUES (?, 2, 1, printf('%064x', 2), 1, '1970-01-01T00:00:03Z', 99)",
        "UPDATE scan_observation_batches SET cumulative_observation_count = 4 \
         WHERE scan_session_id = ? AND sequence = 1",
        "DELETE FROM scan_observation_batches WHERE scan_session_id = ? AND sequence = 1",
    ] {
        let error = sqlx::query(sql)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("scan observation batch"),
            "expected append-only ledger trigger rejection, got {error:?}"
        );
    }
}

#[tokio::test]
async fn scan_session_schema_binds_owner_incarnation_to_owner_node() {
    let (pool, _tmp) = fresh_pool().await;
    sqlx::query(
        "INSERT INTO nodes (id, name, kind, status, registered_at, last_seen_at, retired_at, \
         heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata, epoch) \
         VALUES (9000002, 'other-node', 'local', 'active', '1970-01-01T00:00:00Z', \
         '1970-01-01T00:00:00Z', NULL, 60, 'other-hash', 'other-hint', '{}', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES ('other-incarnation', 9000002, 'active', '1970-01-01T00:00:00Z', \
         '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let error = sqlx::query(
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, \
         owner_incarnation_id, status, idle_timeout_seconds, progress_deadline_at, requested_at, \
         started_at) VALUES (9000001, 1, 9000001, 'other-incarnation', 'running', 300, \
         '1970-01-01T00:05:00Z', '1970-01-01T00:00:00Z', '1970-01-01T00:00:01Z')",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("FOREIGN KEY constraint failed"));
}

#[tokio::test]
async fn scan_session_schema_rejects_invalid_rows_and_preserves_byte_boundaries() {
    let (pool, _tmp) = fresh_pool().await;

    assert_scan_session_rejected(
        &pool,
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         idle_timeout_seconds, progress_deadline_at, requested_at) VALUES \
         (9000001, 1, 9000001, 'unknown', 300, '1970-01-01T00:05:00Z', \
          '1970-01-01T00:00:00Z')",
    )
    .await;
    assert_scan_session_rejected(
        &pool,
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         idle_timeout_seconds, progress_deadline_at, requested_at, retired_location_count) VALUES \
         (9000001, 1, 9000001, 'requested', 300, '1970-01-01T00:05:00Z', \
          '1970-01-01T00:00:00Z', 1)",
    )
    .await;
    for reason in [
        "''".to_owned(),
        "char(9) || char(10) || char(13) || ' '".to_owned(),
        format!("'{}'", "é".repeat(513)),
    ] {
        assert_scan_session_rejected(
            &pool,
            &format!(
                "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
                 idle_timeout_seconds, progress_deadline_at, requested_at, terminal_at, \
                 terminal_reason) VALUES (9000001, 1, 9000001, 'failed', 300, \
                 '1970-01-01T00:05:00Z', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:01:00Z', {reason})"
            ),
        )
        .await;
    }
    assert_scan_session_rejected(
        &pool,
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         next_sequence, idle_timeout_seconds, progress_deadline_at, requested_at) VALUES \
         (9000001, 1, 9000001, 'requested', -1, 300, '1970-01-01T00:05:00Z', \
          '1970-01-01T00:00:00Z')",
    )
    .await;
    assert_scan_session_rejected(
        &pool,
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         next_sequence, batch_count, observation_count, idle_timeout_seconds, \
         progress_deadline_at, requested_at) VALUES \
         (9000001, 1, 9000001, 'requested', 1, 0, 0, 300, \
          '1970-01-01T00:05:00Z', '1970-01-01T00:00:00Z')",
    )
    .await;
    assert_scan_session_rejected(
        &pool,
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         next_sequence, batch_count, observation_count, idle_timeout_seconds, \
         progress_deadline_at, requested_at) VALUES \
         (9000001, 1, 9000001, 'requested', 1, 1, 1001, 300, \
          '1970-01-01T00:05:00Z', '1970-01-01T00:00:00Z')",
    )
    .await;

    let exact_multibyte = "é".repeat(512);
    sqlx::query(&format!(
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         idle_timeout_seconds, progress_deadline_at, requested_at, terminal_at, terminal_reason) \
         VALUES (9000001, 1, 9000001, 'failed', 300, '1970-01-01T00:05:00Z', \
         '1970-01-01T00:00:00Z', '1970-01-01T00:01:00Z', '{exact_multibyte}')"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let stored_reason: String = sqlx::query_scalar(
        "SELECT terminal_reason FROM scan_sessions WHERE terminal_reason IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_reason, exact_multibyte);

    insert_active_incarnation(&pool, "scan-schema-active-incarnation").await;
    let active_session_id =
        insert_running_scan_session(&pool, 9_000_001, "scan-schema-active-incarnation").await;
    assert_scan_session_unique_rejected(
        &pool,
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         idle_timeout_seconds, progress_deadline_at, requested_at) VALUES \
         (9000001, 1, 9000001, 'requested', 300, '1970-01-01T00:05:00Z', \
          '1970-01-01T00:00:00Z')",
    )
    .await;
    sqlx::query(
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         idle_timeout_seconds, progress_deadline_at, requested_at, terminal_at, terminal_reason) \
         VALUES (9000001, 1, 9000001, 'cancelled', 300, '1970-01-01T00:05:00Z', \
         '1970-01-01T00:00:00Z', '1970-01-01T00:01:00Z', 'operator cancelled')",
    )
    .execute(&pool)
    .await
    .unwrap();

    seed_second_storage_root(&pool).await;
    let other_root_location =
        seed_live_rooted_location_for_root(&pool, 9_000_002, "scan-schema-other-root").await;
    assert_scan_session_foreign_key_rejected(
        &pool,
        &format!(
            "UPDATE scan_sessions SET location_high_watermark_id = {other_root_location} \
             WHERE id = {active_session_id}"
        ),
    )
    .await;
    sqlx::query(
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         idle_timeout_seconds, progress_deadline_at, requested_at) VALUES \
         (9000002, 1, 9000001, 'requested', 300, '1970-01-01T00:05:00Z', \
          '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO scan_observation_batches \
         (scan_session_id, sequence, previous_sequence, request_hash, observation_count, \
          accepted_at, cumulative_observation_count) \
         VALUES (?, 0, NULL, ?, 1, '1970-01-01T00:00:01Z', 1)",
    )
    .bind(active_session_id)
    .bind("a".repeat(64))
    .execute(&pool)
    .await
    .unwrap();
    assert_scan_observation_rejected(
        &pool,
        active_session_id,
        "single\\backslash",
        "single backslash locator must be rejected by the migration",
    )
    .await;
    sqlx::query(
        "INSERT INTO scan_observations (scan_session_id, batch_sequence, ordinal, \
         provider_relative_locator, provider_object_identity, size_bytes, modified_at, \
         stability_started_at, stability_confirmed_at) VALUES (?, 0, 1, ?, 'object', 1, \
         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(active_session_id)
    .bind("normal/locator")
    .execute(&pool)
    .await
    .unwrap();

    let location_id = seed_live_rooted_location(&pool).await;
    let session_id: i64 =
        sqlx::query_scalar("SELECT id FROM scan_sessions WHERE status = 'requested' LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_file_location_rejected(
        &pool,
        &format!(
            "UPDATE file_locations SET retired_by_scan_session_id = {session_id} WHERE id = {location_id}"
        ),
    )
    .await;
}

#[tokio::test]
async fn scan_session_capacity_schema_rejects_session_and_batch_totals_over_the_limit() {
    let (pool, _tmp) = fresh_pool().await;
    insert_active_incarnation(&pool, "capacity-incarnation").await;
    let active = insert_running_scan_session(&pool, 9_000_001, "capacity-incarnation").await;
    assert_scan_session_rejected(
        &pool,
        &format!(
            "UPDATE scan_sessions SET next_sequence = 101, batch_count = 101, \
             observation_count = 100001 WHERE id = {active}"
        ),
    )
    .await;
    sqlx::query("DROP TRIGGER scan_observation_batches_validate_insert")
        .execute(&pool)
        .await
        .unwrap();
    let error = sqlx::query(
        "INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
         request_hash, observation_count, accepted_at, cumulative_observation_count) \
         VALUES (?, 0, NULL, ?, 1, '1970-01-01T00:00:00Z', 100001)",
    )
    .bind(active)
    .bind("a".repeat(64))
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("CHECK constraint failed"));
}

async fn assert_scan_session_rejected(pool: &sqlx::SqlitePool, sql: &str) {
    let error = sqlx::query(sql).execute(pool).await.unwrap_err();
    assert!(
        error.to_string().contains("constraint failed"),
        "expected SQLite constraint to reject scan session row, got {error:?}"
    );
}

async fn assert_scan_session_unique_rejected(pool: &sqlx::SqlitePool, sql: &str) {
    let error = sqlx::query(sql).execute(pool).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("UNIQUE constraint failed: scan_sessions.storage_root_id"),
        "expected active-root unique index to reject a lifecycle-valid row, got {error:?}"
    );
}

async fn assert_scan_session_foreign_key_rejected(pool: &sqlx::SqlitePool, sql: &str) {
    let error = sqlx::query(sql).execute(pool).await.unwrap_err();
    assert!(
        error.to_string().contains("FOREIGN KEY constraint failed"),
        "expected scan-session root binding foreign key to reject row, got {error:?}"
    );
}

async fn assert_scan_observation_rejected(
    pool: &sqlx::SqlitePool,
    scan_session_id: i64,
    locator: &str,
    message: &str,
) {
    let error = sqlx::query(
        "INSERT INTO scan_observations (scan_session_id, batch_sequence, ordinal, \
         provider_relative_locator, provider_object_identity, size_bytes, modified_at, \
         stability_started_at, stability_confirmed_at) VALUES (?, 0, 0, ?, 'object', 1, \
         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(scan_session_id)
    .bind(locator)
    .execute(pool)
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{message}, got {error:?}"
    );
}

async fn insert_active_incarnation(pool: &sqlx::SqlitePool, incarnation_id: &str) {
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
         VALUES (?, 9000001, 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', NULL, NULL)",
    )
    .bind(incarnation_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_running_scan_session(
    pool: &sqlx::SqlitePool,
    storage_root_id: i64,
    incarnation_id: &str,
) -> i64 {
    sqlx::query(
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, \
         owner_incarnation_id, status, idle_timeout_seconds, progress_deadline_at, requested_at, \
         started_at) VALUES (?, 1, 9000001, ?, 'running', 300, \
         '1970-01-01T00:05:00Z', '1970-01-01T00:00:00Z', '1970-01-01T00:00:01Z')",
    )
    .bind(storage_root_id)
    .bind(incarnation_id)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
}

async fn seed_second_storage_root(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO library_roots (id, library_id, owner_node_id, provider_kind, \
         provider_locator, display_locator, state, root_epoch, activation_identity, \
         include_globs, exclude_globs, extension_allowlist, scan_mode, symlink_policy, \
         hidden_file_policy, max_depth, stability_seconds, debounce_seconds, \
         default_output_root_id, default_staging_root_id, default_backup_root_id, enabled, \
         created_at, updated_at) VALUES (9000002, 9000001, 9000001, 'local_filesystem', \
         '/second-root', '/second-root', 'active', 1, 'second-root', '[]', '[]', '[]', \
         'manual_recursive', 'reject', 'ignore', NULL, 0, 0, NULL, NULL, NULL, 1, \
         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn assert_file_location_rejected(pool: &sqlx::SqlitePool, sql: &str) {
    let error = sqlx::query(sql).execute(pool).await.unwrap_err();
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "expected file-location provenance CHECK to reject row, got {error:?}"
    );
}

async fn seed_live_rooted_location(pool: &sqlx::SqlitePool) -> i64 {
    seed_live_rooted_location_for_root(pool, 9_000_001, "scan-schema-fixture").await
}

async fn seed_live_rooted_location_for_root(
    pool: &sqlx::SqlitePool,
    storage_root_id: i64,
    fixture_name: &str,
) -> i64 {
    let asset_id = sqlx::query("INSERT INTO file_assets (created_at, epoch) VALUES (?, 0)")
        .bind("1970-01-01T00:00:00Z")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let version_id = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at, retired_at, epoch) \
         VALUES (?, ?, 1, 'ingest', NULL, '1970-01-01T00:00:00Z', NULL, 0)",
    )
    .bind(asset_id)
    .bind(fixture_name)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO file_locations (file_version_id, address_state, storage_root_id, \
         provider_relative_locator, legacy_kind, legacy_locator, proof_kind, proof_value, \
         observed_at, retired_at, epoch) VALUES (?, 'rooted', ?, ?, \
         NULL, NULL, NULL, NULL, '1970-01-01T00:00:00Z', NULL, 0)",
    )
    .bind(version_id)
    .bind(storage_root_id)
    .bind(fixture_name)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
}

/// Column list every video-profile fixture below shares, so each test spells
/// only the columns it is actually exercising.
fn video_profile_insert(name: &str, columns: &str, values: &str) -> String {
    format!(
        "INSERT INTO video_profiles (id, name, target_codec, {columns}) \
         VALUES ('vp-{name}', '{name}', 'hevc', {values})"
    )
}

async fn assert_profile_accepted(pool: &sqlx::SqlitePool, sql: &str) {
    sqlx::query(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("row must be accepted: {e}\n{sql}"));
}

/// Asserts the row is refused **by a CHECK constraint** specifically. The
/// mechanism is load-bearing: a `preset` still declared `NOT NULL` would also
/// refuse a null preset, but for the wrong reason and for every encoder — so
/// matching on the message is what proves the column became nullable and the
/// per-encoder CHECK is what protects it.
async fn assert_profile_check_rejected(pool: &sqlx::SqlitePool, sql: &str) {
    let err = sqlx::query(sql).execute(pool).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("CHECK constraint failed"),
        "expected a CHECK constraint to refuse the row, got {msg}\n{sql}"
    );
}

/// A VAAPI profile is a legal durable row only in the exact shape spec §8
/// describes: null `preset` (`hevc_vaapi` has no speed knob), `qp` as the sole
/// quality field, and `vaapi` decode. `STRICT` must survive the rebuild —
/// without it `SQLite` would silently coerce a text `qp` into the column and the
/// range CHECK would be comparing the wrong type.
#[tokio::test]
async fn video_profiles_admit_a_vaapi_profile_and_stay_strict() {
    let (pool, _tmp) = fresh_pool().await;

    let table_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'video_profiles'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        table_sql.ends_with("STRICT"),
        "rebuilt video_profiles must stay STRICT: {table_sql}"
    );

    assert_profile_accepted(
        &pool,
        &video_profile_insert(
            "vaapi-main10",
            "encoder, preset, qp, codec_profile, pixel_format, decode_backend",
            "'hevc_vaapi', NULL, 23, 'main10', 'p010', 'vaapi'",
        ),
    )
    .await;
}

/// `preset` presence is per-encoder, not global: `hevc_vaapi` exposes no
/// `-preset`, so carrying one would mean the durable row disagrees with the
/// argv the worker can actually build. Every other encoder still requires one,
/// which is why the column can be nullable without weakening them.
#[tokio::test]
async fn video_profiles_bind_preset_presence_to_the_encoder() {
    let (pool, _tmp) = fresh_pool().await;

    assert_profile_check_rejected(
        &pool,
        &video_profile_insert(
            "vaapi-with-preset",
            "encoder, preset, qp, decode_backend",
            "'hevc_vaapi', 'p4', 23, 'vaapi'",
        ),
    )
    .await;
    assert_profile_check_rejected(
        &pool,
        &video_profile_insert(
            "x265-without-preset",
            "encoder, preset, crf",
            "'libx265', NULL, 23",
        ),
    )
    .await;
    assert_profile_check_rejected(
        &pool,
        &video_profile_insert(
            "nvenc-without-preset",
            "encoder, preset, cq, decode_backend",
            "'hevc_nvenc', NULL, 23, 'nvidia'",
        ),
    )
    .await;
    assert_profile_accepted(
        &pool,
        &video_profile_insert(
            "x265-with-preset",
            "encoder, preset, crf",
            "'libx265', 'medium', 23",
        ),
    )
    .await;
}

/// The SQL range must agree with `QualityDomain::Qp { min: 1, max: 52 }` in
/// `voom-core`, inclusive at both ends. `qp = 0` is `hevc_vaapi`'s "auto", not
/// an operator quality target (spec §2.2), and 53 is rejected by ffmpeg itself.
/// Pinning both boundaries here is what keeps the two layers from drifting into
/// a range only one of them enforces.
#[tokio::test]
async fn video_profiles_pin_the_vaapi_qp_range_to_the_rust_descriptor() {
    let (pool, _tmp) = fresh_pool().await;

    // Read both ends off the descriptor rather than restating them: hardcoding
    // them here would let the Rust range move while this test — the thing named
    // for catching that drift — kept passing against the old SQL.
    let voom_core::QualityDomain::Qp { min, max } = voom_core::encoder_descriptor("hevc_vaapi")
        .expect("hevc_vaapi has an encoder descriptor")
        .quality_domain
    else {
        panic!("hevc_vaapi must carry a Qp quality domain");
    };
    let below = min
        .checked_sub(1)
        .expect("qp min must leave room for a below-range probe");
    let above = max
        .checked_add(1)
        .expect("qp max must leave room for an above-range probe");

    for (name, qp) in [("vaapi-qp-min", min), ("vaapi-qp-max", max)] {
        assert_profile_accepted(
            &pool,
            &video_profile_insert(
                name,
                "encoder, preset, qp, decode_backend",
                &format!("'hevc_vaapi', NULL, {qp}, 'vaapi'"),
            ),
        )
        .await;
    }
    for (name, qp) in [("vaapi-qp-auto", below), ("vaapi-qp-over", above)] {
        assert_profile_check_rejected(
            &pool,
            &video_profile_insert(
                name,
                "encoder, preset, qp, decode_backend",
                &format!("'hevc_vaapi', NULL, {qp}, 'vaapi'"),
            ),
        )
        .await;
    }
}

/// Exactly one quality field is legal per encoder — the one its `QualityDomain`
/// names. A VAAPI row carrying `crf` or `cq` would make the durable row
/// ambiguous about which knob the worker should emit, and a row carrying none
/// would leave the encode's quality unspecified.
#[tokio::test]
async fn video_profiles_allow_exactly_one_quality_field_per_encoder() {
    let (pool, _tmp) = fresh_pool().await;

    for (name, columns, values) in [
        (
            "vaapi-and-crf",
            "encoder, preset, qp, crf, decode_backend",
            "'hevc_vaapi', NULL, 23, 23, 'vaapi'",
        ),
        (
            "vaapi-and-cq",
            "encoder, preset, qp, cq, decode_backend",
            "'hevc_vaapi', NULL, 23, 23, 'vaapi'",
        ),
        (
            "vaapi-no-quality",
            "encoder, preset, decode_backend",
            "'hevc_vaapi', NULL, 'vaapi'",
        ),
        (
            "x265-and-qp",
            "encoder, preset, crf, qp",
            "'libx265', 'medium', 23, 23",
        ),
        (
            "nvenc-and-qp",
            "encoder, preset, cq, qp, decode_backend",
            "'hevc_nvenc', 'p4', 23, 23, 'nvidia'",
        ),
    ] {
        assert_profile_check_rejected(&pool, &video_profile_insert(name, columns, values)).await;
    }
}

/// Hardware decode is only reachable through the matching hardware encoder: a
/// `vaapi`-decoded frame lands in a VAAPI surface that only `hevc_vaapi` can
/// consume, and the NVIDIA pairing is the same rule from migration 0029. A row
/// that paired them wrongly would dispatch to a worker that cannot run it.
#[tokio::test]
async fn video_profiles_pair_each_decode_backend_with_its_encoder() {
    let (pool, _tmp) = fresh_pool().await;

    for (name, encoder, backend) in [
        ("vaapi-decode-x265", "libx265", "vaapi"),
        ("vaapi-decode-nvenc", "hevc_nvenc", "vaapi"),
        ("nvidia-decode-vaapi", "hevc_vaapi", "nvidia"),
        ("qsv-decode", "hevc_vaapi", "qsv"),
    ] {
        let quality = if encoder == "libx265" {
            "crf"
        } else if encoder == "hevc_nvenc" {
            "cq"
        } else {
            "qp"
        };
        let preset = if encoder == "hevc_vaapi" {
            "NULL"
        } else {
            "'medium'"
        };
        assert_profile_check_rejected(
            &pool,
            &video_profile_insert(
                name,
                &format!("encoder, preset, {quality}, decode_backend"),
                &format!("'{encoder}', {preset}, 23, '{backend}'"),
            ),
        )
        .await;
    }

    assert_profile_accepted(
        &pool,
        &video_profile_insert(
            "vaapi-decode-vaapi",
            "encoder, preset, qp, decode_backend",
            "'hevc_vaapi', NULL, 23, 'vaapi'",
        ),
    )
    .await;
    assert_profile_accepted(
        &pool,
        &video_profile_insert(
            "nvidia-decode-nvenc",
            "encoder, preset, cq, decode_backend",
            "'hevc_nvenc', 'p4', 23, 'nvidia'",
        ),
    )
    .await;
}

#[tokio::test]
async fn workflow_file_window_schema_is_strict_and_job_owned() {
    let (pool, _tmp) = fresh_pool().await;
    let window_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema \
         WHERE type = 'table' AND name = 'workflow_file_windows'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(window_sql.contains("CHECK (max_in_flight_files > 0)"));
    assert!(window_sql.ends_with("STRICT"));

    let progress_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema \
         WHERE type = 'table' AND name = 'workflow_file_progress'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(progress_sql.contains("UNIQUE (job_id, input_ordinal)"));
    assert!(progress_sql.contains("state IN ('pending', 'active', 'terminalizing', 'terminal')"));
    assert!(progress_sql.ends_with("STRICT"));

    let entry_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema \
         WHERE type = 'table' AND name = 'workflow_file_phase_entries'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(entry_sql.contains("PRIMARY KEY (job_id, phase_ordinal, branch_id)"));
    assert!(entry_sql.contains("gate_admitted IN (0, 1)"));
    assert!(entry_sql.ends_with("STRICT"));
}

#[tokio::test]
async fn workflow_file_run_start_schema_is_strict_and_job_owned() {
    let (pool, _tmp) = fresh_pool().await;

    let table_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema \
         WHERE type = 'table' AND name = 'workflow_file_run_starts'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(table_sql.contains("PRIMARY KEY (job_id, branch_id)"));
    assert!(table_sql.contains("CHECK (starting_phase_ordinal >= 0)"));
    assert!(table_sql.ends_with("STRICT"));

    let job_fk: (String, String) = sqlx::query_as(
        "SELECT \"table\", on_delete \
         FROM pragma_foreign_key_list('workflow_file_run_starts') \
         WHERE \"from\" = 'job_id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(job_fk, ("jobs".to_owned(), "CASCADE".to_owned()));

    let version_fk: (String, String) = sqlx::query_as(
        "SELECT \"table\", on_delete \
         FROM pragma_foreign_key_list('workflow_file_run_starts') \
         WHERE \"from\" = 'starting_file_version_id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        version_fk,
        ("file_versions".to_owned(), "RESTRICT".to_owned())
    );
}

#[tokio::test]
async fn workflow_file_run_history_schema_is_strict_and_run_owned() {
    let (pool, _tmp) = fresh_pool().await;

    let table_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema \
         WHERE type = 'table' AND name = 'workflow_file_run_history'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(table_sql.contains("PRIMARY KEY (job_id, branch_id, phase_ordinal)"));
    assert!(table_sql.contains("CHECK (phase_ordinal >= 0)"));
    assert!(
        table_sql.contains("CHECK (outcome IN ('committed', 'verified', 'skipped', 'blocked'))")
    );
    assert!(table_sql.ends_with("STRICT"));

    let run_fk_columns: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT \"from\", \"to\", on_delete \
         FROM pragma_foreign_key_list('workflow_file_run_history') \
         WHERE \"table\" = 'workflow_file_run_starts' \
         ORDER BY seq ASC",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        run_fk_columns,
        vec![
            (
                "job_id".to_owned(),
                "job_id".to_owned(),
                "CASCADE".to_owned()
            ),
            (
                "branch_id".to_owned(),
                "branch_id".to_owned(),
                "CASCADE".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn library_schema_enforces_node_owned_root_contract() {
    let (pool, _tmp) = fresh_pool().await;

    let libraries_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'libraries'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        libraries_sql.contains("CHECK (media_kind IN ('movie', 'episode', 'personal', 'unknown'))")
    );

    let roots_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'library_roots'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(roots_sql.contains("CHECK (json_valid(include_globs))"));
    assert!(roots_sql.contains("provider_kind IN ('local_filesystem')"));
    assert!(
        roots_sql
            .contains("state IN ('unassigned', 'configured', 'active', 'unavailable', 'retired')")
    );
    assert!(
        roots_sql.contains(
            "CHECK (scan_mode IN ('explicit_only', 'manual_recursive', 'watch_enabled'))"
        )
    );

    let slug_index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = 'libraries_slug'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(slug_index_count, 1);

    let fk_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('library_roots') \
         WHERE \"table\" = 'libraries' AND on_delete = 'RESTRICT'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fk_count, 1);
}

async fn fresh_pool() -> (sqlx::SqlitePool, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

#[tokio::test]
async fn profile_management_columns_extend_existing_registries() {
    let (pool, _tmp) = fresh_pool().await;

    // Soft-retire marker added to the seeded video-profile registry (0021),
    // mirroring the retire column the 0004 scoring registry already carries.
    let video_retired: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('video_profiles') WHERE name = 'retired_at'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(video_retired, 1);

    // The scoring registry from migration 0004 is the one this issue manages —
    // keyed by `name`, carrying a JSON `definition` and its own `retired_at`.
    let scoring_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' \
         AND name = 'quality_scoring_profiles'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(scoring_sql.contains("name        TEXT NOT NULL UNIQUE"));
    assert!(scoring_sql.contains("CHECK (json_valid(definition))"));
    assert!(scoring_sql.contains("retired_at"));

    // Per-library default scoring-profile linkage column (repo-enforced by
    // profile name, not a declared FK — see migration 0021).
    let default_col: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('libraries') \
         WHERE name = 'default_scoring_profile_name'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(default_col, 1);
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
    let tmp = voom_test_support::TempDatabase::new().unwrap();
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
    assert!(
        file_phase_sql
            .contains("CHECK (outcome IN ('committed', 'verified', 'skipped', 'blocked'))")
    );
    assert!(file_phase_sql.contains("CHECK (json_valid(ticket_ids))"));

    // The grandchild references jobs plus produced-artifact and verification tables.
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
            "artifact_verifications".to_owned(),
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
