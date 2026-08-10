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
async fn scan_observation_row_decoder_rejects_invalid_persisted_values() {
    let (pool, _tmp) = fresh_pool().await;
    let id = insert_requested_session(&pool).await;
    let row = sqlx::query(
        "SELECT 'bad//locator' AS provider_relative_locator, 'object' AS provider_object_identity, \
         -1 AS size_bytes, 'not-a-time' AS modified_at, \
         '1970-01-01T00:00:00Z' AS stability_started_at, \
         '1970-01-01T00:00:00Z' AS stability_confirmed_at, ? AS scan_session_id",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let error = super::decode_observation_row(&row).unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
}
