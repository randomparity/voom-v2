use voom_core::{ScanSessionId, VoomError};

use super::SqliteScanSessionRepo;
use crate::test_support::{fresh_initialized_pool_at, with_check_constraints_disabled};

async fn fresh_pool() -> (sqlx::SqlitePool, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
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
async fn scan_observation_row_decoder_rejects_isolated_and_combined_corruption() {
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
