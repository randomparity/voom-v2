use serde_json::json;
use time::OffsetDateTime;
use voom_core::{NodeId, WorkerId};

use super::*;

struct Fixture {
    pool: sqlx::SqlitePool,
    repo: SqliteRemoteIdempotencyRepo,
    node_id: NodeId,
    worker_id: WorkerId,
    _tmp: tempfile::NamedTempFile,
}

async fn fixture() -> Fixture {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let repo = SqliteRemoteIdempotencyRepo::new(pool.clone());

    let node_id = NodeId(
        sqlx::query(
            "INSERT INTO nodes \
             (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
              auth_token_hash, auth_token_hint, metadata) \
             VALUES ('node-1', 'synthetic', 'registered', '1970-01-01T00:00:00Z', \
                     '1970-01-01T00:00:00Z', 60, 'token-hash', 'hint', '{}')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let worker_id = WorkerId(
        sqlx::query(
            "INSERT INTO workers (name, kind, status, node_id, registered_at, last_seen_at) \
             VALUES ('worker-1', 'remote', 'registered', ?, \
                     '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        )
        .bind(i64::try_from(node_id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );

    Fixture {
        pool,
        repo,
        node_id,
        worker_id,
        _tmp: tmp,
    }
}

#[tokio::test]
async fn same_scope_key_and_hash_replays_stored_response() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut tx = fixture.pool.begin().await.unwrap();
    let first = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: "POST /v1/execution/lease/acquire".to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: "same-key".to_owned(),
                request_hash: "hash-a".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(first, IdempotencyOutcome::Reserved);

    repo.complete_in_tx(
        &mut tx,
        node_id,
        "POST /v1/execution/lease/acquire",
        Some(worker_id),
        "same-key",
        RemoteMutationReplay::Ok {
            data: json!({"lease_id":1}),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = fixture.pool.begin().await.unwrap();
    let replay = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: "POST /v1/execution/lease/acquire".to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: "same-key".to_owned(),
                request_hash: "hash-a".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        replay,
        IdempotencyOutcome::Replay(RemoteMutationReplay::Ok {
            data: json!({"lease_id":1}),
        })
    );
}

#[tokio::test]
async fn same_scope_key_and_hash_replays_stored_error_response() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut tx = fixture.pool.begin().await.unwrap();
    let first = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: "POST /v1/execution/lease/1/reject".to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: "error-key".to_owned(),
                request_hash: "hash-error".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(first, IdempotencyOutcome::Reserved);

    repo.complete_in_tx(
        &mut tx,
        node_id,
        "POST /v1/execution/lease/1/reject",
        Some(worker_id),
        "error-key",
        RemoteMutationReplay::Error {
            code: "WORKER_REJECTED".to_owned(),
            message: "worker rejected lease completion".to_owned(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = fixture.pool.begin().await.unwrap();
    let replay = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: "POST /v1/execution/lease/1/reject".to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: "error-key".to_owned(),
                request_hash: "hash-error".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        replay,
        IdempotencyOutcome::Replay(RemoteMutationReplay::Error {
            code: "WORKER_REJECTED".to_owned(),
            message: "worker rejected lease completion".to_owned(),
        })
    );
}

#[tokio::test]
async fn completed_row_with_malformed_replay_payload_is_database_error() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;
    let route_key = "POST /v1/execution/lease/1/malformed";
    let idempotency_key = "malformed-key";

    let mut tx = fixture.pool.begin().await.unwrap();
    let first = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: route_key.to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: idempotency_key.to_owned(),
                request_hash: "hash-malformed".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(first, IdempotencyOutcome::Reserved);

    sqlx::query(
        "UPDATE remote_idempotency_keys \
         SET status = 'completed', response_json = ? \
         WHERE node_id = ? AND route_key = ? AND worker_scope_id = ? AND idempotency_key = ?",
    )
    .bind(r#"{"status":"ok"}"#)
    .bind(i64::try_from(node_id.0).unwrap())
    .bind(route_key)
    .bind(i64::try_from(worker_id.0).unwrap())
    .bind(idempotency_key)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = fixture.pool.begin().await.unwrap();
    let err = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: route_key.to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: idempotency_key.to_owned(),
                request_hash: "hash-malformed".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "DB_UNREACHABLE");
    assert!(err.to_string().contains("response_json"));
}

#[tokio::test]
async fn same_scope_key_with_different_hash_is_conflict() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut tx = fixture.pool.begin().await.unwrap();
    repo.reserve_or_replay_in_tx(
        &mut tx,
        RemoteIdempotencyInput {
            node_id,
            route_key: "POST /v1/execution/lease/1/complete".to_owned(),
            worker_id: Some(worker_id),
            idempotency_key: "complete-key".to_owned(),
            request_hash: "hash-a".to_owned(),
            created_at: now,
        },
    )
    .await
    .unwrap();
    repo.complete_in_tx(
        &mut tx,
        node_id,
        "POST /v1/execution/lease/1/complete",
        Some(worker_id),
        "complete-key",
        RemoteMutationReplay::Ok {
            data: json!({"lease_id":1}),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = fixture.pool.begin().await.unwrap();
    let err = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: "POST /v1/execution/lease/1/complete".to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: "complete-key".to_owned(),
                request_hash: "hash-b".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "CONFLICT");
    assert!(
        err.to_string()
            .contains("idempotency key reused with different request body")
    );
}

#[tokio::test]
async fn same_scope_key_and_hash_while_in_progress_is_conflict() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut tx = fixture.pool.begin().await.unwrap();
    let first = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: "POST /v1/execution/lease/1/renew".to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: "renew-key".to_owned(),
                request_hash: "hash-a".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(first, IdempotencyOutcome::Reserved);
    tx.commit().await.unwrap();

    let mut tx = fixture.pool.begin().await.unwrap();
    let err = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: "POST /v1/execution/lease/1/renew".to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: "renew-key".to_owned(),
                request_hash: "hash-a".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFLICT");
    assert!(
        err.to_string()
            .contains("idempotency key is already in progress")
    );
}

#[tokio::test]
async fn unscoped_key_uses_zero_scope_and_does_not_collide_with_worker_scope() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;
    let route_key = "POST /v1/execution/node-action";
    let idempotency_key = "scope-key";

    let mut tx = fixture.pool.begin().await.unwrap();
    let unscoped = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: route_key.to_owned(),
                worker_id: None,
                idempotency_key: idempotency_key.to_owned(),
                request_hash: "node-hash".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(unscoped, IdempotencyOutcome::Reserved);

    let worker_scoped = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: route_key.to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: idempotency_key.to_owned(),
                request_hash: "worker-hash".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(worker_scoped, IdempotencyOutcome::Reserved);
    tx.commit().await.unwrap();

    let rows = sqlx::query(
        "SELECT worker_scope_id, worker_id FROM remote_idempotency_keys \
         WHERE node_id = ? AND route_key = ? AND idempotency_key = ? \
         ORDER BY worker_scope_id",
    )
    .bind(i64::try_from(node_id.0).unwrap())
    .bind(route_key)
    .bind(idempotency_key)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<i64, _>("worker_scope_id"), 0);
    assert!(rows[0].get::<Option<i64>, _>("worker_id").is_none());
    assert_eq!(
        rows[1].get::<i64, _>("worker_scope_id"),
        i64::try_from(worker_id.0).unwrap()
    );
    assert_eq!(
        rows[1].get::<Option<i64>, _>("worker_id"),
        Some(i64::try_from(worker_id.0).unwrap())
    );
}

#[tokio::test]
async fn repoint_completed_replay_overwrites_stored_response() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;
    let route = "POST /v1/execution/lease/acquire";

    let mut tx = fixture.pool.begin().await.unwrap();
    repo.reserve_or_replay_in_tx(
        &mut tx,
        RemoteIdempotencyInput {
            node_id,
            route_key: route.to_owned(),
            worker_id: Some(worker_id),
            idempotency_key: "poison".to_owned(),
            request_hash: "hash-a".to_owned(),
            created_at: now,
        },
    )
    .await
    .unwrap();
    repo.complete_in_tx(
        &mut tx,
        node_id,
        route,
        Some(worker_id),
        "poison",
        RemoteMutationReplay::Ok {
            data: json!({"unreadable": true}),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Repoint the completed row to a terminal Error.
    let mut tx = fixture.pool.begin().await.unwrap();
    repo.repoint_completed_replay_in_tx(
        &mut tx,
        node_id,
        route,
        Some(worker_id),
        "poison",
        RemoteMutationReplay::Error {
            code: "INTERNAL".to_owned(),
            message: "replay result unreadable".to_owned(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // A subsequent replay returns the terminal Error, not the original Ok.
    let mut tx = fixture.pool.begin().await.unwrap();
    let replay = repo
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: route.to_owned(),
                worker_id: Some(worker_id),
                idempotency_key: "poison".to_owned(),
                request_hash: "hash-a".to_owned(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        replay,
        IdempotencyOutcome::Replay(RemoteMutationReplay::Error {
            code: "INTERNAL".to_owned(),
            message: "replay result unreadable".to_owned(),
        })
    );
}

#[tokio::test]
async fn repoint_completed_replay_rejects_unknown_or_in_progress_key() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;
    let route = "POST /v1/execution/lease/acquire";

    // Reserve only (status stays 'in_progress', never completed).
    let mut tx = fixture.pool.begin().await.unwrap();
    repo.reserve_or_replay_in_tx(
        &mut tx,
        RemoteIdempotencyInput {
            node_id,
            route_key: route.to_owned(),
            worker_id: Some(worker_id),
            idempotency_key: "in-progress".to_owned(),
            request_hash: "hash-a".to_owned(),
            created_at: now,
        },
    )
    .await
    .unwrap();

    let err = repo
        .repoint_completed_replay_in_tx(
            &mut tx,
            node_id,
            route,
            Some(worker_id),
            "in-progress",
            RemoteMutationReplay::Error {
                code: "INTERNAL".to_owned(),
                message: "should not apply".to_owned(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}
