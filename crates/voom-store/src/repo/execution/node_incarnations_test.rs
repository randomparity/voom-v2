use std::str::FromStr;

use sqlx::Row;
use time::{Duration, UtcOffset};
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

#[tokio::test]
async fn activation_count_is_inclusive_and_scoped_to_one_node() {
    let (pool, _tmp) = fresh_pool().await;
    let first = seed_node(&pool, "node-a").await;
    let second = seed_node(&pool, "node-b").await;
    let lower_bound = T0 + Duration::seconds(60);
    for (id, node_id, started_at) in [
        (FIRST_ID, first.id, lower_bound - Duration::nanoseconds(1)),
        (SECOND_ID, first.id, lower_bound),
        (
            "2123456789abcdef0123456789abcdef",
            first.id,
            lower_bound + Duration::seconds(30),
        ),
        (
            "3123456789abcdef0123456789abcdef",
            second.id,
            lower_bound + Duration::seconds(30),
        ),
    ] {
        sqlx::query(
            "INSERT INTO node_incarnations \
             (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
             VALUES (?, ?, 'superseded', ?, ?, ?, 'superseded')",
        )
        .bind(id)
        .bind(i64::try_from(node_id.0).unwrap())
        .bind(timestamp(started_at))
        .bind(timestamp(started_at))
        .bind(timestamp(started_at))
        .execute(&pool)
        .await
        .unwrap();
    }
    let repo = SqliteNodeIncarnationRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();

    let count = repo
        .count_started_at_or_after_in_tx(&mut tx, first.id, lower_bound, u32::MAX)
        .await
        .unwrap();

    assert_eq!(count, 2);
}

#[tokio::test]
async fn activation_count_rejects_malformed_qualifying_evidence() {
    let (pool, _tmp) = fresh_pool().await;
    let node = seed_node(&pool, "corrupt-activation-evidence").await;
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
         VALUES ('not-an-incarnation', ?, 'superseded', 'zzzz', 'zzzz', 'zzzz', 'superseded')",
    )
    .bind(i64::try_from(node.id.0).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let repo = SqliteNodeIncarnationRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();

    let error = repo
        .count_started_at_or_after_in_tx(&mut tx, node.id, T0, 5)
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert!(error.to_string().contains("activation evidence"));
}

#[tokio::test]
async fn activation_count_rejects_empty_persisted_incarnation_id() {
    let (pool, _tmp) = fresh_pool().await;
    let node = seed_node(&pool, "empty-activation-evidence").await;
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
         VALUES ('', ?, 'superseded', ?, ?, ?, 'superseded')",
    )
    .bind(i64::try_from(node.id.0).unwrap())
    .bind(timestamp(T0))
    .bind(timestamp(T0))
    .bind(timestamp(T0))
    .execute(&pool)
    .await
    .unwrap();
    let repo = SqliteNodeIncarnationRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();

    let error = repo
        .count_started_at_or_after_in_tx(&mut tx, node.id, T0, 5)
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert!(error.to_string().contains("activation evidence id"));
}

#[tokio::test]
async fn activation_count_compares_offset_timestamps_as_instants_and_honors_limit() {
    let (pool, _tmp) = fresh_pool().await;
    let node = seed_node(&pool, "offset-activation-evidence").await;
    let lower_bound = T0 + Duration::seconds(60);
    let negative_offset = UtcOffset::from_hms(-1, 0, 0).unwrap();
    for ordinal in 100..164 {
        let started_at = lower_bound - Duration::seconds(100);
        sqlx::query(
            "INSERT INTO node_incarnations \
             (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
             VALUES (?, ?, 'superseded', ?, ?, ?, 'superseded')",
        )
        .bind(format!("{ordinal:032x}"))
        .bind(i64::try_from(node.id.0).unwrap())
        .bind(timestamp(started_at))
        .bind(timestamp(started_at))
        .bind(timestamp(started_at))
        .execute(&pool)
        .await
        .unwrap();
    }
    for ordinal in 8..=13 {
        let started_at =
            (lower_bound + Duration::nanoseconds(i64::from(ordinal))).to_offset(negative_offset);
        sqlx::query(
            "INSERT INTO node_incarnations \
             (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
             VALUES (?, ?, 'superseded', ?, ?, ?, 'superseded')",
        )
        .bind(format!("{ordinal:032x}"))
        .bind(i64::try_from(node.id.0).unwrap())
        .bind(timestamp(started_at))
        .bind(timestamp(started_at))
        .bind(timestamp(started_at))
        .execute(&pool)
        .await
        .unwrap();
    }
    let repo = SqliteNodeIncarnationRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();

    let count = repo
        .count_started_at_or_after_in_tx(&mut tx, node.id, lower_bound, 5)
        .await
        .unwrap();

    assert_eq!(count, 5);
}

#[tokio::test]
async fn activation_evidence_pages_use_the_history_index_without_temp_sort() {
    let (pool, _tmp) = fresh_pool().await;
    let node = seed_node(&pool, "activation-plan-node").await;
    let mut details: Vec<String> = sqlx::query(
        "EXPLAIN QUERY PLAN \
         SELECT incarnation_id, started_at FROM node_incarnations \
         WHERE node_id = ? ORDER BY started_at DESC, incarnation_id DESC LIMIT 32",
    )
    .bind(i64::try_from(node.id.0).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap()
    .iter()
    .map(|row| row.try_get("detail").unwrap())
    .collect();
    details.extend(
        sqlx::query(
            "EXPLAIN QUERY PLAN \
             SELECT incarnation_id, started_at FROM node_incarnations \
             WHERE node_id = ? AND (started_at, incarnation_id) < (?, ?) \
             ORDER BY started_at DESC, incarnation_id DESC LIMIT 32",
        )
        .bind(i64::try_from(node.id.0).unwrap())
        .bind(timestamp(T0))
        .bind(FIRST_ID)
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get("detail").unwrap()),
    );

    assert!(
        details
            .iter()
            .any(|detail| detail.contains("node_incarnations_history"))
    );
    assert!(
        details
            .iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE"))
    );
}

#[tokio::test]
async fn prune_candidates_are_terminal_strict_old_scoped_and_exact() {
    let (pool, _tmp) = fresh_pool().await;
    let first = seed_node(&pool, "prune-a").await;
    let second = seed_node(&pool, "prune-b").await;
    let cutoff = T0 + Duration::seconds(100);
    let ids = [
        FIRST_ID,
        SECOND_ID,
        "2123456789abcdef0123456789abcdef",
        "3123456789abcdef0123456789abcdef",
        "4123456789abcdef0123456789abcdef",
    ];
    for (id, node_id, ended_at) in [
        (ids[0], first.id, cutoff - Duration::seconds(2)),
        (ids[1], first.id, cutoff - Duration::seconds(1)),
        (ids[2], first.id, cutoff),
        (ids[3], first.id, cutoff + Duration::seconds(1)),
        (ids[4], second.id, cutoff - Duration::seconds(3)),
    ] {
        sqlx::query(
            "INSERT INTO node_incarnations \
             (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
             VALUES (?, ?, 'superseded', ?, ?, ?, 'superseded')",
        )
        .bind(id)
        .bind(i64::try_from(node_id.0).unwrap())
        .bind(timestamp(ended_at - Duration::seconds(1)))
        .bind(timestamp(ended_at))
        .bind(timestamp(ended_at))
        .execute(&pool)
        .await
        .unwrap();
    }
    let repo = SqliteNodeIncarnationRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();

    let candidates = repo
        .terminal_before_in_tx(&mut tx, first.id, cutoff)
        .await
        .unwrap();
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
        vec![incarnation(ids[0]), incarnation(ids[1])]
    );
    assert!(
        repo.delete_terminal_if_empty_in_tx(&mut tx, first.id, incarnation(ids[1]), cutoff)
            .await
            .unwrap()
    );
    assert!(
        !repo
            .delete_terminal_if_empty_in_tx(&mut tx, second.id, incarnation(ids[0]), cutoff)
            .await
            .unwrap()
    );
    assert!(
        !repo
            .delete_terminal_if_empty_in_tx(&mut tx, first.id, incarnation(ids[2]), cutoff)
            .await
            .unwrap()
    );
    assert!(
        repo.get_in_tx(&mut tx, incarnation(ids[0]))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn prune_cutoff_compares_offset_timestamps_as_instants() {
    let (pool, _tmp) = fresh_pool().await;
    let node = seed_node(&pool, "offset-prune-node").await;
    let cutoff = T0 + Duration::hours(2);
    let before_id = incarnation("8123456789abcdef0123456789abcdef");
    let after_id = incarnation("9123456789abcdef0123456789abcdef");
    let positive_offset = UtcOffset::from_hms(1, 0, 0).unwrap();
    let negative_offset = UtcOffset::from_hms(-1, 0, 0).unwrap();
    for (id, ended_at) in [
        (
            before_id,
            (cutoff - Duration::nanoseconds(1)).to_offset(positive_offset),
        ),
        (
            after_id,
            (cutoff + Duration::nanoseconds(1)).to_offset(negative_offset),
        ),
    ] {
        sqlx::query(
            "INSERT INTO node_incarnations \
             (incarnation_id, node_id, status, started_at, last_seen_at, ended_at, end_reason) \
             VALUES (?, ?, 'superseded', ?, ?, ?, 'superseded')",
        )
        .bind(id.to_string())
        .bind(i64::try_from(node.id.0).unwrap())
        .bind(timestamp(ended_at - Duration::seconds(1)))
        .bind(timestamp(ended_at))
        .bind(timestamp(ended_at))
        .execute(&pool)
        .await
        .unwrap();
    }
    let repo = SqliteNodeIncarnationRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();

    let candidates = repo
        .terminal_before_in_tx(&mut tx, node.id, cutoff)
        .await
        .unwrap();

    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
        vec![before_id]
    );
    assert!(
        !repo
            .delete_terminal_if_empty_in_tx(&mut tx, node.id, after_id, cutoff)
            .await
            .unwrap()
    );
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
