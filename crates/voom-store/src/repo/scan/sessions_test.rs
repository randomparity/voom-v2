use sqlx::Acquire;
use voom_core::{
    NodeId, ProviderRelativeLocator, ScanSessionId, ScanSessionStatus, ScanTerminalReason,
    StorageRootId, VoomError,
};

use super::{
    CompleteScanSessionInput, MAX_SCAN_SESSION_OBSERVATIONS, NewScanObservationBatch,
    NewScanSession, ScanObservation, ScanReconciliationQuery, ScanSessionListQuery,
    SqliteScanSessionRepo,
};
use crate::test_support::{
    T0, fresh_initialized_pool_at, seed_test_storage_root, with_check_constraints_disabled,
};

async fn fresh_pool() -> (sqlx::SqlitePool, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

#[tokio::test]
async fn scan_session_capacity_accepts_the_limit_replays_and_rejects_crossing_atomically() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "abababababababababababababababab").await;
    remove_batch_parent_frontier_guard(&pool).await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        "abababababababababababababababab".parse().unwrap(),
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    sqlx::query(
        "WITH RECURSIVE numbers(value) AS (\
             SELECT 0 UNION ALL SELECT value + 1 FROM numbers WHERE value < 99\
         )\
         INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
             request_hash, observation_count, accepted_at, cumulative_observation_count)\
         SELECT ?, value, CASE WHEN value = 0 THEN NULL ELSE value - 1 END, \
             printf('%064x', value), CASE WHEN value < 99 THEN 1000 ELSE 999 END, \
             '1970-01-01T00:00:00Z', \
             CASE WHEN value < 99 THEN (value + 1) * 1000 ELSE 99999 END \
         FROM numbers ORDER BY value ASC",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "WITH RECURSIVE numbers(value) AS (\
             SELECT 0 UNION ALL SELECT value + 1 FROM numbers WHERE value < 99998\
         )\
         INSERT INTO scan_observations (scan_session_id, batch_sequence, ordinal, \
             provider_relative_locator, provider_object_identity, size_bytes, modified_at, \
             stability_started_at, stability_confirmed_at)\
         SELECT ?, value / 1000, value % 1000, 'capacity/' || value, 'object-' || value, 1, \
             '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
             '1970-01-01T00:00:00Z' FROM numbers ORDER BY value ASC",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = 100, batch_count = 100, \
         observation_count = ? WHERE id = ?",
    )
    .bind(i64::try_from(MAX_SCAN_SESSION_OBSERVATIONS - 1).unwrap())
    .bind(i64::try_from(session.id.0).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    let at_limit = batch(session.id, 100, 'a', vec![observation("capacity/last.mkv")]);
    let accepted = repo
        .accepted_batch_in_tx(&mut tx, at_limit.clone())
        .await
        .unwrap();
    assert_eq!(
        accepted.cumulative_observation_count,
        MAX_SCAN_SESSION_OBSERVATIONS
    );
    assert_eq!(
        repo.accepted_batch_in_tx(&mut tx, at_limit).await.unwrap(),
        accepted
    );

    let crossing = batch(
        session.id,
        101,
        'b',
        vec![observation("capacity/private.mkv")],
    );
    let before: (i64, i64, i64, String) = sqlx::query_as(
        "SELECT next_sequence, batch_count, observation_count, progress_deadline_at \
         FROM scan_sessions WHERE id = ?",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let error = repo
        .accepted_batch_in_tx(&mut tx, crossing)
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Conflict(_)));
    let message = error.to_string();
    assert!(message.contains("maximum 100000"));
    assert!(message.contains("current 100000"));
    assert!(message.contains("incoming 1"));
    let after: (i64, i64, i64, String) = sqlx::query_as(
        "SELECT next_sequence, batch_count, observation_count, progress_deadline_at \
         FROM scan_sessions WHERE id = ?",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(after, before);
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM scan_observation_batches WHERE scan_session_id = ?), \
                (SELECT COUNT(*) FROM scan_observations WHERE scan_session_id = ?)",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .bind(i64::try_from(session.id.0).unwrap())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(counts, (101, 100_000));
}

#[tokio::test]
async fn scan_session_capacity_typed_read_rejects_coherent_over_cap_counters() {
    let (pool, _tmp) = fresh_pool().await;
    let id = insert_requested_session(&pool).await;
    with_check_constraints_disabled(&pool, move |connection| {
        Box::pin(async move {
            sqlx::query(
                "UPDATE scan_sessions SET next_sequence = 101, batch_count = 101, \
                 observation_count = 100001 WHERE id = ?",
            )
            .bind(id)
            .execute(connection)
            .await
        })
    })
    .await
    .unwrap();
    let error = SqliteScanSessionRepo::new(pool)
        .get(ScanSessionId(u64::try_from(id).unwrap()))
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    assert!(
        error
            .to_string()
            .contains("observation_count 100001 exceeds maximum 100000")
    );
}

async fn insert_requested_session(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query(
        "INSERT INTO scan_sessions (\
             storage_root_id, root_epoch, owner_node_id, status, idle_timeout_seconds, \
             progress_deadline_at, requested_at\
         ) VALUES (9000001, 1, 9000001, 'requested', 300, \
                   '1970-01-01T00:05:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
}

#[tokio::test]
async fn scan_session_row_decoder_preserves_typed_ids_and_rejects_corruption() {
    let (pool, _tmp) = fresh_pool().await;
    let id = insert_requested_session(&pool).await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let decoded = repo
        .get(ScanSessionId(u64::try_from(id).unwrap()))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(decoded.id.0, u64::try_from(id).unwrap());
    assert_eq!(decoded.storage_root_id.0, 9_000_001);
    assert_eq!(decoded.owner_node_id.0, 9_000_001);
    assert_eq!(decoded.next_sequence, 0);

    with_check_constraints_disabled(&pool, |connection| {
        Box::pin(async move {
            sqlx::query("UPDATE scan_sessions SET next_sequence = -1 WHERE id = ?")
                .bind(id)
                .execute(connection)
                .await
        })
    })
    .await
    .unwrap();
    let error = repo
        .get(ScanSessionId(u64::try_from(id).unwrap()))
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
}

#[tokio::test]
async fn scan_session_row_decoder_rejects_isolated_persisted_corruption() {
    let (pool, _tmp) = fresh_pool().await;
    for (name, sql) in [
        (
            "negative sequence",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'requested' AS status, -1 AS next_sequence, 0 AS batch_count, \
             0 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, '1970-01-01T00:00:00Z' AS requested_at, NULL AS started_at, \
             NULL AS terminal_at, NULL AS terminal_reason, 0 AS retired_location_count",
        ),
        (
            "unknown status",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'unknown' AS status, 0 AS next_sequence, 0 AS batch_count, \
             0 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, '1970-01-01T00:00:00Z' AS requested_at, NULL AS started_at, \
             NULL AS terminal_at, NULL AS terminal_reason, 0 AS retired_location_count",
        ),
        (
            "invalid requested timestamp",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'requested' AS status, 0 AS next_sequence, 0 AS batch_count, \
             0 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, 'not-a-time' AS requested_at, NULL AS started_at, \
             NULL AS terminal_at, NULL AS terminal_reason, 0 AS retired_location_count",
        ),
        (
            "blank terminal reason",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'failed' AS status, 0 AS next_sequence, 0 AS batch_count, \
             0 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, '1970-01-01T00:00:00Z' AS requested_at, NULL AS started_at, \
             '1970-01-01T00:01:00Z' AS terminal_at, '   ' AS terminal_reason, 0 AS retired_location_count",
        ),
        (
            "invalid terminal timestamp",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'failed' AS status, 0 AS next_sequence, 0 AS batch_count, \
             0 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, '1970-01-01T00:00:00Z' AS requested_at, NULL AS started_at, \
             'not-a-time' AS terminal_at, 'operator failed' AS terminal_reason, 0 AS retired_location_count",
        ),
        (
            "invalid terminal lifecycle shape",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'failed' AS status, 0 AS next_sequence, 0 AS batch_count, \
             0 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, '1970-01-01T00:00:00Z' AS requested_at, NULL AS started_at, \
             NULL AS terminal_at, 'operator failed' AS terminal_reason, 0 AS retired_location_count",
        ),
        (
            "retired locations on non-succeeded session",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'requested' AS status, 0 AS next_sequence, 0 AS batch_count, \
             0 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, '1970-01-01T00:00:00Z' AS requested_at, NULL AS started_at, \
             NULL AS terminal_at, NULL AS terminal_reason, 1 AS retired_location_count",
        ),
        (
            "batch count differs from next sequence",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'requested' AS status, 1 AS next_sequence, 0 AS batch_count, \
             0 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, '1970-01-01T00:00:00Z' AS requested_at, NULL AS started_at, \
             NULL AS terminal_at, NULL AS terminal_reason, 0 AS retired_location_count",
        ),
        (
            "fewer observations than non-empty batches",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'requested' AS status, 2 AS next_sequence, 2 AS batch_count, \
             1 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, '1970-01-01T00:00:00Z' AS requested_at, NULL AS started_at, \
             NULL AS terminal_at, NULL AS terminal_reason, 0 AS retired_location_count",
        ),
        (
            "more observations than bounded batches can contain",
            "SELECT 1 AS id, 9000001 AS storage_root_id, 1 AS root_epoch, 9000001 AS owner_node_id, \
             NULL AS owner_incarnation_id, 'requested' AS status, 1 AS next_sequence, 1 AS batch_count, \
             1001 AS observation_count, 300 AS idle_timeout_seconds, '1970-01-01T00:05:00Z' AS progress_deadline_at, \
             NULL AS location_high_watermark_id, '1970-01-01T00:00:00Z' AS requested_at, NULL AS started_at, \
             NULL AS terminal_at, NULL AS terminal_reason, 0 AS retired_location_count",
        ),
    ] {
        let row = sqlx::query(sql).fetch_one(&pool).await.unwrap();
        let error = super::row_to_scan_session(&row).unwrap_err();
        assert!(
            matches!(error, VoomError::Database { .. }),
            "{name}: {error:?}"
        );
    }
}

struct ObservationFixture<'a> {
    locator: &'a str,
    object_identity: &'a str,
    size_bytes: i64,
    modified_at: &'a str,
    stability_started_at: &'a str,
    stability_confirmed_at: &'a str,
}

const VALID_OBSERVATION: ObservationFixture<'static> = ObservationFixture {
    locator: "valid/locator",
    object_identity: "object",
    size_bytes: 1,
    modified_at: "1970-01-01T00:00:00Z",
    stability_started_at: "1970-01-01T00:00:00Z",
    stability_confirmed_at: "1970-01-01T00:00:00Z",
};

async fn observation_row(
    pool: &sqlx::SqlitePool,
    fixture: &ObservationFixture<'_>,
) -> sqlx::sqlite::SqliteRow {
    sqlx::query(
        "SELECT ? AS provider_relative_locator, ? AS provider_object_identity, ? AS size_bytes, \
         ? AS modified_at, ? AS stability_started_at, ? AS stability_confirmed_at",
    )
    .bind(fixture.locator)
    .bind(fixture.object_identity)
    .bind(fixture.size_bytes)
    .bind(fixture.modified_at)
    .bind(fixture.stability_started_at)
    .bind(fixture.stability_confirmed_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn assert_observation_decoder_rejects(
    pool: &sqlx::SqlitePool,
    fixture: ObservationFixture<'_>,
    name: &str,
) {
    let row = observation_row(pool, &fixture).await;
    let error = super::decode_observation_row(&row).unwrap_err();
    assert!(
        matches!(error, VoomError::Database { .. }),
        "{name}: {error:?}"
    );
}

#[tokio::test]
async fn scan_observation_row_decoder_rejects_locator_identity_and_size_corruption() {
    let (pool, _tmp) = fresh_pool().await;
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            locator: "bad//locator",
            ..VALID_OBSERVATION
        },
        "invalid locator",
    )
    .await;
    let oversize_object_identity = "o".repeat(4_097);
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            object_identity: &oversize_object_identity,
            ..VALID_OBSERVATION
        },
        "oversize object identity",
    )
    .await;
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            locator: "single\\backslash",
            ..VALID_OBSERVATION
        },
        "backslash locator",
    )
    .await;
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            object_identity: "",
            ..VALID_OBSERVATION
        },
        "empty object identity",
    )
    .await;
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            object_identity: "object\0identity",
            ..VALID_OBSERVATION
        },
        "NUL object identity",
    )
    .await;
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            size_bytes: -1,
            ..VALID_OBSERVATION
        },
        "negative size",
    )
    .await;
}

#[tokio::test]
async fn scan_observation_row_decoder_rejects_timestamp_and_combined_corruption() {
    let (pool, _tmp) = fresh_pool().await;
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            modified_at: "not-a-time",
            ..VALID_OBSERVATION
        },
        "invalid modified timestamp",
    )
    .await;
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            stability_started_at: "not-a-time",
            ..VALID_OBSERVATION
        },
        "invalid stability start timestamp",
    )
    .await;
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            stability_confirmed_at: "not-a-time",
            ..VALID_OBSERVATION
        },
        "invalid stability confirmation timestamp",
    )
    .await;
    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            stability_started_at: "1970-01-01T00:01:00Z",
            ..VALID_OBSERVATION
        },
        "reversed stability timestamps",
    )
    .await;

    assert_observation_decoder_rejects(
        &pool,
        ObservationFixture {
            locator: "bad//locator",
            object_identity: "",
            size_bytes: -1,
            modified_at: "not-a-time",
            stability_started_at: "not-a-time",
            stability_confirmed_at: "not-a-time",
        },
        "combined corruption",
    )
    .await;
}

fn new_session(storage_root_id: StorageRootId) -> NewScanSession {
    NewScanSession {
        storage_root_id,
        root_epoch: 1,
        owner_node_id: NodeId(9_000_001),
        idle_timeout_seconds: 300,
        progress_deadline_at: T0 + time::Duration::minutes(5),
        requested_at: T0,
    }
}

async fn seed_incarnation(pool: &sqlx::SqlitePool, incarnation: &str) {
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES (?, 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(incarnation)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_other_node_incarnation(pool: &sqlx::SqlitePool, incarnation: &str) {
    sqlx::query(
        "INSERT INTO nodes (id, name, kind, status, registered_at, last_seen_at, retired_at, \
         heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata, epoch) \
         VALUES (9000002, 'other-scan-owner', 'local', 'active', '1970-01-01T00:00:00Z', \
         '1970-01-01T00:00:00Z', NULL, 60, 'other-hash', 'other-hint', '{}', 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES (?, 9000002, 'active', '1970-01-01T00:00:00Z', \
         '1970-01-01T00:00:00Z')",
    )
    .bind(incarnation)
    .execute(pool)
    .await
    .unwrap();
}

async fn corrupt_session_owner_incarnation(
    pool: &sqlx::SqlitePool,
    session: ScanSessionId,
    incarnation: &str,
) {
    let mut connection = pool.acquire().await.unwrap();
    connection.close_on_drop();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE scan_sessions SET owner_incarnation_id = ? WHERE id = ?")
        .bind(incarnation)
        .bind(i64::try_from(session.0).unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
}

async fn remove_batch_update_guard(pool: &sqlx::SqlitePool) {
    sqlx::query("DROP TRIGGER scan_observation_batches_no_update")
        .execute(pool)
        .await
        .unwrap();
}

async fn remove_batch_delete_guard(pool: &sqlx::SqlitePool) {
    sqlx::query("DROP TRIGGER scan_observation_batches_no_delete")
        .execute(pool)
        .await
        .unwrap();
}

async fn remove_batch_parent_frontier_guard(pool: &sqlx::SqlitePool) {
    sqlx::query("DROP TRIGGER IF EXISTS scan_observation_batches_validate_parent_frontier")
        .execute(pool)
        .await
        .unwrap();
}

async fn seed_second_root(pool: &sqlx::SqlitePool) -> StorageRootId {
    sqlx::query(
        "INSERT INTO libraries (id, slug, display_name, media_kind, description, enabled, created_at, updated_at) \
         VALUES (9000002, 'repository-test-root-two', 'Repository Test Root Two', 'unknown', NULL, 1, \
         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_roots (id, library_id, owner_node_id, provider_kind, provider_locator, display_locator, \
         state, root_epoch, activation_identity, include_globs, exclude_globs, extension_allowlist, scan_mode, \
         symlink_policy, hidden_file_policy, max_depth, stability_seconds, debounce_seconds, default_output_root_id, \
         default_staging_root_id, default_backup_root_id, enabled, created_at, updated_at) \
         VALUES (9000002, 9000002, 9000001, 'local_filesystem', '/two', '/two', 'active', 1, 'root-two', \
         '[]', '[]', '[]', 'manual_recursive', 'reject', 'ignore', NULL, 0, 0, NULL, NULL, NULL, 1, \
         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    StorageRootId(9_000_002)
}

async fn seed_rooted_location(
    pool: &sqlx::SqlitePool,
    storage_root_id: StorageRootId,
    locator: &str,
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
    .bind(format!("scan-session-{locator}"))
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO file_locations (file_version_id, address_state, storage_root_id, \
         provider_relative_locator, legacy_kind, legacy_locator, proof_kind, proof_value, \
         observed_at, retired_at, epoch) VALUES (?, 'rooted', ?, ?, NULL, NULL, NULL, NULL, \
         '1970-01-01T00:00:00Z', NULL, 0)",
    )
    .bind(version_id)
    .bind(i64::try_from(storage_root_id.0).unwrap())
    .bind(locator)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
}

async fn seed_succeeded_session(
    pool: &sqlx::SqlitePool,
    storage_root_id: StorageRootId,
    high_watermark_id: Option<i64>,
    retired_location_count: u64,
    terminal_at: &str,
) -> ScanSessionId {
    seed_incarnation(pool, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").await;
    let id = sqlx::query(
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, \
         owner_incarnation_id, status, idle_timeout_seconds, progress_deadline_at, \
         location_high_watermark_id, requested_at, started_at, terminal_at, \
         retired_location_count) VALUES (?, 1, 9000001, ?, 'succeeded', 300, \
         '1970-01-01T00:05:00Z', ?, '1970-01-01T00:00:00Z', \
         '1970-01-01T00:01:00Z', ?, ?)",
    )
    .bind(i64::try_from(storage_root_id.0).unwrap())
    .bind("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    .bind(high_watermark_id)
    .bind(terminal_at)
    .bind(i64::try_from(retired_location_count).unwrap())
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    ScanSessionId(u64::try_from(id).unwrap())
}

async fn attribute_location(
    pool: &sqlx::SqlitePool,
    location_id: i64,
    session_id: ScanSessionId,
    retired_at: &str,
) {
    sqlx::query(
        "UPDATE file_locations SET retired_at = ?, retired_by_scan_session_id = ?, epoch = 1 \
         WHERE id = ?",
    )
    .bind(retired_at)
    .bind(i64::try_from(session_id.0).unwrap())
    .bind(location_id)
    .execute(pool)
    .await
    .unwrap();
}

fn observation(locator: &str) -> ScanObservation {
    ScanObservation {
        provider_relative_locator: ProviderRelativeLocator::new(locator.to_owned()).unwrap(),
        provider_object_identity: format!("identity-{locator}"),
        size_bytes: 1,
        modified_at: T0,
        stability_started_at: T0,
        stability_confirmed_at: T0,
        evidence: None,
    }
}

fn batch(
    session_id: ScanSessionId,
    sequence: u64,
    request_hash: char,
    observations: Vec<ScanObservation>,
) -> NewScanObservationBatch {
    NewScanObservationBatch {
        scan_session_id: session_id,
        sequence,
        request_hash: request_hash.to_string().repeat(64),
        observations,
        accepted_at: T0,
        next_progress_deadline_at: T0 + time::Duration::minutes(5),
    }
}

#[tokio::test]
async fn request_snapshots_root_and_start_binds_incarnation_and_high_watermark() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let other_root = seed_second_root(&pool).await;
    let expected_high_watermark = seed_rooted_location(&pool, root, "live.mkv").await;
    let retired = seed_rooted_location(&pool, root, "retired.mkv").await;
    sqlx::query("UPDATE file_locations SET retired_at = ? WHERE id = ?")
        .bind("1970-01-01T00:01:00Z")
        .bind(retired)
        .execute(&pool)
        .await
        .unwrap();
    seed_rooted_location(&pool, other_root, "other.mkv").await;
    seed_incarnation(&pool, "11111111111111111111111111111111").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();

    let requested = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    assert_eq!(requested.storage_root_id, root);
    assert_eq!(requested.root_epoch, 1);
    assert_eq!(requested.status, ScanSessionStatus::Requested);
    assert_eq!(requested.owner_incarnation_id, None);

    let incarnation = "11111111111111111111111111111111".parse().unwrap();
    let started = repo
        .start_in_tx(
            &mut tx,
            requested.id,
            incarnation,
            T0 + time::Duration::minutes(10),
            T0 + time::Duration::minutes(5),
        )
        .await
        .unwrap();
    assert_eq!(started.status, ScanSessionStatus::Running);
    assert_eq!(started.owner_incarnation_id, Some(incarnation));
    assert_eq!(
        started.location_high_watermark_id,
        Some(voom_core::FileLocationId(
            u64::try_from(expected_high_watermark).unwrap()
        ))
    );
    assert_eq!(
        started.progress_deadline_at,
        T0 + time::Duration::minutes(10)
    );

    let duplicate_start = repo
        .start_in_tx(
            &mut tx,
            requested.id,
            incarnation,
            T0 + time::Duration::minutes(10),
            T0 + time::Duration::minutes(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(duplicate_start, VoomError::Conflict(_)));
}

#[tokio::test]
async fn request_rejects_timeout_bounds_before_inserting() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();

    for timeout in [0, 86_401] {
        let error = repo
            .insert_requested_in_tx(
                &mut tx,
                NewScanSession {
                    idle_timeout_seconds: timeout,
                    ..new_session(root)
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, VoomError::Config(_)));
    }
    let accepted = repo
        .insert_requested_in_tx(
            &mut tx,
            NewScanSession {
                idle_timeout_seconds: 86_400,
                ..new_session(root)
            },
        )
        .await
        .unwrap();
    assert_eq!(accepted.idle_timeout_seconds, 86_400);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_sessions")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn concurrent_requests_have_one_same_root_winner_and_independent_roots_both_win() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let other_root = seed_second_root(&pool).await;
    let request = |storage_root_id| {
        let pool = pool.clone();
        async move {
            let repo = SqliteScanSessionRepo::new(pool.clone());
            let mut tx = pool.begin().await.unwrap();
            let result = repo
                .insert_requested_in_tx(&mut tx, new_session(storage_root_id))
                .await;
            match result {
                Ok(session) => {
                    tx.commit().await.unwrap();
                    Ok(session)
                }
                Err(error) => {
                    tx.rollback().await.unwrap();
                    Err(error)
                }
            }
        }
    };
    let (left, right) = tokio::join!(request(root), request(root));
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let active = repo
        .active_for_root_in_tx(&mut tx, root)
        .await
        .unwrap()
        .unwrap();
    repo.terminalize_in_tx(
        &mut tx,
        active.id,
        ScanSessionStatus::Cancelled,
        voom_core::ScanTerminalReason::new("test reset").unwrap(),
        T0,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (left, right) = tokio::join!(request(other_root), request(root));
    assert!(left.is_ok());
    assert!(right.is_ok());
}

#[tokio::test]
async fn stale_expiry_is_set_based_at_the_exact_deadline_and_returns_post_transition_rows() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let other_root = seed_second_root(&pool).await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let requested = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    let earlier = repo
        .insert_requested_in_tx(
            &mut tx,
            NewScanSession {
                progress_deadline_at: T0 + time::Duration::minutes(4),
                ..new_session(other_root)
            },
        )
        .await
        .unwrap();

    let expired = repo
        .stale_expired_in_tx(&mut tx, T0 + time::Duration::minutes(5))
        .await
        .unwrap();
    assert_eq!(expired.len(), 2);
    assert_eq!(expired[0].id, requested.id);
    assert_eq!(expired[1].id, earlier.id);
    assert!(
        expired
            .iter()
            .all(|session| session.status == ScanSessionStatus::Stale)
    );
    assert_eq!(
        expired[0].terminal_at,
        Some(T0 + time::Duration::minutes(5))
    );
}

#[tokio::test]
async fn stale_expiry_compares_valid_non_utc_deadlines_chronologically() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let requested = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    sqlx::query("UPDATE scan_sessions SET progress_deadline_at = ? WHERE id = ?")
        .bind("1970-01-01T01:00:00+01:00")
        .bind(i64::try_from(requested.id.0).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();

    let expired = repo.stale_expired_in_tx(&mut tx, T0).await.unwrap();

    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].id, requested.id);
    assert_eq!(expired[0].status, ScanSessionStatus::Stale);
}

async fn assert_malformed_deadline_rejects_without_partial_staleness(deadline: &str) {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let other_root = seed_second_root(&pool).await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let valid = repo
        .insert_requested_in_tx(
            &mut tx,
            NewScanSession {
                progress_deadline_at: T0,
                ..new_session(root)
            },
        )
        .await
        .unwrap();
    let malformed = repo
        .insert_requested_in_tx(&mut tx, new_session(other_root))
        .await
        .unwrap();
    sqlx::query("UPDATE scan_sessions SET progress_deadline_at = ? WHERE id = ?")
        .bind(deadline)
        .bind(i64::try_from(malformed.id.0).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();

    let error = repo.stale_expired_in_tx(&mut tx, T0).await.unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    tx.commit().await.unwrap();

    for id in [valid.id, malformed.id] {
        let status: String = sqlx::query_scalar("SELECT status FROM scan_sessions WHERE id = ?")
            .bind(i64::try_from(id.0).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "requested");
    }
}

#[tokio::test]
async fn stale_expiry_decodes_every_active_deadline_before_any_mutation() {
    for deadline in ["0000-not-a-time", "zzzz-not-a-time"] {
        assert_malformed_deadline_rejects_without_partial_staleness(deadline).await;
    }
}

#[tokio::test]
async fn stale_running_for_incarnation_returns_checked_rows_in_id_order() {
    let (pool, _tmp) = fresh_pool().await;
    let first_root = seed_test_storage_root(&pool).await.unwrap();
    let second_root = seed_second_root(&pool).await;
    let incarnation = "22222222222222222222222222222222";
    seed_incarnation(&pool, incarnation).await;
    let incarnation = incarnation.parse().unwrap();
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let first = repo
        .insert_requested_in_tx(&mut tx, new_session(first_root))
        .await
        .unwrap();
    let second = repo
        .insert_requested_in_tx(&mut tx, new_session(second_root))
        .await
        .unwrap();
    for session in [&first, &second] {
        repo.start_in_tx(
            &mut tx,
            session.id,
            incarnation,
            T0 + time::Duration::minutes(10),
            T0,
        )
        .await
        .unwrap();
    }

    let stale = repo
        .stale_running_for_incarnation_in_tx(
            &mut tx,
            incarnation,
            ScanTerminalReason::new("owner incarnation ended").unwrap(),
            T0 + time::Duration::minutes(1),
        )
        .await
        .unwrap();

    assert_eq!(
        stale.iter().map(|session| session.id).collect::<Vec<_>>(),
        vec![first.id, second.id]
    );
    assert!(stale.iter().all(|session| {
        session.status == ScanSessionStatus::Stale
            && session.owner_incarnation_id == Some(incarnation)
    }));
}

#[tokio::test]
async fn stale_running_for_incarnation_decodes_all_running_rows_before_mutation() {
    let (pool, _tmp) = fresh_pool().await;
    let first_root = seed_test_storage_root(&pool).await.unwrap();
    let second_root = seed_second_root(&pool).await;
    let target = "22222222222222222222222222222222";
    seed_incarnation(&pool, target).await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let target_session = repo
        .insert_requested_in_tx(&mut tx, new_session(first_root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        target_session.id,
        target.parse().unwrap(),
        T0 + time::Duration::minutes(10),
        T0,
    )
    .await
    .unwrap();
    let corrupt_session = repo
        .insert_requested_in_tx(&mut tx, new_session(second_root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        corrupt_session.id,
        target.parse().unwrap(),
        T0 + time::Duration::minutes(10),
        T0,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    with_check_constraints_disabled(&pool, |connection| {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO node_incarnations \
                 (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
                 VALUES ('not-an-incarnation', 9000001, 'superseded', \
                         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                         '1970-01-01T00:00:00Z', 'superseded')",
            )
            .execute(&mut *connection)
            .await?;
            sqlx::query("UPDATE scan_sessions SET owner_incarnation_id = ? WHERE id = ?")
                .bind("not-an-incarnation")
                .bind(i64::try_from(corrupt_session.id.0).unwrap())
                .execute(connection)
                .await
        })
    })
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();

    let error = repo
        .stale_running_for_incarnation_in_tx(
            &mut tx,
            target.parse().unwrap(),
            ScanTerminalReason::new("owner incarnation ended").unwrap(),
            T0 + time::Duration::minutes(1),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert_eq!(
        repo.get_in_tx(&mut tx, target_session.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ScanSessionStatus::Running
    );
}

#[tokio::test]
async fn mutation_and_recovery_reads_reject_cross_node_incarnation_before_updates() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let owner_incarnation = "12121212121212121212121212121212";
    let other_incarnation = "34343434343434343434343434343434";
    seed_incarnation(&pool, owner_incarnation).await;
    seed_other_node_incarnation(&pool, other_incarnation).await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        owner_incarnation.parse().unwrap(),
        T0,
        T0,
    )
    .await
    .unwrap();
    let accepted = batch(session.id, 0, 'a', vec![observation("cross-node.mkv")]);
    repo.accepted_batch_in_tx(&mut tx, accepted.clone())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    corrupt_session_owner_incarnation(&pool, session.id, other_incarnation).await;

    for incarnation_scope in [false, true] {
        let mut tx = pool.begin().await.unwrap();
        let error = if incarnation_scope {
            repo.stale_running_for_incarnation_in_tx(
                &mut tx,
                other_incarnation.parse().unwrap(),
                voom_core::ScanTerminalReason::new("cross-node recovery").unwrap(),
                T0,
            )
            .await
            .unwrap_err()
        } else {
            repo.stale_expired_in_tx(&mut tx, T0 + time::Duration::minutes(5))
                .await
                .unwrap_err()
        };
        assert!(matches!(error, VoomError::Database { .. }));
        tx.rollback().await.unwrap();
    }

    let mut tx = pool.begin().await.unwrap();
    let error = repo
        .accepted_batch_in_tx(&mut tx, accepted)
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    tx.rollback().await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    let error = repo
        .terminalize_in_tx(
            &mut tx,
            session.id,
            ScanSessionStatus::Stale,
            voom_core::ScanTerminalReason::new("cross-node corruption").unwrap(),
            T0,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    tx.rollback().await.unwrap();

    let state: (String, i64, i64) = sqlx::query_as(
        "SELECT status, next_sequence, observation_count FROM scan_sessions WHERE id = ?",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, ("running".to_owned(), 1, 1));
}

#[tokio::test]
async fn start_read_rejects_cross_node_incarnation_with_foreign_keys_disabled() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let other_incarnation = "45454545454545454545454545454545";
    seed_other_node_incarnation(&pool, other_incarnation).await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut connection = pool.acquire().await.unwrap();
    connection.close_on_drop();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    let mut tx = connection.begin().await.unwrap();
    let error = repo
        .start_in_tx(
            &mut tx,
            session.id,
            other_incarnation.parse().unwrap(),
            T0 + time::Duration::minutes(5),
            T0,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    tx.rollback().await.unwrap();
    connection.close().await.unwrap();

    let stored = repo.get(session.id).await.unwrap().unwrap();
    assert_eq!(stored.status, ScanSessionStatus::Requested);
    assert_eq!(stored.owner_incarnation_id, None);
}

#[tokio::test]
async fn batch_acceptance_replays_the_same_session_sequence_and_rejects_conflicts_without_rows() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "22222222222222222222222222222222").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    let incarnation = "22222222222222222222222222222222".parse().unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        incarnation,
        T0 + time::Duration::minutes(10),
        T0 + time::Duration::minutes(5),
    )
    .await
    .unwrap();

    let batch = NewScanObservationBatch {
        scan_session_id: session.id,
        sequence: 0,
        request_hash: "a".repeat(64),
        observations: vec![ScanObservation {
            provider_relative_locator: ProviderRelativeLocator::new("one.mkv".to_owned()).unwrap(),
            provider_object_identity: "opaque-id".to_owned(),
            size_bytes: 1,
            modified_at: T0,
            stability_started_at: T0,
            stability_confirmed_at: T0,
            evidence: None,
        }],
        accepted_at: T0 + time::Duration::minutes(5),
        next_progress_deadline_at: T0 + time::Duration::minutes(10),
    };
    let accepted = repo
        .accepted_batch_in_tx(&mut tx, batch.clone())
        .await
        .unwrap();
    let replay = repo
        .accepted_batch_in_tx(&mut tx, batch.clone())
        .await
        .unwrap();
    assert_eq!(accepted, replay);
    let gap = repo
        .accepted_batch_in_tx(
            &mut tx,
            NewScanObservationBatch {
                sequence: 2,
                request_hash: "f".repeat(64),
                ..batch.clone()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(gap, VoomError::Conflict(_)));
    let thousand = (0_u64..1_000)
        .map(|ordinal| ScanObservation {
            provider_relative_locator: ProviderRelativeLocator::new(format!("many/{ordinal}.mkv"))
                .unwrap(),
            provider_object_identity: format!("identity-{ordinal}"),
            size_bytes: ordinal,
            modified_at: T0,
            stability_started_at: T0,
            stability_confirmed_at: T0,
            evidence: None,
        })
        .collect();
    let outcome = repo
        .accepted_batch_in_tx(
            &mut tx,
            NewScanObservationBatch {
                sequence: 1,
                request_hash: "f".repeat(64),
                observations: thousand,
                ..batch.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome.accepted_observation_count, 1_000);
    let conflict = repo
        .accepted_batch_in_tx(
            &mut tx,
            NewScanObservationBatch {
                request_hash: "b".repeat(64),
                ..batch
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(conflict, VoomError::Conflict(_)));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_observations")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 1_001);
}

#[tokio::test]
async fn batch_locator_conflict_leaves_no_partial_ledger_row_when_the_caller_commits() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "33333333333333333333333333333333").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        "33333333333333333333333333333333".parse().unwrap(),
        T0 + time::Duration::minutes(10),
        T0,
    )
    .await
    .unwrap();
    let first = NewScanObservationBatch {
        scan_session_id: session.id,
        sequence: 0,
        request_hash: "c".repeat(64),
        observations: vec![ScanObservation {
            provider_relative_locator: ProviderRelativeLocator::new("same.mkv".to_owned()).unwrap(),
            provider_object_identity: "first-identity".to_owned(),
            size_bytes: 1,
            modified_at: T0,
            stability_started_at: T0,
            stability_confirmed_at: T0,
            evidence: None,
        }],
        accepted_at: T0,
        next_progress_deadline_at: T0 + time::Duration::minutes(5),
    };
    repo.accepted_batch_in_tx(&mut tx, first).await.unwrap();
    let duplicate = NewScanObservationBatch {
        sequence: 1,
        request_hash: "d".repeat(64),
        observations: vec![ScanObservation {
            provider_relative_locator: ProviderRelativeLocator::new("same.mkv".to_owned()).unwrap(),
            provider_object_identity: "second-identity".to_owned(),
            size_bytes: 2,
            modified_at: T0,
            stability_started_at: T0,
            stability_confirmed_at: T0,
            evidence: None,
        }],
        accepted_at: T0,
        next_progress_deadline_at: T0 + time::Duration::minutes(5),
        scan_session_id: session.id,
    };
    let error = repo
        .accepted_batch_in_tx(&mut tx, duplicate)
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Conflict(_)));
    assert!(error.to_string().contains("same.mkv"));
    assert!(!error.to_string().contains("second-identity"));
    tx.commit().await.unwrap();
    let batch_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_observation_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(batch_count, 1);
}

#[tokio::test]
async fn batch_rejections_and_cross_session_hash_reuse_preserve_each_sessions_counts() {
    let (pool, _tmp) = fresh_pool().await;
    let first_root = seed_test_storage_root(&pool).await.unwrap();
    let second_root = seed_second_root(&pool).await;
    seed_incarnation(&pool, "44444444444444444444444444444444").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let first = repo
        .insert_requested_in_tx(&mut tx, new_session(first_root))
        .await
        .unwrap();
    let second = repo
        .insert_requested_in_tx(&mut tx, new_session(second_root))
        .await
        .unwrap();
    let incarnation = "44444444444444444444444444444444".parse().unwrap();
    for session in [first.id, second.id] {
        repo.start_in_tx(
            &mut tx,
            session,
            incarnation,
            T0 + time::Duration::minutes(5),
            T0,
        )
        .await
        .unwrap();
    }

    let first_outcome = repo
        .accepted_batch_in_tx(
            &mut tx,
            batch(first.id, 0, 'a', vec![observation("same.mkv")]),
        )
        .await
        .unwrap();
    let second_outcome = repo
        .accepted_batch_in_tx(
            &mut tx,
            batch(second.id, 0, 'a', vec![observation("same.mkv")]),
        )
        .await
        .unwrap();
    assert_eq!(first_outcome.scan_session_id, first.id);
    assert_eq!(second_outcome.scan_session_id, second.id);

    let within_body = batch(
        first.id,
        1,
        'b',
        vec![observation("duplicate.mkv"), observation("duplicate.mkv")],
    );
    let error = repo
        .accepted_batch_in_tx(&mut tx, within_body)
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Conflict(_)));
    assert!(error.to_string().contains("duplicate.mkv"));
    let across_bodies = batch(first.id, 1, 'c', vec![observation("same.mkv")]);
    assert!(matches!(
        repo.accepted_batch_in_tx(&mut tx, across_bodies)
            .await
            .unwrap_err(),
        VoomError::Conflict(_)
    ));
    tx.commit().await.unwrap();
    let first_id = i64::try_from(first.id.0).unwrap();
    with_check_constraints_disabled(&pool, move |connection| {
        Box::pin(async move {
            sqlx::query("UPDATE scan_sessions SET next_sequence = 2 WHERE id = ?")
                .bind(first_id)
                .execute(connection)
                .await
        })
    })
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let regression = batch(first.id, 1, 'd', vec![observation("regression.mkv")]);
    assert!(matches!(
        repo.accepted_batch_in_tx(&mut tx, regression)
            .await
            .unwrap_err(),
        VoomError::Database { .. }
    ));
    tx.commit().await.unwrap();
    sqlx::query("UPDATE scan_sessions SET next_sequence = 1 WHERE id = ?")
        .bind(first_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();

    let batch_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_observation_batches")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let observation_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_observations")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!((batch_count, observation_count), (2, 2));
    for session in [first.id, second.id] {
        let stored = repo.get_in_tx(&mut tx, session).await.unwrap().unwrap();
        assert_eq!((stored.batch_count, stored.observation_count), (1, 1));
    }
}

#[derive(Clone, Copy)]
enum BatchLedgerCorruption {
    RequestHash,
    ObservationCount,
    CumulativeCount,
}

async fn assert_batch_replay_rejects_corruption(case: BatchLedgerCorruption) {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "66666666666666666666666666666666").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        "66666666666666666666666666666666".parse().unwrap(),
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    let input = batch(session.id, 0, 'a', vec![observation("corrupt.mkv")]);
    repo.accepted_batch_in_tx(&mut tx, input.clone())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    remove_batch_update_guard(&pool).await;
    with_check_constraints_disabled(&pool, |connection| {
        Box::pin(async move {
            match case {
                BatchLedgerCorruption::RequestHash => {
                    sqlx::query("UPDATE scan_observation_batches SET request_hash = 'invalid'")
                        .execute(connection)
                        .await
                }
                BatchLedgerCorruption::ObservationCount => {
                    sqlx::query("UPDATE scan_observation_batches SET observation_count = 0")
                        .execute(connection)
                        .await
                }
                BatchLedgerCorruption::CumulativeCount => {
                    sqlx::query(
                        "UPDATE scan_observation_batches SET cumulative_observation_count = 0",
                    )
                    .execute(connection)
                    .await
                }
            }
        })
    })
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let error = repo.accepted_batch_in_tx(&mut tx, input).await.unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
}

#[tokio::test]
async fn batch_replay_validates_the_stored_hash_and_outcome_before_returning_it() {
    for case in [
        BatchLedgerCorruption::RequestHash,
        BatchLedgerCorruption::ObservationCount,
        BatchLedgerCorruption::CumulativeCount,
    ] {
        assert_batch_replay_rejects_corruption(case).await;
    }
}

#[tokio::test]
async fn batch_insert_rejects_a_frontier_not_owned_by_the_parent_session() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "67676767676767676767676767676767").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        "67676767676767676767676767676767".parse().unwrap(),
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();

    let error = sqlx::query(
        "INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
         request_hash, observation_count, accepted_at, cumulative_observation_count) \
         VALUES (?, 0, NULL, ?, 1, '1970-01-01T00:00:00Z', 1)",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .bind("a".repeat(64))
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("parent frontier mismatch"));
}

#[tokio::test]
async fn batch_replay_rejects_a_cached_outcome_ahead_of_the_parent_frontier() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "68686868686868686868686868686868").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        "68686868686868686868686868686868".parse().unwrap(),
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    remove_batch_parent_frontier_guard(&pool).await;
    let session_id = i64::try_from(session.id.0).unwrap();
    with_check_constraints_disabled(&pool, move |connection| {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO scan_observation_batches (scan_session_id, sequence, \
                 previous_sequence, request_hash, observation_count, accepted_at, \
                 cumulative_observation_count) VALUES (?, 0, NULL, ?, 1, \
                 '1970-01-01T00:00:00Z', 1)",
            )
            .bind(session_id)
            .bind("a".repeat(64))
            .execute(connection)
            .await
        })
    })
    .await
    .unwrap();

    let input = batch(session.id, 0, 'a', vec![observation("ahead.mkv")]);
    let mut tx = pool.begin().await.unwrap();
    let error = repo.accepted_batch_in_tx(&mut tx, input).await.unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("does not match parent frontier"));
}

#[tokio::test]
async fn older_batch_replay_rejects_a_missing_parent_frontier_batch() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "69696969696969696969696969696969").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        "69696969696969696969696969696969".parse().unwrap(),
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    let first = batch(session.id, 0, 'a', vec![observation("older/first.mkv")]);
    let second = batch(session.id, 1, 'b', vec![observation("older/second.mkv")]);
    repo.accepted_batch_in_tx(&mut tx, first.clone())
        .await
        .unwrap();
    repo.accepted_batch_in_tx(&mut tx, second).await.unwrap();
    tx.commit().await.unwrap();

    sqlx::query("DELETE FROM scan_observations WHERE scan_session_id = ? AND batch_sequence = 1")
        .bind(i64::try_from(session.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    remove_batch_delete_guard(&pool).await;
    sqlx::query("DELETE FROM scan_observation_batches WHERE scan_session_id = ? AND sequence = 1")
        .bind(i64::try_from(session.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let error = repo.accepted_batch_in_tx(&mut tx, first).await.unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("missing frontier batch 1"));
}

#[derive(Clone, Copy)]
enum BatchLinkCorruption {
    MissingPredecessor,
    PredecessorCumulative,
}

async fn assert_batch_link_corruption_rejected(case: BatchLinkCorruption) {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "56565656565656565656565656565656").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        "56565656565656565656565656565656".parse().unwrap(),
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    let first = batch(session.id, 0, 'a', vec![observation("chain/first.mkv")]);
    let second = batch(session.id, 1, 'b', vec![observation("chain/second.mkv")]);
    repo.accepted_batch_in_tx(&mut tx, first.clone())
        .await
        .unwrap();
    repo.accepted_batch_in_tx(&mut tx, second.clone())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    match case {
        BatchLinkCorruption::MissingPredecessor => {
            remove_batch_delete_guard(&pool).await;
            let mut connection = pool.acquire().await.unwrap();
            connection.close_on_drop();
            sqlx::query("PRAGMA foreign_keys = OFF")
                .execute(&mut *connection)
                .await
                .unwrap();
            sqlx::query(
                "DELETE FROM scan_observation_batches WHERE scan_session_id = ? AND sequence = 0",
            )
            .bind(i64::try_from(session.id.0).unwrap())
            .execute(&mut *connection)
            .await
            .unwrap();
            connection.close().await.unwrap();
        }
        BatchLinkCorruption::PredecessorCumulative => {
            remove_batch_update_guard(&pool).await;
            sqlx::query(
                "UPDATE scan_observation_batches SET cumulative_observation_count = 2 \
                 WHERE scan_session_id = ? AND sequence = 0",
            )
            .bind(i64::try_from(session.id.0).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        }
    }

    let before: (i64, i64, i64, String) = sqlx::query_as(
        "SELECT next_sequence, batch_count, observation_count, progress_deadline_at \
         FROM scan_sessions WHERE id = ?",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    let third = batch(session.id, 2, 'c', vec![observation("chain/third.mkv")]);
    let mut tx = pool.begin().await.unwrap();
    for input in [second.clone(), third.clone()] {
        let error = repo.accepted_batch_in_tx(&mut tx, input).await.unwrap_err();
        assert!(matches!(error, VoomError::Database { .. }));
    }
    tx.commit().await.unwrap();
    let after: (i64, i64, i64, String) = sqlx::query_as(
        "SELECT next_sequence, batch_count, observation_count, progress_deadline_at \
         FROM scan_sessions WHERE id = ?",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after, before);

    match case {
        BatchLinkCorruption::MissingPredecessor => {
            remove_batch_parent_frontier_guard(&pool).await;
            sqlx::query(
                "INSERT INTO scan_observation_batches (scan_session_id, sequence, \
                 previous_sequence, request_hash, observation_count, accepted_at, \
                 cumulative_observation_count) VALUES (?, 0, NULL, ?, 1, \
                 '1970-01-01T00:00:00Z', 1)",
            )
            .bind(i64::try_from(session.id.0).unwrap())
            .bind("a".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        }
        BatchLinkCorruption::PredecessorCumulative => {
            sqlx::query(
                "UPDATE scan_observation_batches SET cumulative_observation_count = 1 \
                 WHERE scan_session_id = ? AND sequence = 0",
            )
            .bind(i64::try_from(session.id.0).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        repo.accepted_batch_in_tx(&mut tx, second)
            .await
            .unwrap()
            .sequence,
        1
    );
    assert_eq!(
        repo.accepted_batch_in_tx(&mut tx, third)
            .await
            .unwrap()
            .sequence,
        2
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn new_and_replayed_batches_reject_broken_immediate_links_until_repaired() {
    for case in [
        BatchLinkCorruption::MissingPredecessor,
        BatchLinkCorruption::PredecessorCumulative,
    ] {
        assert_batch_link_corruption_rejected(case).await;
    }
}

#[tokio::test]
async fn completion_global_backstop_rejects_deeper_batch_link_corruption() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let absent = seed_rooted_location(&pool, root, "completion/deeper-absent.mkv").await;
    let incarnation = "90909090909090909090909090909090";
    seed_incarnation(&pool, incarnation).await;
    let incarnation_id = incarnation.parse().unwrap();
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        incarnation_id,
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    for (sequence, hash) in [(0, 'a'), (1, 'b'), (2, 'c')] {
        repo.accepted_batch_in_tx(
            &mut tx,
            batch(
                session.id,
                sequence,
                hash,
                vec![observation(&format!("completion/deeper-{sequence}.mkv"))],
            ),
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    remove_batch_update_guard(&pool).await;
    sqlx::query(
        "UPDATE scan_observation_batches SET cumulative_observation_count = 2 \
         WHERE scan_session_id = ? AND sequence = 0",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .execute(&pool)
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let error = repo
        .complete_in_tx(
            &mut tx,
            CompleteScanSessionInput {
                scan_session_id: session.id,
                expected_storage_root_id: root,
                expected_root_epoch: 1,
                expected_owner_node_id: NodeId(9_000_001),
                expected_owner_incarnation_id: incarnation_id,
                last_sequence: Some(2),
                observation_count: 3,
                completed_at: T0 + time::Duration::minutes(1),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    tx.rollback().await.unwrap();
    let state: String = sqlx::query_scalar("SELECT status FROM scan_sessions WHERE id = ?")
        .bind(i64::try_from(session.id.0).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(absent)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "running");
    assert!(retired_at.is_none());
}

#[tokio::test]
async fn missing_batch_below_coherent_progress_is_database_and_repair_accepts_same_input() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "77777777777777777777777777777777").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        "77777777777777777777777777777777".parse().unwrap(),
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    repo.accepted_batch_in_tx(
        &mut tx,
        batch(session.id, 0, 'a', vec![observation("present.mkv")]),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let session_id = i64::try_from(session.id.0).unwrap();
    with_check_constraints_disabled(&pool, move |connection| {
        Box::pin(async move {
            sqlx::query(
                "UPDATE scan_sessions SET next_sequence = 2, batch_count = 2, \
                 observation_count = 2 WHERE id = ?",
            )
            .bind(session_id)
            .execute(connection)
            .await
        })
    })
    .await
    .unwrap();

    let input = batch(session.id, 1, 'b', vec![observation("repairable.mkv")]);
    let mut tx = pool.begin().await.unwrap();
    let error = repo
        .accepted_batch_in_tx(&mut tx, input.clone())
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    tx.commit().await.unwrap();
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), (SELECT COUNT(*) FROM scan_observations) \
         FROM scan_observation_batches",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 1));
    let progress: (i64, i64, i64) = sqlx::query_as(
        "SELECT next_sequence, batch_count, observation_count FROM scan_sessions WHERE id = ?",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(progress, (2, 2, 2));

    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = 1, batch_count = 1, \
         observation_count = 1 WHERE id = ?",
    )
    .bind(session_id)
    .execute(&pool)
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let outcome = repo.accepted_batch_in_tx(&mut tx, input).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(outcome.sequence, 1);
    assert_eq!(outcome.cumulative_observation_count, 2);
}

#[tokio::test]
async fn batch_validation_rejects_bounds_overflow_and_reversed_stability_before_sql() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let observation = ScanObservation {
        provider_relative_locator: ProviderRelativeLocator::new("one.mkv".to_owned()).unwrap(),
        provider_object_identity: "identity".to_owned(),
        size_bytes: 1,
        modified_at: T0,
        stability_started_at: T0,
        stability_confirmed_at: T0,
        evidence: None,
    };
    let batch = |observations: Vec<ScanObservation>| NewScanObservationBatch {
        scan_session_id: ScanSessionId(999),
        sequence: 0,
        request_hash: "e".repeat(64),
        observations,
        accepted_at: T0,
        next_progress_deadline_at: T0,
    };
    let mut tx = pool.begin().await.unwrap();
    let too_many = vec![observation.clone(); 1_001];
    assert!(matches!(
        repo.accepted_batch_in_tx(&mut tx, batch(too_many))
            .await
            .unwrap_err(),
        VoomError::Config(_)
    ));
    let overflow = ScanObservation {
        size_bytes: u64::MAX,
        ..observation.clone()
    };
    assert!(matches!(
        repo.accepted_batch_in_tx(&mut tx, batch(vec![overflow]))
            .await
            .unwrap_err(),
        VoomError::Config(_)
    ));
    let reversed = ScanObservation {
        stability_confirmed_at: T0 - time::Duration::seconds(1),
        evidence: None,
        ..observation
    };
    assert!(matches!(
        repo.accepted_batch_in_tx(&mut tx, batch(vec![reversed]))
            .await
            .unwrap_err(),
        VoomError::Config(_)
    ));
    tx.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_observation_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn session_list_uses_ascending_exclusive_keyset_pagination_and_validates_limits() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let other_root = seed_second_root(&pool).await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let first = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.terminalize_in_tx(
        &mut tx,
        first.id,
        ScanSessionStatus::Cancelled,
        voom_core::ScanTerminalReason::new("operator cancelled").unwrap(),
        T0,
    )
    .await
    .unwrap();
    let second = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    let other = repo
        .insert_requested_in_tx(&mut tx, new_session(other_root))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let first_page = repo
        .list(ScanSessionListQuery {
            storage_root_id: Some(root),
            status: None,
            after_id: None,
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(
        first_page.items,
        vec![repo.get(first.id).await.unwrap().unwrap()]
    );
    assert_eq!(first_page.next_after_id, Some(first.id));
    let second_page = repo
        .list(ScanSessionListQuery {
            storage_root_id: Some(root),
            status: None,
            after_id: first_page.next_after_id,
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(
        second_page.items,
        vec![repo.get(second.id).await.unwrap().unwrap()]
    );
    assert_eq!(second_page.next_after_id, None);
    let invalid_limit = repo
        .list(ScanSessionListQuery {
            storage_root_id: None,
            status: None,
            after_id: None,
            limit: 0,
        })
        .await
        .unwrap_err();
    assert!(matches!(invalid_limit, VoomError::Config(_)));
    let over_limit = repo
        .list(ScanSessionListQuery {
            storage_root_id: None,
            status: None,
            after_id: None,
            limit: 101,
        })
        .await
        .unwrap_err();
    assert!(matches!(over_limit, VoomError::Config(_)));
    let cancelled = repo
        .list(ScanSessionListQuery {
            storage_root_id: Some(root),
            status: Some(ScanSessionStatus::Cancelled),
            after_id: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert_eq!(cancelled.items.len(), 1);
    assert_eq!(cancelled.items[0].id, first.id);
    let other_root_page = repo
        .list(ScanSessionListQuery {
            storage_root_id: Some(other_root),
            status: None,
            after_id: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert_eq!(other_root_page.items.len(), 1);
    assert_eq!(other_root_page.items[0].id, other.id);
}

#[tokio::test]
async fn reconciliation_pages_in_location_order_with_an_exclusive_cursor() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let first = seed_rooted_location(&pool, root, "first.mkv").await;
    let second = seed_rooted_location(&pool, root, "second.mkv").await;
    let third = seed_rooted_location(&pool, root, "third.mkv").await;
    let session = seed_succeeded_session(&pool, root, Some(third), 3, "1970-01-01T00:02:00Z").await;
    for location in [third, first, second] {
        attribute_location(&pool, location, session, "1970-01-01T00:02:00Z").await;
    }
    sqlx::query("UPDATE library_roots SET last_scan_session_id = ? WHERE id = ?")
        .bind(i64::try_from(session.0).unwrap())
        .bind(i64::try_from(root.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let repo = SqliteScanSessionRepo::new(pool.clone());

    let mut transaction = pool.begin().await.unwrap();
    let transactional_page = repo
        .reconciliation_page_in_tx(
            &mut transaction,
            ScanReconciliationQuery {
                scan_session_id: session,
                after_id: None,
                limit: 2,
            },
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(transactional_page.items.len(), 2);

    let latest = repo.latest_succeeded_for_root(root).await.unwrap().unwrap();
    assert_eq!(latest.id, session);
    let first_page = repo
        .reconciliation_page(ScanReconciliationQuery {
            scan_session_id: session,
            after_id: None,
            limit: 2,
        })
        .await
        .unwrap();
    assert_eq!(
        first_page
            .items
            .iter()
            .map(|item| item.file_location_id.0)
            .collect::<Vec<_>>(),
        vec![
            u64::try_from(first).unwrap(),
            u64::try_from(second).unwrap()
        ]
    );
    assert_eq!(
        first_page.next_after_id,
        Some(voom_core::FileLocationId(u64::try_from(second).unwrap()))
    );
    assert!(
        first_page
            .items
            .iter()
            .all(|item| (item.prior_epoch, item.retired_epoch) == (0, 1))
    );
    let second_page = repo
        .reconciliation_page(ScanReconciliationQuery {
            scan_session_id: session,
            after_id: first_page.next_after_id,
            limit: 2,
        })
        .await
        .unwrap();
    assert_eq!(second_page.items.len(), 1);
    assert_eq!(
        second_page.items[0].file_location_id.0,
        u64::try_from(third).unwrap()
    );
    assert_eq!(second_page.next_after_id, None);
}

#[tokio::test]
async fn reconciliation_rejects_off_page_nanosecond_timestamp_corruption() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let mut locations = Vec::new();
    for locator in ["first.mkv", "second.mkv", "third.mkv", "fourth.mkv"] {
        locations.push(seed_rooted_location(&pool, root, locator).await);
    }
    let session = seed_succeeded_session(
        &pool,
        root,
        locations.last().copied(),
        u64::try_from(locations.len()).unwrap(),
        "1970-01-01T00:02:00Z",
    )
    .await;
    for location in &locations {
        attribute_location(&pool, *location, session, "1970-01-01T00:02:00Z").await;
    }
    attribute_location(
        &pool,
        locations[3],
        session,
        "1970-01-01T00:02:00.000000001Z",
    )
    .await;

    let repo = SqliteScanSessionRepo::new(pool);
    let error = repo
        .reconciliation_page(ScanReconciliationQuery {
            scan_session_id: session,
            after_id: None,
            limit: 1,
        })
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
}

#[tokio::test]
async fn reconciliation_rejects_limits_outside_one_through_one_hundred() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteScanSessionRepo::new(pool);
    for limit in [0, 101] {
        let error = repo
            .reconciliation_page(ScanReconciliationQuery {
                scan_session_id: ScanSessionId(1),
                after_id: None,
                limit,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, VoomError::Config(_)));
    }
}

#[tokio::test]
async fn reconciliation_page_sql_is_indexed_bounded_and_set_based() {
    let (pool, _tmp) = fresh_pool().await;
    assert!(super::RECONCILIATION_PAGE_AFTER_SQL.contains("l.id > ?"));
    assert!(super::RECONCILIATION_PAGE_AFTER_SQL.contains("LIMIT ?"));
    assert!(super::RECONCILIATION_INVALID_SQL.contains("EXISTS"));
    assert!(!super::RECONCILIATION_INVALID_SQL.contains("SELECT provider_relative_locator FROM"));

    let explain_sql = format!(
        "EXPLAIN QUERY PLAN {}",
        super::RECONCILIATION_PAGE_AFTER_SQL
    );
    let rows = sqlx::query(&explain_sql)
        .bind(1_i64)
        .bind(1_i64)
        .bind(2_i64)
        .fetch_all(&pool)
        .await
        .unwrap();
    let details = rows
        .iter()
        .map(|row| sqlx::Row::get::<String, _>(row, "detail"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        details.contains("file_locations_by_retired_scan_session"),
        "query plan did not use reconciliation index: {details}"
    );
}

#[tokio::test]
async fn lifecycle_and_batch_mutations_remain_owned_by_the_callers_transaction() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    seed_incarnation(&pool, "55555555555555555555555555555555").await;
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        "55555555555555555555555555555555".parse().unwrap(),
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    repo.accepted_batch_in_tx(
        &mut tx,
        batch(session.id, 0, 'a', vec![observation("rollback.mkv")]),
    )
    .await
    .unwrap();
    repo.terminalize_in_tx(
        &mut tx,
        session.id,
        ScanSessionStatus::Failed,
        voom_core::ScanTerminalReason::new("rollback proof").unwrap(),
        T0 + time::Duration::minutes(1),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    assert!(repo.get(session.id).await.unwrap().is_none());
    let batches: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_observation_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(batches, 0);
}

#[tokio::test]
async fn completion_compare_and_set_rejects_non_running_session_without_reconciliation() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let location = seed_rooted_location(&pool, root, "non-running.mkv").await;
    let incarnation = "55555555555555555555555555555555";
    seed_incarnation(&pool, incarnation).await;
    let incarnation_id = incarnation.parse().unwrap();
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let requested = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        requested.id,
        incarnation_id,
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE scan_sessions SET status = 'succeeded', terminal_at = ? WHERE id = ?")
        .bind("1970-01-01T00:01:00Z")
        .bind(i64::try_from(requested.id.0).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();

    let error = repo
        .complete_in_tx(
            &mut tx,
            CompleteScanSessionInput {
                scan_session_id: requested.id,
                expected_storage_root_id: root,
                expected_root_epoch: 1,
                expected_owner_node_id: NodeId(9_000_001),
                expected_owner_incarnation_id: incarnation_id,
                last_sequence: None,
                observation_count: 0,
                completed_at: T0 + time::Duration::minutes(2),
            },
        )
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    tx.rollback().await.unwrap();
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(location)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_none());
}

#[derive(Clone, Copy)]
enum CompletionLedgerCorruption {
    ResidualRowsWithResetCounters,
    BatchAndSessionCountsExceedActual,
    InvalidObservationLocator,
}

async fn corrupt_completion_ledger(
    pool: &sqlx::SqlitePool,
    session: ScanSessionId,
    case: CompletionLedgerCorruption,
) {
    let session_id = i64::try_from(session.0).unwrap();
    match case {
        CompletionLedgerCorruption::ResidualRowsWithResetCounters => {
            sqlx::query(
                "UPDATE scan_sessions SET next_sequence = 0, batch_count = 0, \
                 observation_count = 0 WHERE id = ?",
            )
            .bind(session_id)
            .execute(pool)
            .await
            .unwrap();
        }
        CompletionLedgerCorruption::BatchAndSessionCountsExceedActual => {
            remove_batch_update_guard(pool).await;
            sqlx::query(
                "UPDATE scan_observation_batches SET observation_count = 2, \
                 cumulative_observation_count = 2 WHERE scan_session_id = ?",
            )
            .bind(session_id)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query("UPDATE scan_sessions SET observation_count = 2 WHERE id = ?")
                .bind(session_id)
                .execute(pool)
                .await
                .unwrap();
        }
        CompletionLedgerCorruption::InvalidObservationLocator => {
            with_check_constraints_disabled(pool, |connection| {
                Box::pin(async move {
                    sqlx::query(
                        "UPDATE scan_observations SET provider_relative_locator = '/corrupt' \
                         WHERE scan_session_id = ?",
                    )
                    .bind(session_id)
                    .execute(connection)
                    .await
                })
            })
            .await
            .unwrap();
        }
    }
}

fn corrupted_completion_watermark(case: CompletionLedgerCorruption) -> (Option<u64>, u64) {
    match case {
        CompletionLedgerCorruption::ResidualRowsWithResetCounters => (None, 0),
        CompletionLedgerCorruption::BatchAndSessionCountsExceedActual => (Some(0), 2),
        CompletionLedgerCorruption::InvalidObservationLocator => (Some(0), 1),
    }
}

async fn assert_completion_rejects_ledger_corruption(case: CompletionLedgerCorruption) {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let observed = seed_rooted_location(&pool, root, "observed.mkv").await;
    let absent = seed_rooted_location(&pool, root, "absent.mkv").await;
    let incarnation = "77777777777777777777777777777777";
    seed_incarnation(&pool, incarnation).await;
    let incarnation_id = incarnation.parse().unwrap();
    let repo = SqliteScanSessionRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let session = repo
        .insert_requested_in_tx(&mut tx, new_session(root))
        .await
        .unwrap();
    repo.start_in_tx(
        &mut tx,
        session.id,
        incarnation_id,
        T0 + time::Duration::minutes(5),
        T0,
    )
    .await
    .unwrap();
    repo.accepted_batch_in_tx(
        &mut tx,
        batch(session.id, 0, 'a', vec![observation("observed.mkv")]),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    corrupt_completion_ledger(&pool, session.id, case).await;

    let (last_sequence, observation_count) = corrupted_completion_watermark(case);
    let mut tx = pool.begin().await.unwrap();
    let error = repo
        .complete_in_tx(
            &mut tx,
            CompleteScanSessionInput {
                scan_session_id: session.id,
                expected_storage_root_id: root,
                expected_root_epoch: 1,
                expected_owner_node_id: NodeId(9_000_001),
                expected_owner_incarnation_id: incarnation_id,
                last_sequence,
                observation_count,
                completed_at: T0 + time::Duration::minutes(2),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    let state: (String, Option<String>, i64) = sqlx::query_as(
        "SELECT status, terminal_at, retired_location_count FROM scan_sessions WHERE id = ?",
    )
    .bind(i64::try_from(session.id.0).unwrap())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(state, ("running".to_owned(), None, 0));
    let retired: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM file_locations WHERE id IN (?, ?) AND retired_at IS NOT NULL",
    )
    .bind(observed)
    .bind(absent)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(retired, 0);
    let pointer: Option<i64> =
        sqlx::query_scalar("SELECT last_scan_session_id FROM library_roots WHERE id = ?")
            .bind(i64::try_from(root.0).unwrap())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(pointer, None);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn completion_rejects_incoherent_or_invalid_persisted_traversal_evidence() {
    for case in [
        CompletionLedgerCorruption::ResidualRowsWithResetCounters,
        CompletionLedgerCorruption::BatchAndSessionCountsExceedActual,
        CompletionLedgerCorruption::InvalidObservationLocator,
    ] {
        assert_completion_rejects_ledger_corruption(case).await;
    }
}

#[derive(Clone, Copy)]
enum RootPointerCorruption {
    WrongRoot,
    NonSuccess,
    OlderSuccess,
}

async fn assert_root_pointer_corruption(case: RootPointerCorruption) {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let other_root = seed_second_root(&pool).await;
    let (pointed, historical) = match case {
        RootPointerCorruption::WrongRoot => {
            let session =
                seed_succeeded_session(&pool, other_root, None, 0, "1970-01-01T00:02:00Z").await;
            (session, session)
        }
        RootPointerCorruption::NonSuccess => {
            seed_incarnation(&pool, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").await;
            let id = sqlx::query(
                "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
                 idle_timeout_seconds, progress_deadline_at, requested_at, terminal_at, \
                 terminal_reason) VALUES (?, 1, 9000001, 'failed', 300, \
                 '1970-01-01T00:05:00Z', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:02:00Z', 'failed scan')",
            )
            .bind(i64::try_from(root.0).unwrap())
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
            let session = ScanSessionId(u64::try_from(id).unwrap());
            (session, session)
        }
        RootPointerCorruption::OlderSuccess => {
            let older = seed_succeeded_session(&pool, root, None, 0, "1970-01-01T00:02:00Z").await;
            let newer = seed_succeeded_session(&pool, root, None, 0, "1970-01-01T00:03:00Z").await;
            (older, newer)
        }
    };
    sqlx::query("UPDATE library_roots SET last_scan_session_id = ? WHERE id = ?")
        .bind(i64::try_from(pointed.0).unwrap())
        .bind(i64::try_from(root.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let repo = SqliteScanSessionRepo::new(pool);
    assert!(matches!(
        repo.latest_succeeded_for_root(root).await.unwrap_err(),
        VoomError::Database { .. }
    ));
    assert_eq!(repo.get(historical).await.unwrap().unwrap().id, historical);
}

#[tokio::test]
async fn root_pointer_semantic_corruption_is_database_only_when_followed() {
    for case in [
        RootPointerCorruption::WrongRoot,
        RootPointerCorruption::NonSuccess,
        RootPointerCorruption::OlderSuccess,
    ] {
        assert_root_pointer_corruption(case).await;
    }
}

#[derive(Clone, Copy)]
enum LocationPointerCorruption {
    WrongRoot,
    NonSuccess,
    RetirementTime,
    AboveHighWatermark,
    ObservedLocator,
    RetiredCount,
}

async fn add_observed_locator(pool: &sqlx::SqlitePool, session: ScanSessionId) {
    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = 1, batch_count = 1, observation_count = 1 \
         WHERE id = ?",
    )
    .bind(i64::try_from(session.0).unwrap())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
         request_hash, observation_count, accepted_at, cumulative_observation_count) \
         VALUES (?, 0, NULL, ?, 1, '1970-01-01T00:01:00Z', 1)",
    )
    .bind(i64::try_from(session.0).unwrap())
    .bind("b".repeat(64))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scan_observations (scan_session_id, batch_sequence, ordinal, \
         provider_relative_locator, provider_object_identity, size_bytes, modified_at, \
         stability_started_at, stability_confirmed_at) VALUES \
         (?, 0, 0, 'low.mkv', 'observed', 1, '1970-01-01T00:00:00Z', \
         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(i64::try_from(session.0).unwrap())
    .execute(pool)
    .await
    .unwrap();
}

async fn apply_location_pointer_detail(
    pool: &sqlx::SqlitePool,
    session: ScanSessionId,
    case: LocationPointerCorruption,
) {
    match case {
        LocationPointerCorruption::ObservedLocator => add_observed_locator(pool, session).await,
        LocationPointerCorruption::RetiredCount => {
            sqlx::query("UPDATE scan_sessions SET retired_location_count = 2 WHERE id = ?")
                .bind(i64::try_from(session.0).unwrap())
                .execute(pool)
                .await
                .unwrap();
        }
        LocationPointerCorruption::WrongRoot
        | LocationPointerCorruption::NonSuccess
        | LocationPointerCorruption::RetirementTime
        | LocationPointerCorruption::AboveHighWatermark => {}
    }
}

async fn assert_location_pointer_corruption(case: LocationPointerCorruption) {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let other_root = seed_second_root(&pool).await;
    let low = seed_rooted_location(&pool, root, "low.mkv").await;
    let (session, attributed) = match case {
        LocationPointerCorruption::WrongRoot => {
            let other = seed_rooted_location(&pool, other_root, "other-pointer.mkv").await;
            (
                seed_succeeded_session(&pool, other_root, Some(other), 1, "1970-01-01T00:02:00Z")
                    .await,
                low,
            )
        }
        LocationPointerCorruption::NonSuccess => {
            seed_incarnation(&pool, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").await;
            let id = sqlx::query(
                "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, \
                 owner_incarnation_id, status, idle_timeout_seconds, progress_deadline_at, \
                 location_high_watermark_id, requested_at, started_at, terminal_at, \
                 terminal_reason) VALUES (?, 1, 9000001, ?, 'failed', 300, \
                 '1970-01-01T00:05:00Z', ?, '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:01:00Z', '1970-01-01T00:02:00Z', 'failed scan')",
            )
            .bind(i64::try_from(root.0).unwrap())
            .bind("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .bind(low)
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
            (ScanSessionId(u64::try_from(id).unwrap()), low)
        }
        LocationPointerCorruption::AboveHighWatermark => {
            let session =
                seed_succeeded_session(&pool, root, Some(low), 1, "1970-01-01T00:02:00Z").await;
            let high = seed_rooted_location(&pool, root, "high.mkv").await;
            (session, high)
        }
        _ => (
            seed_succeeded_session(&pool, root, Some(low), 1, "1970-01-01T00:02:00Z").await,
            low,
        ),
    };
    let retired_at = if matches!(case, LocationPointerCorruption::RetirementTime) {
        "1970-01-01T00:03:00Z"
    } else {
        "1970-01-01T00:02:00Z"
    };
    attribute_location(&pool, attributed, session, retired_at).await;
    apply_location_pointer_detail(&pool, session, case).await;
    let repo = SqliteScanSessionRepo::new(pool);
    assert!(matches!(
        repo.reconciliation_page(ScanReconciliationQuery {
            scan_session_id: session,
            after_id: None,
            limit: 100,
        })
        .await
        .unwrap_err(),
        VoomError::Database { .. }
    ));
    let inspected = repo.get(session).await;
    if matches!(
        case,
        LocationPointerCorruption::NonSuccess | LocationPointerCorruption::RetiredCount
    ) {
        assert!(matches!(inspected.unwrap_err(), VoomError::Database { .. }));
    } else {
        assert_eq!(inspected.unwrap().unwrap().id, session);
    }
}

#[tokio::test]
async fn location_pointer_semantic_corruption_is_database_only_when_followed() {
    for case in [
        LocationPointerCorruption::WrongRoot,
        LocationPointerCorruption::NonSuccess,
        LocationPointerCorruption::RetirementTime,
        LocationPointerCorruption::AboveHighWatermark,
        LocationPointerCorruption::ObservedLocator,
        LocationPointerCorruption::RetiredCount,
    ] {
        assert_location_pointer_corruption(case).await;
    }
}

#[tokio::test]
async fn session_get_and_list_reject_a_mismatched_attributed_retirement_count() {
    let (pool, _tmp) = fresh_pool().await;
    let root = seed_test_storage_root(&pool).await.unwrap();
    let location = seed_rooted_location(&pool, root, "retired-count.mkv").await;
    let session =
        seed_succeeded_session(&pool, root, Some(location), 1, "1970-01-01T00:02:00Z").await;
    attribute_location(&pool, location, session, "1970-01-01T00:02:00Z").await;
    sqlx::query("UPDATE scan_sessions SET retired_location_count = 2 WHERE id = ?")
        .bind(i64::try_from(session.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let repo = SqliteScanSessionRepo::new(pool);
    let get_error = repo.get(session).await.unwrap_err();
    let list_error = repo
        .list(ScanSessionListQuery {
            storage_root_id: Some(root),
            status: None,
            after_id: None,
            limit: 100,
        })
        .await
        .unwrap_err();
    for error in [get_error, list_error] {
        assert!(matches!(error, VoomError::Database { .. }));
        assert!(
            error
                .to_string()
                .contains("does not match 1 attributed locations")
        );
    }
}
