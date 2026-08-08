use std::str::FromStr;

use time::Duration;
use voom_core::{ErrorCode, NodeIncarnationEndReason, NodeIncarnationId, NodeIncarnationStatus};

use super::{NewNodeIncarnation, SqliteNodeIncarnationRepo};
use crate::repo::execution::nodes::{NewNode, NodeKind, SqliteNodeRepo};
use crate::test_support::{T0, with_check_constraints_disabled};

const FIRST_ID: &str = "0123456789abcdef0123456789abcdef";
const SECOND_ID: &str = "1123456789abcdef0123456789abcdef";

#[tokio::test]
async fn incarnation_lifecycle_round_trips_in_newest_first_order() {
    let (pool, _tmp) = fresh_pool().await;
    let node = seed_node(&pool, "node-a").await;
    let repo = SqliteNodeIncarnationRepo::new(pool.clone());
    let first_id = incarnation(FIRST_ID);
    let second_id = incarnation(SECOND_ID);

    let mut tx = pool.begin().await.unwrap();
    repo.insert_in_tx(
        &mut tx,
        NewNodeIncarnation {
            id: first_id,
            node_id: node.id,
            started_at: T0,
        },
    )
    .await
    .unwrap();
    repo.heartbeat_in_tx(&mut tx, first_id, T0 + Duration::seconds(1))
        .await
        .unwrap();
    repo.end_in_tx(
        &mut tx,
        first_id,
        NodeIncarnationStatus::Superseded,
        NodeIncarnationEndReason::Superseded,
        T0 + Duration::seconds(2),
    )
    .await
    .unwrap();
    repo.insert_in_tx(
        &mut tx,
        NewNodeIncarnation {
            id: second_id,
            node_id: node.id,
            started_at: T0 + Duration::seconds(3),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let history = repo.list_for_node(node.id, 10).await.unwrap();
    assert_eq!(
        history.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![second_id, first_id]
    );
    assert_eq!(history[0].status, NodeIncarnationStatus::Active);
    assert_eq!(history[1].last_seen_at, T0 + Duration::seconds(1));
    assert_eq!(
        history[1].end_reason,
        Some(NodeIncarnationEndReason::Superseded)
    );
}

#[tokio::test]
async fn schema_enforces_one_active_incarnation_and_exact_terminal_pairs() {
    let (pool, _tmp) = fresh_pool().await;
    let node = seed_node(&pool, "node-a").await;
    let repo = SqliteNodeIncarnationRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    repo.insert_in_tx(
        &mut tx,
        NewNodeIncarnation {
            id: incarnation(FIRST_ID),
            node_id: node.id,
            started_at: T0,
        },
    )
    .await
    .unwrap();
    let duplicate = repo
        .insert_in_tx(
            &mut tx,
            NewNodeIncarnation {
                id: incarnation(SECOND_ID),
                node_id: node.id,
                started_at: T0,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate.error_code(), ErrorCode::DbUnreachable);
    tx.rollback().await.unwrap();

    let invalid = sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
         VALUES (?, ?, 'retired', ?, ?, ?, 'superseded')",
    )
    .bind(FIRST_ID)
    .bind(i64::try_from(node.id.0).unwrap())
    .bind(timestamp(T0))
    .bind(timestamp(T0))
    .bind(timestamp(T0))
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(invalid.to_string().contains("CHECK constraint failed"));
}

#[tokio::test]
async fn history_rejects_malformed_ids_and_invalid_status_reason_pairs() {
    let (pool, _tmp) = fresh_pool().await;
    let node = seed_node(&pool, "node-a").await;
    let node_id = i64::try_from(node.id.0).unwrap();
    let repo = SqliteNodeIncarnationRepo::new(pool.clone());
    with_check_constraints_disabled(&pool, move |connection| {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO node_incarnations \
                 (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
                 VALUES ('not-an-incarnation', ?, 'active', ?, ?, NULL, NULL)",
            )
            .bind(node_id)
            .bind(timestamp(T0))
            .bind(timestamp(T0))
            .execute(&mut *connection)
            .await?;

            let malformed = repo.list_for_node(node.id, 10).await.unwrap_err();
            assert_eq!(malformed.error_code(), ErrorCode::DbUnreachable);
            assert!(malformed.to_string().contains("incarnation id"));

            sqlx::query("DELETE FROM node_incarnations")
                .execute(&mut *connection)
                .await?;
            sqlx::query(
                "INSERT INTO node_incarnations \
                 (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
                 VALUES (?, ?, 'retired', ?, ?, ?, 'superseded')",
            )
            .bind(FIRST_ID)
            .bind(node_id)
            .bind(timestamp(T0))
            .bind(timestamp(T0))
            .bind(timestamp(T0))
            .execute(&mut *connection)
            .await?;
            let invalid_pair = repo.list_for_node(node.id, 10).await.unwrap_err();
            assert_eq!(invalid_pair.error_code(), ErrorCode::DbUnreachable);
            assert!(invalid_pair.to_string().contains("status/end reason"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[test]
fn worker_count_conversion_is_checked() {
    let error = super::worker_count_from_i64(i64::MAX).unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
}

async fn fresh_pool() -> (sqlx::SqlitePool, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (pool, tmp)
}

async fn seed_node(pool: &sqlx::SqlitePool, name: &str) -> crate::repo::execution::nodes::Node {
    let repo = SqliteNodeRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let node = repo
        .register_in_tx(
            &mut tx,
            NewNode {
                name: name.to_owned(),
                kind: NodeKind::Synthetic,
                registered_at: T0,
                heartbeat_ttl_seconds: 60,
                auth_token_hash: format!("voom-node-token-sha256-v1:{name}"),
                auth_token_hint: name.to_owned(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    node
}

fn incarnation(value: &str) -> NodeIncarnationId {
    NodeIncarnationId::from_str(value).unwrap()
}

fn timestamp(value: time::OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap()
}
