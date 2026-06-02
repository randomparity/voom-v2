use time::{Duration, OffsetDateTime};
use voom_core::NodeId;

use super::*;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

async fn repo() -> (SqliteSchedulerNodeLimitRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    seed_node(&pool).await;
    (SqliteSchedulerNodeLimitRepo::new(pool), tmp)
}

async fn seed_node(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO nodes \
         (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (3, 'node-3', 'remote', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'token-hash', 'hint', '{}')",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn defaults_to_one_and_upserts() {
    let (repo, _tmp) = repo().await;
    let mut tx = repo.pool.begin().await.unwrap();
    assert_eq!(repo.node_limit_in_tx(&mut tx, NodeId(3)).await.unwrap(), 1);
    tx.commit().await.unwrap();

    let first = repo.set_node_limit(NodeId(3), 2, T0).await.unwrap();
    let second = repo
        .set_node_limit(NodeId(3), 4, T0 + Duration::seconds(5))
        .await
        .unwrap();

    assert_eq!(first.max_parallel_leases, 2);
    assert_eq!(second.max_parallel_leases, 4);
    assert_eq!(second.created_at, first.created_at);
    assert!(second.updated_at > first.updated_at);

    let mut tx = repo.pool.begin().await.unwrap();
    assert_eq!(repo.node_limit_in_tx(&mut tx, NodeId(3)).await.unwrap(), 4);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn rejects_zero_and_unknown_node() {
    let (repo, _tmp) = repo().await;

    let zero = repo.set_node_limit(NodeId(3), 0, T0).await.unwrap_err();
    assert_eq!(zero.error_code(), voom_core::ErrorCode::ConfigInvalid);

    let missing_node = repo.set_node_limit(NodeId(404), 1, T0).await.unwrap_err();
    assert_eq!(
        missing_node.error_code(),
        voom_core::ErrorCode::DbUnreachable
    );
}
