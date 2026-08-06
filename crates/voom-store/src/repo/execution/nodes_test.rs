use time::{Duration, OffsetDateTime};
use voom_core::{
    ErrorCode, NodeId, NodeIncarnationEndReason, NodeIncarnationId, NodeIncarnationStatus,
};

use super::{NewNode, Node, NodeKind, NodeStatus, SqliteNodeRepo};
use crate::repo::execution::node_incarnations::{NewNodeIncarnation, SqliteNodeIncarnationRepo};
use crate::test_support::T0;

#[tokio::test]
async fn nodes_register_get_and_list_round_trip_without_exposing_plaintext_token() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let node = repo
        .register_in_tx(
            &mut tx,
            NewNode {
                name: "synthetic-a".to_owned(),
                kind: NodeKind::Synthetic,
                registered_at: T0,
                heartbeat_ttl_seconds: 60,
                auth_token_hash: "voom-node-token-sha256-v1:abc".to_owned(),
                auth_token_hint: "abc".to_owned(),
                metadata: serde_json::json!({"zone":"test"}),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(node.status, NodeStatus::Registered);
    assert_eq!(node.last_seen_at, T0);
    assert_eq!(node.epoch, 0);
    assert_eq!(node.active_incarnation_id, None);
    let got = repo.get(node.id).await.unwrap().unwrap();
    assert_eq!(got.auth_token_hint, "abc");
    assert_eq!(got.metadata, serde_json::json!({"zone":"test"}));
    let listed = repo.list(Some(NodeStatus::Registered), 10).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, node.id);
}

#[tokio::test]
async fn node_reads_reject_null_pointer_with_active_incarnation() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let node = seed_node(&pool, &repo, "corrupt-null", NodeStatus::Active, T0, 60).await;
    insert_incarnation(&pool, node.id, "0123456789abcdef0123456789abcdef", "active").await;

    let error = repo.get(node.id).await.unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert!(error.to_string().contains("active incarnation pointer"));
}

#[tokio::test]
async fn node_reads_reject_cross_node_active_pointer() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let owner = seed_node(&pool, &repo, "owner", NodeStatus::Active, T0, 60).await;
    let pointed = seed_node(&pool, &repo, "pointed", NodeStatus::Active, T0, 60).await;
    let incarnation_id = "0123456789abcdef0123456789abcdef";
    insert_incarnation(&pool, owner.id, incarnation_id, "active").await;
    sqlx::query("UPDATE nodes SET active_incarnation_id = ? WHERE id = ?")
        .bind(incarnation_id)
        .bind(i64::try_from(pointed.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let error = repo.get(pointed.id).await.unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert!(error.to_string().contains("owned by node"));
    let mut tx = pool.begin().await.unwrap();
    let auth_error = repo
        .auth_record_in_tx(&mut tx, pointed.id)
        .await
        .unwrap_err();
    assert_eq!(auth_error.error_code(), ErrorCode::DbUnreachable);
}

#[tokio::test]
async fn node_reads_reject_malformed_active_incarnation_id() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let node = seed_node(&pool, &repo, "malformed", NodeStatus::Active, T0, 60).await;
    insert_incarnation(&pool, node.id, "malformed", "active").await;
    sqlx::query("UPDATE nodes SET active_incarnation_id = 'malformed' WHERE id = ?")
        .bind(i64::try_from(node.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let error = repo.get(node.id).await.unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert!(error.to_string().contains("active incarnation id"));
}

#[tokio::test]
async fn node_reads_reject_terminal_active_pointer_before_lifecycle_classification() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let node = seed_node(&pool, &repo, "terminal", NodeStatus::Retired, T0, 60).await;
    let incarnation_id = "0123456789abcdef0123456789abcdef";
    insert_incarnation(&pool, node.id, incarnation_id, "active").await;
    sqlx::query("UPDATE nodes SET active_incarnation_id = ? WHERE id = ?")
        .bind(incarnation_id)
        .bind(i64::try_from(node.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE node_incarnations SET status = 'retired', ended_at = ?, \
         end_reason = 'graceful_shutdown' WHERE incarnation_id = ?",
    )
    .bind(iso8601_for_test(T0))
    .bind(incarnation_id)
    .execute(&pool)
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let error = repo
        .retire_in_tx(&mut tx, node.id, node.epoch, T0)
        .await
        .unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert!(error.to_string().contains("not active"));
}

#[tokio::test]
async fn node_incarnation_pointer_transitions_bridge_atomic_replacement_and_clear() {
    let (pool, _tmp) = fresh_pool().await;
    let nodes = SqliteNodeRepo::new(pool.clone());
    let incarnations = SqliteNodeIncarnationRepo::new(pool.clone());
    let node = seed_node(&pool, &nodes, "transition", NodeStatus::Registered, T0, 60).await;
    let incarnation_id: NodeIncarnationId = "0123456789abcdef0123456789abcdef".parse().unwrap();

    let mut tx = pool.begin().await.unwrap();
    incarnations
        .insert_in_tx(
            &mut tx,
            NewNodeIncarnation {
                id: incarnation_id,
                node_id: node.id,
                started_at: T0,
            },
        )
        .await
        .unwrap();
    let active = nodes
        .activate_incarnation_in_tx(&mut tx, node.id, None, incarnation_id, T0)
        .await
        .unwrap();
    assert_eq!(active.active_incarnation_id, Some(incarnation_id));
    assert_eq!(active.status, NodeStatus::Active);

    incarnations
        .end_in_tx(
            &mut tx,
            incarnation_id,
            NodeIncarnationStatus::Retired,
            NodeIncarnationEndReason::GracefulShutdown,
            T0,
        )
        .await
        .unwrap();
    let cleared = nodes
        .clear_incarnation_in_tx(&mut tx, node.id, incarnation_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(cleared.active_incarnation_id, None);
    assert_eq!(cleared.status, NodeStatus::Registered);
    assert_eq!(cleared.epoch, node.epoch + 2);
}

#[tokio::test]
async fn nodes_debug_redacts_auth_token_hash_but_keeps_hint() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let hash = "voom-node-token-sha256-v1:debug-secret";
    let expected_hint = "hint1234";
    let input = NewNode {
        name: "synthetic-a".to_owned(),
        kind: NodeKind::Synthetic,
        registered_at: T0,
        heartbeat_ttl_seconds: 60,
        auth_token_hash: hash.to_owned(),
        auth_token_hint: expected_hint.to_owned(),
        metadata: serde_json::json!({"zone":"test"}),
    };

    let input_debug = format!("{input:?}");
    assert!(!input_debug.contains(hash));
    assert!(!input_debug.contains("voom-node-token-sha256-v1:"));
    assert!(input_debug.contains(expected_hint));

    let mut tx = pool.begin().await.unwrap();
    let node = repo.register_in_tx(&mut tx, input).await.unwrap();
    let auth = repo
        .auth_record_in_tx(&mut tx, node.id)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();

    let auth_debug = format!("{auth:?}");
    assert!(!auth_debug.contains(hash));
    assert!(!auth_debug.contains("voom-node-token-sha256-v1:"));
}

#[tokio::test]
async fn nodes_heartbeat_moves_registered_or_stale_node_to_active_and_increments_epoch() {
    for status in [NodeStatus::Registered, NodeStatus::Stale] {
        let (_tmp, pool, repo, node) = seeded_node(status, T0).await;
        let mut tx = pool.begin().await.unwrap();
        let updated = repo
            .heartbeat_in_tx(&mut tx, node.id, T0 + Duration::seconds(10))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(updated.status, NodeStatus::Active);
        assert_eq!(updated.last_seen_at, T0 + Duration::seconds(10));
        assert_eq!(updated.epoch, node.epoch + 1);
    }
}

#[tokio::test]
async fn nodes_heartbeat_unknown_node_returns_not_found() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .heartbeat_in_tx(&mut tx, NodeId(9_999), T0 + Duration::seconds(10))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn nodes_mark_stale_changes_only_freshly_expired_non_retired_rows() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let expired = seed_node(&pool, &repo, "expired", NodeStatus::Active, T0, 5).await;
    let fresh = seed_node(
        &pool,
        &repo,
        "fresh",
        NodeStatus::Active,
        T0 + Duration::seconds(20),
        60,
    )
    .await;
    let stale = seed_node(&pool, &repo, "already-stale", NodeStatus::Stale, T0, 5).await;

    let mut tx = pool.begin().await.unwrap();
    let changed = repo
        .mark_stale_in_tx(&mut tx, T0 + Duration::seconds(10))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        changed.iter().map(|n| n.id).collect::<Vec<_>>(),
        vec![expired.id]
    );
    assert_eq!(
        repo.get(fresh.id).await.unwrap().unwrap().status,
        NodeStatus::Active
    );
    assert_eq!(
        repo.get(stale.id).await.unwrap().unwrap().epoch,
        stale.epoch
    );
}

#[tokio::test]
async fn nodes_mark_stale_skips_obsolete_candidate_snapshot() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let node = seed_node(&pool, &repo, "expired", NodeStatus::Active, T0, 5).await;
    let stale_snapshot = repo.get(node.id).await.unwrap().unwrap();

    let mut tx = pool.begin().await.unwrap();
    let heartbeat = repo
        .heartbeat_in_tx(&mut tx, node.id, T0 + Duration::seconds(20))
        .await
        .unwrap();
    let changed =
        super::mark_stale_candidate_in_tx(&mut tx, &stale_snapshot, T0 + Duration::seconds(10))
            .await
            .unwrap();
    tx.commit().await.unwrap();

    assert!(changed.is_none());
    let stored = repo.get(node.id).await.unwrap().unwrap();
    assert_eq!(stored.status, NodeStatus::Active);
    assert_eq!(stored.last_seen_at, heartbeat.last_seen_at);
    assert_eq!(stored.epoch, heartbeat.epoch);
}

#[tokio::test]
async fn nodes_retire_unknown_node_returns_not_found() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .retire_in_tx(&mut tx, NodeId(9_999), 0, T0 + Duration::seconds(30))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn nodes_retire_is_terminal_and_epoch_guarded() {
    let (_tmp, pool, repo, node) = seeded_node(NodeStatus::Active, T0).await;

    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .retire_in_tx(&mut tx, node.id, node.epoch + 1, T0 + Duration::seconds(29))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    tx.rollback().await.unwrap();
    assert_eq!(
        repo.get(node.id).await.unwrap().unwrap().status,
        NodeStatus::Active
    );

    let mut tx = pool.begin().await.unwrap();
    let retired = repo
        .retire_in_tx(&mut tx, node.id, node.epoch, T0 + Duration::seconds(30))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(retired.status, NodeStatus::Retired);
    assert_eq!(retired.retired_at, Some(T0 + Duration::seconds(30)));
    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .retire_in_tx(&mut tx, node.id, retired.epoch, T0 + Duration::seconds(31))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
}

async fn fresh_pool() -> (sqlx::SqlitePool, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (pool, tmp)
}

async fn seed_node(
    pool: &sqlx::SqlitePool,
    repo: &SqliteNodeRepo,
    name: &str,
    status: NodeStatus,
    last_seen_at: OffsetDateTime,
    ttl_seconds: u32,
) -> Node {
    let mut tx = pool.begin().await.unwrap();
    let mut node = repo
        .register_in_tx(
            &mut tx,
            NewNode {
                name: name.to_owned(),
                kind: NodeKind::Synthetic,
                registered_at: T0,
                heartbeat_ttl_seconds: ttl_seconds,
                auth_token_hash: format!("voom-node-token-sha256-v1:{name}"),
                auth_token_hint: name.to_owned(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    if status != NodeStatus::Registered || last_seen_at != T0 {
        let retired_at = (status == NodeStatus::Retired).then_some(last_seen_at);
        sqlx::query(
            "UPDATE nodes SET status = ?, last_seen_at = ?, retired_at = ?, epoch = 1 WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(iso8601_for_test(last_seen_at))
        .bind(retired_at.map(iso8601_for_test))
        .bind(i64::try_from(node.id.0).unwrap())
        .execute(pool)
        .await
        .unwrap();
        node = repo.get(node.id).await.unwrap().unwrap();
    }
    node
}

async fn seeded_node(
    status: NodeStatus,
    last_seen_at: OffsetDateTime,
) -> (
    voom_test_support::TempDatabase,
    sqlx::SqlitePool,
    SqliteNodeRepo,
    Node,
) {
    let (pool, tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let node = seed_node(&pool, &repo, "seeded", status, last_seen_at, 60).await;
    (tmp, pool, repo, node)
}

fn iso8601_for_test(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap()
}

async fn insert_incarnation(
    pool: &sqlx::SqlitePool,
    node_id: NodeId,
    incarnation_id: &str,
    status: &str,
) {
    let (ended_at, reason) = if status == "active" {
        (None, None)
    } else {
        (Some(iso8601_for_test(T0)), Some("graceful_shutdown"))
    };
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(incarnation_id)
    .bind(i64::try_from(node_id.0).unwrap())
    .bind(status)
    .bind(iso8601_for_test(T0))
    .bind(iso8601_for_test(T0))
    .bind(ended_at)
    .bind(reason)
    .execute(pool)
    .await
    .unwrap();
}
