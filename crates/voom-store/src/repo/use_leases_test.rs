use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use voom_core::{FileAssetId, UseLeaseId};

use super::*;
use crate::test_support::{T0, fresh_initialized_pool_at};

/// Spin up a fresh pool with migration 0004 applied, plus a single
/// `file_assets` row so tests have a live scope to attach leases to.
async fn pool_with_asset() -> (SqlitePool, NamedTempFile, FileAssetId) {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(
            T0.format(&time::format_description::well_known::Iso8601::DEFAULT)
                .unwrap(),
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
    (pool, tmp, FileAssetId(u64::try_from(asset_id).unwrap()))
}

#[tokio::test]
async fn get_returns_none_for_unknown_id() {
    let (pool, _tmp, _asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    assert!(repo.get(UseLeaseId(99999)).await.unwrap().is_none());
}

#[tokio::test]
async fn list_for_scope_returns_empty_on_clean_db() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let listed = repo.list_for_scope(LeaseScope::Asset(asset)).await.unwrap();
    assert!(listed.is_empty());
}
