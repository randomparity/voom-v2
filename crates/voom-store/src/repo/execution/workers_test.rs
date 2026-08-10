use super::*;

use std::str::FromStr;

use sqlx::Executor;
use time::OffsetDateTime;
use voom_core::{ErrorCode, NodeIncarnationId, TicketOperation, VoomError};

use crate::repo::execution::node_incarnations::{NewNodeIncarnation, SqliteNodeIncarnationRepo};
use crate::repo::execution::nodes::{NewNode, Node, NodeKind, SqliteNodeRepo};
use crate::test_support::{T0, fresh_initialized_pool_at, with_check_constraints_disabled};

async fn pool() -> (sqlx::SqlitePool, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let p = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (p, tmp)
}

fn worker_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

fn sample_new_worker(name: &str) -> NewWorker {
    NewWorker {
        name: name.to_owned(),
        kind: WorkerKind::Synthetic,
        registered_at: OffsetDateTime::UNIX_EPOCH,
        node_id: None,
    }
}

#[tokio::test]
async fn register_returns_worker_in_registered_status() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let w = repo.register(sample_new_worker("w-1")).await.unwrap();
    assert!(w.id.0 > 0);
    assert_eq!(w.name, "w-1");
    assert_eq!(w.status, WorkerStatus::Registered);
    assert_eq!(w.retired_at, None);
    assert_eq!(w.node_incarnation_id, None);
}

#[tokio::test]
async fn bind_count_and_retire_incarnation_workers_preserve_typed_ownership() {
    let (pool, _tmp) = pool().await;
    let node = register_test_node(&pool, "node-a").await;
    let incarnation_id = NodeIncarnationId::from_str("0123456789abcdef0123456789abcdef").unwrap();
    let incarnations = SqliteNodeIncarnationRepo::new(pool.clone());
    let workers = SqliteWorkerRepo::new(pool.clone());
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
    let worker = workers
        .register_in_tx(
            &mut tx,
            NewWorker {
                name: "incarnation-worker".to_owned(),
                kind: WorkerKind::Remote,
                registered_at: T0,
                node_id: Some(node.id),
            },
        )
        .await
        .unwrap();
    let bound = workers
        .bind_incarnation_in_tx(&mut tx, worker.id, node.id, incarnation_id)
        .await
        .unwrap();
    assert_eq!(bound.node_incarnation_id, Some(incarnation_id));
    assert_eq!(
        workers
            .incarnation_owned_worker_in_tx(&mut tx, worker.id, node.id, incarnation_id)
            .await
            .unwrap()
            .id,
        worker.id
    );
    let retired = workers
        .retire_live_for_incarnation_in_tx(&mut tx, incarnation_id, T0)
        .await
        .unwrap();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].id, worker.id);
    assert_eq!(retired[0].status, WorkerStatus::Retired);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn prune_retired_incarnation_workers_deletes_only_unreferenced_owned_rows() {
    let (pool, _tmp) = pool().await;
    let node = register_test_node(&pool, "prune-worker-node").await;
    let incarnation_id = NodeIncarnationId::from_str("5123456789abcdef0123456789abcdef").unwrap();
    let incarnations = SqliteNodeIncarnationRepo::new(pool.clone());
    let workers = SqliteWorkerRepo::new(pool.clone());
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
    let retired = workers
        .register_in_tx(
            &mut tx,
            NewWorker {
                name: "prune-retired".to_owned(),
                kind: WorkerKind::Remote,
                registered_at: T0,
                node_id: Some(node.id),
            },
        )
        .await
        .unwrap();
    workers
        .bind_incarnation_in_tx(&mut tx, retired.id, node.id, incarnation_id)
        .await
        .unwrap();
    workers
        .retire_in_tx(&mut tx, retired.id, retired.epoch, T0)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO worker_capabilities \
         (worker_id, operation, codecs, hardware, artifact_access, extra) \
         VALUES (?, 'probe', '[]', '[]', '[]', '{}')",
    )
    .bind(i64::try_from(retired.id.0).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO worker_grants \
         (worker_id, can_execute, can_access_read, can_access_write, denies, max_parallel) \
         VALUES (?, '[]', '[]', '[]', '[]', '{}')",
    )
    .bind(i64::try_from(retired.id.0).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    let live = workers
        .register_in_tx(
            &mut tx,
            NewWorker {
                name: "prune-live".to_owned(),
                kind: WorkerKind::Remote,
                registered_at: T0,
                node_id: Some(node.id),
            },
        )
        .await
        .unwrap();
    workers
        .bind_incarnation_in_tx(&mut tx, live.id, node.id, incarnation_id)
        .await
        .unwrap();
    let referenced = workers
        .register_in_tx(
            &mut tx,
            NewWorker {
                name: "prune-referenced".to_owned(),
                kind: WorkerKind::Remote,
                registered_at: T0,
                node_id: Some(node.id),
            },
        )
        .await
        .unwrap();
    workers
        .bind_incarnation_in_tx(&mut tx, referenced.id, node.id, incarnation_id)
        .await
        .unwrap();
    workers
        .retire_in_tx(&mut tx, referenced.id, referenced.epoch, T0)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO remote_idempotency_keys \
         (node_id, route_key, worker_scope_id, worker_id, idempotency_key, request_hash, \
          response_json, status, created_at) \
         VALUES (?, 'test', ?, ?, 'retain', 'hash', '{}', 'completed', ?)",
    )
    .bind(i64::try_from(node.id.0).unwrap())
    .bind(i64::try_from(referenced.id.0).unwrap())
    .bind(i64::try_from(referenced.id.0).unwrap())
    .bind(T0.to_string())
    .execute(&mut *tx)
    .await
    .unwrap();

    assert_eq!(
        workers
            .prune_retired_for_incarnation_in_tx(&mut tx, incarnation_id)
            .await
            .unwrap(),
        1
    );
    assert!(
        super::get_in_tx(&mut tx, retired.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(super::get_in_tx(&mut tx, live.id).await.unwrap().is_some());
    assert!(
        super::get_in_tx(&mut tx, referenced.id)
            .await
            .unwrap()
            .is_some()
    );
    let owned_rows: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM worker_capabilities WHERE worker_id = ?) + \
                (SELECT COUNT(*) FROM worker_grants WHERE worker_id = ?)",
    )
    .bind(i64::try_from(retired.id.0).unwrap())
    .bind(i64::try_from(retired.id.0).unwrap())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(owned_rows, 0);
}

#[tokio::test]
async fn prune_retired_incarnation_workers_rejects_corrupt_candidate() {
    let (pool, _tmp) = pool().await;
    let node = register_test_node(&pool, "corrupt-prune-worker-node").await;
    let incarnation_id = NodeIncarnationId::from_str("6123456789abcdef0123456789abcdef").unwrap();
    let incarnations = SqliteNodeIncarnationRepo::new(pool.clone());
    let workers = SqliteWorkerRepo::new(pool.clone());
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
    let worker = workers
        .register_in_tx(
            &mut tx,
            NewWorker {
                name: "corrupt-prune-worker".to_owned(),
                kind: WorkerKind::Remote,
                registered_at: T0,
                node_id: Some(node.id),
            },
        )
        .await
        .unwrap();
    workers
        .bind_incarnation_in_tx(&mut tx, worker.id, node.id, incarnation_id)
        .await
        .unwrap();
    workers
        .retire_in_tx(&mut tx, worker.id, worker.epoch, T0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let raw_worker_id = i64::try_from(worker.id.0).unwrap();
    with_check_constraints_disabled(&pool, move |connection| {
        Box::pin(async move {
            sqlx::query("UPDATE workers SET retired_at = NULL WHERE id = ?")
                .bind(raw_worker_id)
                .execute(&mut *connection)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();

    let error = workers
        .prune_retired_for_incarnation_in_tx(&mut tx, incarnation_id)
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workers WHERE id = ?")
        .bind(i64::try_from(worker.id.0).unwrap())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(row_count, 1);
}

#[tokio::test]
async fn prune_retains_both_scheduler_decision_worker_references() {
    let (pool, _tmp) = pool().await;
    let node = register_test_node(&pool, "scheduler-hold-node").await;
    let incarnation_id = NodeIncarnationId::from_str("7123456789abcdef0123456789abcdef").unwrap();
    let incarnations = SqliteNodeIncarnationRepo::new(pool.clone());
    let workers = SqliteWorkerRepo::new(pool.clone());
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
    let mut held = Vec::new();
    for name in ["scheduler-request-hold", "scheduler-selected-hold"] {
        let worker = workers
            .register_in_tx(
                &mut tx,
                NewWorker {
                    name: name.to_owned(),
                    kind: WorkerKind::Remote,
                    registered_at: T0,
                    node_id: Some(node.id),
                },
            )
            .await
            .unwrap();
        workers
            .bind_incarnation_in_tx(&mut tx, worker.id, node.id, incarnation_id)
            .await
            .unwrap();
        workers
            .retire_in_tx(&mut tx, worker.id, worker.epoch, T0)
            .await
            .unwrap();
        held.push(worker.id);
    }
    let timestamp = T0.to_string();
    sqlx::query(
        "INSERT INTO scheduler_decisions \
         (created_at, updated_at, first_seen_at, last_seen_at, decision_kind, request_source, \
          request_worker_id, outcome, reason_code, summary, candidate_count, explanation_json) \
         VALUES (?, ?, ?, ?, 'no_candidate', 'remote_acquire', ?, 'no_eligible_candidate', \
                 'missing_capability', 'request hold', 1, '{}')",
    )
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(i64::try_from(held[0].0).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scheduler_decisions \
         (created_at, updated_at, first_seen_at, last_seen_at, decision_kind, request_source, \
          selected_worker_id, outcome, reason_code, summary, candidate_count, explanation_json) \
         VALUES (?, ?, ?, ?, 'lease_acquire', 'remote_acquire', ?, 'selected', 'selected', \
                 'selected hold', 1, '{}')",
    )
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(i64::try_from(held[1].0).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();

    assert_eq!(
        workers
            .prune_retired_for_incarnation_in_tx(&mut tx, incarnation_id)
            .await
            .unwrap(),
        0
    );
    let references: Vec<(Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT request_worker_id, selected_worker_id FROM scheduler_decisions ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        references,
        vec![
            (Some(i64::try_from(held[0].0).unwrap()), None),
            (None, Some(i64::try_from(held[1].0).unwrap()))
        ]
    );
    for worker_id in held {
        assert!(
            super::get_in_tx(&mut tx, worker_id)
                .await
                .unwrap()
                .is_some()
        );
    }
}

#[tokio::test]
async fn worker_foreign_key_actions_preserve_pruning_audit_holds() {
    let (pool, _tmp) = pool().await;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut cascade = Vec::new();
    let mut set_null = Vec::new();
    let mut restrict = Vec::new();
    for table in tables {
        assert!(
            table
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        );
        let rows = sqlx::query(&format!("PRAGMA foreign_key_list(\"{table}\")"))
            .fetch_all(&pool)
            .await
            .unwrap();
        for row in rows {
            let referenced_table: String = row.try_get("table").unwrap();
            if referenced_table != "workers" {
                continue;
            }
            let from: String = row.try_get("from").unwrap();
            let to: String = row.try_get("to").unwrap();
            assert_eq!(to, "id");
            let edge = format!("{table}.{from}");
            let on_delete: String = row.try_get("on_delete").unwrap();
            match on_delete.as_str() {
                "CASCADE" => cascade.push(edge),
                "SET NULL" => set_null.push(edge),
                "RESTRICT" => restrict.push(edge),
                action => panic!("unexpected worker foreign-key delete action {action}: {edge}"),
            }
        }
    }
    cascade.sort();
    set_null.sort();
    restrict.sort();
    assert_eq!(
        cascade,
        ["worker_capabilities.worker_id", "worker_grants.worker_id"]
    );
    assert_eq!(
        set_null,
        [
            "scheduler_decisions.request_worker_id",
            "scheduler_decisions.selected_worker_id"
        ]
    );
    assert!(!restrict.is_empty());
}

#[tokio::test]
async fn worker_reads_reject_node_incarnation_owner_disagreement() {
    let (pool, _tmp) = pool().await;
    let owner = register_test_node(&pool, "owner").await;
    let worker_node = register_test_node(&pool, "worker-node").await;
    let incarnation_id = "0123456789abcdef0123456789abcdef";
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES (?, ?, 'active', ?, ?)",
    )
    .bind(incarnation_id)
    .bind(i64::try_from(owner.id.0).unwrap())
    .bind(T0.to_string())
    .bind(T0.to_string())
    .execute(&pool)
    .await
    .unwrap();
    let repo = SqliteWorkerRepo::new(pool.clone());
    let worker = repo
        .register(NewWorker {
            name: "mismatched-worker".to_owned(),
            kind: WorkerKind::Remote,
            registered_at: T0,
            node_id: Some(worker_node.id),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET node_incarnation_id = ? WHERE id = ?")
        .bind(incarnation_id)
        .bind(i64::try_from(worker.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let error = repo.get(worker.id).await.unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert!(error.to_string().contains("incarnation owner"));
    repo.record_capability(NewCapability {
        worker_id: worker.id,
        operation: worker_op("probe"),
        codecs: vec![],
        hardware: vec![],
        artifact_access: vec![],
        extra: serde_json::json!({}),
    })
    .await
    .unwrap();
    let capability_error = repo
        .runtime_capabilities_for_operations(&[worker_op("probe")])
        .await
        .unwrap_err();
    assert_eq!(capability_error.error_code(), ErrorCode::DbUnreachable);
}

#[tokio::test]
async fn worker_reads_reject_malformed_persisted_incarnation_id() {
    let (pool, _tmp) = pool().await;
    let node = register_test_node(&pool, "node-a").await;
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES ('malformed', ?, 'active', ?, ?)",
    )
    .bind(i64::try_from(node.id.0).unwrap())
    .bind(T0.to_string())
    .bind(T0.to_string())
    .execute(&pool)
    .await
    .unwrap();
    let repo = SqliteWorkerRepo::new(pool.clone());
    let worker = repo
        .register(NewWorker {
            name: "malformed-incarnation-worker".to_owned(),
            kind: WorkerKind::Remote,
            registered_at: T0,
            node_id: Some(node.id),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET node_incarnation_id = 'malformed' WHERE id = ?")
        .bind(i64::try_from(worker.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let error = repo.get(worker.id).await.unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert!(error.to_string().contains("node incarnation id"));
}

#[tokio::test]
async fn register_with_duplicate_name_fails() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let _w = repo.register(sample_new_worker("w-1")).await.unwrap();
    let err = repo.register(sample_new_worker("w-1")).await.unwrap_err();
    assert!(matches!(err, VoomError::Database { .. }));
}

#[tokio::test]
async fn register_builtin_if_missing_inserts_and_returns_the_worker() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let input = NewWorker {
        name: "builtin.verify_artifact".to_owned(),
        kind: WorkerKind::Local,
        registered_at: T0,
        node_id: None,
    };
    let mut tx = pool.begin().await.unwrap();

    let worker = repo
        .register_builtin_if_missing_in_tx(&mut tx, input)
        .await
        .unwrap();

    assert_eq!(worker.name, "builtin.verify_artifact");
    assert_eq!(worker.kind, WorkerKind::Local);
    assert_eq!(worker.status, WorkerStatus::Registered);
    assert_eq!(worker.registered_at, T0);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn register_builtin_if_missing_returns_existing_identity_without_mutation() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let existing = repo
        .register(sample_new_worker("builtin.worker"))
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();

    let returned = repo
        .register_builtin_if_missing_in_tx(
            &mut tx,
            NewWorker {
                name: existing.name.clone(),
                kind: WorkerKind::Local,
                registered_at: T0 + time::Duration::hours(1),
                node_id: Some(voom_core::NodeId(42)),
            },
        )
        .await
        .unwrap();

    assert_eq!(returned.id, existing.id);
    assert_eq!(returned.name, existing.name);
    assert_eq!(returned.kind, existing.kind);
    assert_eq!(returned.registered_at, existing.registered_at);
    assert_eq!(returned.node_id, existing.node_id);
    assert_eq!(returned.epoch, existing.epoch);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn get_by_name_returns_seeded_worker() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let worker = repo
        .register(sample_new_worker("builtin.ffprobe"))
        .await
        .unwrap();

    let found = repo.get_by_name("builtin.ffprobe").await.unwrap().unwrap();

    assert_eq!(found.id, worker.id);
    assert_eq!(found.name, "builtin.ffprobe");
}

#[tokio::test]
async fn get_by_name_returns_none_for_missing_name() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());

    let found = repo.get_by_name("missing.worker").await.unwrap();

    assert!(found.is_none());
}

#[tokio::test]
async fn list_by_name_namespace_includes_exact_prefixed_and_retired_workers() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool);
    let exact = repo
        .register(sample_new_worker("local-ffmpeg"))
        .await
        .unwrap();
    let prefixed = repo
        .register(sample_new_worker("local-ffmpeg-incarnation"))
        .await
        .unwrap();
    let unrelated = repo.register(sample_new_worker("ffmpeg")).await.unwrap();
    repo.retire(prefixed.id, prefixed.epoch, T0).await.unwrap();

    let workers = repo
        .list_by_name_namespace("local-ffmpeg", "local-ffmpeg-")
        .await
        .unwrap();

    assert_eq!(workers.len(), 2);
    assert_eq!(workers[0].id, exact.id);
    assert_eq!(workers[1].id, prefixed.id);
    assert_eq!(workers[1].status, WorkerStatus::Retired);
    assert!(!workers.iter().any(|worker| worker.id == unrelated.id));
}

#[tokio::test]
async fn record_capability_stores_arrays_as_json() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let w = repo.register(sample_new_worker("w-1")).await.unwrap();
    let cap = repo
        .record_capability(NewCapability {
            worker_id: w.id,
            operation: worker_op("transcode_video"),
            codecs: vec!["h264".to_owned(), "hevc".to_owned()],
            hardware: vec!["cuda".to_owned()],
            artifact_access: vec!["local_path".to_owned()],
            extra: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert!(cap.id > 0);
}

#[tokio::test]
async fn record_grant_stores_max_parallel_as_json_object() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let w = repo.register(sample_new_worker("w-1")).await.unwrap();
    let g = repo
        .record_grant(NewGrant {
            worker_id: w.id,
            can_execute: vec![worker_op("transcode_video")],
            can_access_read: vec!["local_path".to_owned()],
            can_access_write: vec!["staging".to_owned()],
            denies: vec![],
            max_parallel: serde_json::json!({"transcode_video": 2}),
        })
        .await
        .unwrap();
    assert!(g.id > 0);
}

#[tokio::test]
async fn runtime_capabilities_for_operations_projects_live_workers_epoch_operation_and_extra() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let registered = repo
        .register(sample_new_worker("registered"))
        .await
        .unwrap();
    let active = repo.register(sample_new_worker("active")).await.unwrap();
    let stale = repo.register(sample_new_worker("stale")).await.unwrap();
    let retired = repo.register(sample_new_worker("retired")).await.unwrap();
    let operation = worker_op("transcode_video");
    let extra = serde_json::json!({"endpoint": "127.0.0.1:8080", "secret": "token"});

    for worker_id in [registered.id, active.id, stale.id, retired.id] {
        repo.record_capability(NewCapability {
            worker_id,
            operation: operation.clone(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: extra.clone(),
        })
        .await
        .unwrap();
    }
    sqlx::query("UPDATE workers SET status = 'active', epoch = 4 WHERE id = ?")
        .bind(i64::try_from(active.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET status = 'stale' WHERE id = ?")
        .bind(i64::try_from(stale.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    repo.retire(retired.id, retired.epoch, T0).await.unwrap();

    let capabilities = repo
        .runtime_capabilities_for_operations(std::slice::from_ref(&operation))
        .await
        .unwrap();

    assert_eq!(capabilities.len(), 2);
    assert_eq!(capabilities[0].worker_id, registered.id);
    assert_eq!(capabilities[0].worker_epoch, registered.epoch);
    assert_eq!(capabilities[0].operation, operation);
    assert_eq!(capabilities[0].extra, extra);
    assert_eq!(capabilities[1].worker_id, active.id);
    assert_eq!(capabilities[1].worker_epoch, 4);
}

#[tokio::test]
async fn runtime_capabilities_for_operations_returns_empty_without_operations() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool);

    let capabilities = repo.runtime_capabilities_for_operations(&[]).await.unwrap();

    assert!(capabilities.is_empty());
}

#[tokio::test]
async fn runtime_capabilities_for_operations_rejects_negative_worker_epoch() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let worker = repo
        .register(sample_new_worker("negative-epoch"))
        .await
        .unwrap();
    let operation = worker_op("transcode_video");
    repo.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra: serde_json::json!({}),
    })
    .await
    .unwrap();
    sqlx::query("UPDATE workers SET epoch = -1 WHERE id = ?")
        .bind(i64::try_from(worker.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let error = repo
        .runtime_capabilities_for_operations(std::slice::from_ref(&operation))
        .await
        .unwrap_err();

    let VoomError::Database { message, .. } = error else {
        panic!("expected a database error naming the corrupt epoch, got {error:?}");
    };
    assert!(
        message.contains("runtime worker capability worker epoch was negative: -1"),
        "expected the error to name the corrupt epoch, got {message:?}"
    );
}

#[tokio::test]
async fn retire_transitions_status_and_sets_retired_at() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let w = repo.register(sample_new_worker("w-1")).await.unwrap();
    let when = OffsetDateTime::UNIX_EPOCH + time::Duration::days(3);
    let r = repo.retire(w.id, w.epoch, when).await.unwrap();
    assert_eq!(r.status, WorkerStatus::Retired);
    assert_eq!(r.retired_at, Some(when));
}

#[tokio::test]
async fn retire_with_stale_epoch_returns_conflict() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let w = repo.register(sample_new_worker("w-1")).await.unwrap();
    let err = repo
        .retire(w.id, w.epoch + 7, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));
}

#[tokio::test]
async fn retire_missing_worker_returns_not_found() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let err = repo
        .retire(WorkerId(424_242), 0, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap_err();
    assert!(
        matches!(err, VoomError::NotFound(_)),
        "expected NotFound for a missing worker, got {err:?}"
    );
}

#[tokio::test]
async fn list_by_status_filters_correctly() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let a = repo.register(sample_new_worker("a")).await.unwrap();
    let _b = repo.register(sample_new_worker("b")).await.unwrap();
    repo.retire(a.id, a.epoch, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    let registered = repo
        .list_by_status(WorkerStatus::Registered, 10)
        .await
        .unwrap();
    let retired = repo
        .list_by_status(WorkerStatus::Retired, 10)
        .await
        .unwrap();
    assert_eq!(registered.len(), 1);
    assert_eq!(retired.len(), 1);
}

#[tokio::test]
async fn legacy_worker_without_node_remains_listable_with_null_node_context() {
    let (_tmp, repo) = worker_repo_with_current_schema().await;
    let worker = repo
        .register(NewWorker {
            name: "legacy".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();

    let inspection = repo.get_inspection(worker.id).await.unwrap().unwrap();
    assert_eq!(inspection.worker.id, worker.id);
    assert!(inspection.node.is_none());
}

#[tokio::test]
async fn worker_registered_with_node_id_projects_node_context() {
    let (_tmp, worker_repo, node) = worker_repo_with_seeded_node().await;
    let worker = worker_repo
        .register(NewWorker {
            name: "linked".to_owned(),
            kind: WorkerKind::Remote,
            registered_at: T0,
            node_id: Some(node.id),
        })
        .await
        .unwrap();

    let inspection = worker_repo
        .get_inspection(worker.id)
        .await
        .unwrap()
        .unwrap();
    let context = inspection.node.unwrap();
    assert_eq!(context.id, node.id);
    assert_eq!(context.name, node.name);
    assert_eq!(context.kind, node.kind);
    assert_eq!(context.status, node.status);
    assert_eq!(context.last_seen_at, node.last_seen_at);
}

#[tokio::test]
async fn worker_inspection_rejects_missing_node_context_for_linked_worker() {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let repo = SqliteWorkerRepo::new(pool.clone());
    let missing_node_worker = insert_worker_with_missing_node(&pool).await;

    let err = repo.get_inspection(missing_node_worker).await.unwrap_err();
    assert!(
        matches!(err, VoomError::Database { message, .. } if message.contains("missing node context"))
    );
}

#[tokio::test]
async fn list_inspections_without_status_projects_linked_and_legacy_workers_in_worker_order() {
    let (_tmp, repo, node) = worker_repo_with_seeded_node().await;
    let late_legacy = repo
        .register(NewWorker {
            name: "late-legacy".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0 + time::Duration::seconds(20),
            node_id: None,
        })
        .await
        .unwrap();
    let linked = repo
        .register(NewWorker {
            name: "linked".to_owned(),
            kind: WorkerKind::Remote,
            registered_at: T0,
            node_id: Some(node.id),
        })
        .await
        .unwrap();
    let early_legacy = repo
        .register(NewWorker {
            name: "early-legacy".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0 + time::Duration::seconds(10),
            node_id: None,
        })
        .await
        .unwrap();

    let inspections = repo.list_inspections(None, 2).await.unwrap();

    assert_eq!(
        inspections.iter().map(|i| i.worker.id).collect::<Vec<_>>(),
        vec![linked.id, early_legacy.id]
    );
    assert_eq!(inspections[0].node.as_ref().unwrap().id, node.id);
    assert!(inspections[1].node.is_none());
    assert!(!inspections.iter().any(|i| i.worker.id == late_legacy.id));
}

#[tokio::test]
async fn list_inspections_with_status_filters_worker_rows_before_projecting_context() {
    let (_tmp, repo, node) = worker_repo_with_seeded_node().await;
    let linked = repo
        .register(NewWorker {
            name: "linked".to_owned(),
            kind: WorkerKind::Remote,
            registered_at: T0,
            node_id: Some(node.id),
        })
        .await
        .unwrap();
    let retired_legacy = repo
        .register(NewWorker {
            name: "retired-legacy".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0 + time::Duration::seconds(1),
            node_id: None,
        })
        .await
        .unwrap();
    repo.retire(retired_legacy.id, retired_legacy.epoch, T0)
        .await
        .unwrap();

    let registered = repo
        .list_inspections(Some(WorkerStatus::Registered), 10)
        .await
        .unwrap();
    let retired = repo
        .list_inspections(Some(WorkerStatus::Retired), 10)
        .await
        .unwrap();

    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].worker.id, linked.id);
    assert_eq!(registered[0].node.as_ref().unwrap().id, node.id);
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].worker.id, retired_legacy.id);
    assert!(retired[0].node.is_none());
}

#[tokio::test]
async fn worker_operation_eligibility_requires_capability_and_grant_without_deny() {
    let fixture = worker_fixture().await;
    fixture
        .insert_capability("transcode_video", &["shared_mount"])
        .await;
    fixture.insert_grant(&["transcode_video"], &[]).await;

    let eligible = fixture
        .repo
        .operation_eligibility(fixture.worker_id, &worker_op("transcode_video"))
        .await
        .unwrap();
    assert!(eligible.has_capability);
    assert!(eligible.has_grant);
    assert!(!eligible.is_denied);
    assert_eq!(eligible.worker_status, Some(WorkerStatus::Registered));
    assert!(eligible.is_eligible());
    assert_eq!(eligible.artifact_access, vec!["shared_mount"]);

    fixture.set_status("active").await;
    let active = fixture
        .repo
        .operation_eligibility(fixture.worker_id, &worker_op("transcode_video"))
        .await
        .unwrap();
    assert_eq!(active.worker_status, Some(WorkerStatus::Active));
    assert!(active.is_eligible());
}

#[tokio::test]
async fn worker_operation_eligibility_applies_deny_across_split_grant_rows() {
    let fixture = worker_fixture().await;
    fixture
        .insert_capability("transcode_video", &["shared_mount"])
        .await;
    fixture.insert_grant(&["transcode_video"], &[]).await;
    fixture.insert_grant(&[], &["transcode_video"]).await;

    let eligibility = fixture
        .repo
        .operation_eligibility(fixture.worker_id, &worker_op("transcode_video"))
        .await
        .unwrap();
    assert!(eligibility.has_grant);
    assert!(eligibility.is_denied);
    assert!(!eligibility.is_eligible());
}

#[tokio::test]
async fn worker_operation_eligibility_rejects_stale_and_retired_lifecycle_states() {
    let fixture = worker_fixture().await;
    fixture
        .insert_capability("transcode_video", &["shared_mount"])
        .await;
    fixture.insert_grant(&["transcode_video"], &[]).await;

    fixture.set_status("stale").await;
    let stale = fixture
        .repo
        .operation_eligibility(fixture.worker_id, &worker_op("transcode_video"))
        .await
        .unwrap();
    assert_eq!(stale.worker_status, Some(WorkerStatus::Stale));
    assert!(!stale.is_eligible());

    fixture.set_retired().await;
    let retired = fixture
        .repo
        .operation_eligibility(fixture.worker_id, &worker_op("transcode_video"))
        .await
        .unwrap();
    assert_eq!(retired.worker_status, Some(WorkerStatus::Retired));
    assert!(!retired.is_eligible());
}

#[tokio::test]
async fn candidate_operations_unite_this_workers_rows_and_exclude_denials() {
    let fixture = worker_fixture().await;
    fixture.insert_capability("capability_only", &[]).await;
    fixture.insert_capability("probe_media", &[]).await;
    fixture.insert_capability("transcode_video", &[]).await;
    fixture
        .insert_grant(&["grant_only", "transcode_video"], &["probe_media"])
        .await;
    let other = fixture
        .repo
        .register(sample_new_worker("other-worker"))
        .await
        .unwrap();
    fixture
        .repo
        .record_capability(NewCapability {
            worker_id: other.id,
            operation: worker_op("other_worker_operation"),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: serde_json::json!({}),
        })
        .await
        .unwrap();
    let mut tx = fixture.repo.pool.begin().await.unwrap();

    let operations = fixture
        .repo
        .candidate_operations_in_tx(&mut tx, fixture.worker_id)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(
        operations,
        vec![
            worker_op("capability_only"),
            worker_op("grant_only"),
            worker_op("transcode_video"),
        ]
    );
}

#[tokio::test]
async fn require_live_accepts_registered_and_active_but_distinguishes_rejections() {
    let fixture = worker_fixture().await;
    let mut tx = fixture.repo.pool.begin().await.unwrap();
    let registered = fixture
        .repo
        .require_live_in_tx(&mut tx, fixture.worker_id)
        .await
        .unwrap();
    assert_eq!(registered.status, WorkerStatus::Registered);
    tx.rollback().await.unwrap();

    fixture.set_status("active").await;
    let mut tx = fixture.repo.pool.begin().await.unwrap();
    let active = fixture
        .repo
        .require_live_in_tx(&mut tx, fixture.worker_id)
        .await
        .unwrap();
    assert_eq!(active.status, WorkerStatus::Active);
    tx.rollback().await.unwrap();

    fixture.set_status("stale").await;
    let mut tx = fixture.repo.pool.begin().await.unwrap();
    let stale = fixture
        .repo
        .require_live_in_tx(&mut tx, fixture.worker_id)
        .await
        .unwrap_err();
    tx.rollback().await.unwrap();
    assert!(matches!(stale, VoomError::Conflict(message) if message.contains("Stale")));

    fixture.set_retired().await;
    let mut tx = fixture.repo.pool.begin().await.unwrap();
    let retired = fixture
        .repo
        .require_live_in_tx(&mut tx, fixture.worker_id)
        .await
        .unwrap_err();
    let missing = fixture
        .repo
        .require_live_in_tx(&mut tx, voom_core::WorkerId(99_999))
        .await
        .unwrap_err();
    tx.rollback().await.unwrap();
    assert!(matches!(retired, VoomError::Conflict(message) if message.contains("Retired")));
    assert!(matches!(missing, VoomError::NotFound(_)));
}

#[tokio::test]
async fn operation_candidates_returns_each_effective_worker_once() {
    let fixture = worker_fixture().await;
    fixture
        .insert_capability("transcode_video", &["shared_mount"])
        .await;
    fixture
        .insert_grant_with_limit(&["transcode_video"], &[], serde_json::json!({"*": 2}))
        .await;
    fixture
        .insert_grant_with_limit(
            &["transcode_video"],
            &[],
            serde_json::json!({"transcode_video": 4}),
        )
        .await;

    let candidates = fixture
        .repo
        .operation_candidates(&worker_op("transcode_video"))
        .await
        .unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].worker_id, fixture.worker_id);
    assert_eq!(candidates[0].active_leases, 0);
    assert_eq!(candidates[0].max_parallel, 4);
}

#[tokio::test]
async fn operation_capacity_honors_custom_ticket_operation_limit() {
    let fixture = worker_fixture().await;
    let operation = worker_op("plugin.custom.noop");
    fixture
        .insert_grant_with_limit(
            &[operation.as_str()],
            &[],
            serde_json::json!({"plugin.custom.noop": 7}),
        )
        .await;
    let mut tx = fixture.repo.pool.begin().await.unwrap();

    let capacity = fixture
        .repo
        .operation_capacity_in_tx(&mut tx, fixture.worker_id, &operation)
        .await
        .unwrap();

    assert_eq!(capacity.active_leases, 0);
    assert_eq!(capacity.max_parallel, 7);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn node_owned_worker_in_tx_returns_matching_worker() {
    let (pool, _tmp, repo, node, worker) = worker_with_node_fixture().await;
    let mut tx = pool.begin().await.unwrap();

    let found = repo
        .node_owned_worker_in_tx(&mut tx, worker.id, node.id)
        .await
        .unwrap();

    assert_eq!(found.id, worker.id);
    assert_eq!(found.node_id, Some(node.id));
}

#[tokio::test]
async fn node_owned_worker_in_tx_rejects_wrong_node() {
    let (pool, _tmp, repo, _node, worker) = worker_with_node_fixture().await;
    let other = register_test_node(&pool, "node-b").await;
    let mut tx = pool.begin().await.unwrap();

    let err = repo
        .node_owned_worker_in_tx(&mut tx, worker.id, other.id)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn node_owned_worker_in_tx_returns_not_found_for_missing_worker() {
    let (pool, _tmp, repo, node, _worker) = worker_with_node_fixture().await;
    let mut tx = pool.begin().await.unwrap();

    let err = repo
        .node_owned_worker_in_tx(&mut tx, voom_core::WorkerId(99_999), node.id)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::NotFound(_)), "got: {err:?}");
}

#[tokio::test]
async fn node_owned_worker_in_tx_rejects_worker_without_node() {
    let (_tmp, repo, node) = worker_repo_with_seeded_node().await;
    let legacy = repo
        .register(sample_new_worker("legacy-no-node"))
        .await
        .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();

    let err = repo
        .node_owned_worker_in_tx(&mut tx, legacy.id, node.id)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn node_owned_worker_in_tx_rejects_retired_worker() {
    let (pool, _tmp, repo, node, worker) = worker_with_node_fixture().await;
    repo.retire(worker.id, worker.epoch, T0).await.unwrap();
    let mut tx = pool.begin().await.unwrap();

    let err = repo
        .node_owned_worker_in_tx(&mut tx, worker.id, node.id)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

async fn worker_repo_with_current_schema() -> (voom_test_support::TempDatabase, SqliteWorkerRepo) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (tmp, SqliteWorkerRepo::new(pool))
}

async fn worker_repo_with_seeded_node() -> (voom_test_support::TempDatabase, SqliteWorkerRepo, Node)
{
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let node_repo = SqliteNodeRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let node = node_repo
        .register_in_tx(
            &mut tx,
            NewNode {
                name: "node-a".to_owned(),
                kind: NodeKind::Remote,
                registered_at: T0,
                heartbeat_ttl_seconds: 60,
                auth_token_hash: "voom-node-token-sha256-v1:node-a".to_owned(),
                auth_token_hint: "node-a".to_owned(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (tmp, SqliteWorkerRepo::new(pool), node)
}

async fn worker_with_node_fixture() -> (
    sqlx::SqlitePool,
    voom_test_support::TempDatabase,
    SqliteWorkerRepo,
    Node,
    Worker,
) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let node = register_test_node(&pool, "node-a").await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let worker = repo
        .register(NewWorker {
            name: "node-worker".to_owned(),
            kind: WorkerKind::Remote,
            registered_at: T0,
            node_id: Some(node.id),
        })
        .await
        .unwrap();
    (pool, tmp, repo, node, worker)
}

async fn register_test_node(pool: &sqlx::SqlitePool, name: &str) -> Node {
    let node_repo = SqliteNodeRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let node = node_repo
        .register_in_tx(
            &mut tx,
            NewNode {
                name: name.to_owned(),
                kind: NodeKind::Remote,
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

struct WorkerFixture {
    _tmp: voom_test_support::TempDatabase,
    repo: SqliteWorkerRepo,
    worker_id: voom_core::WorkerId,
}

impl WorkerFixture {
    async fn insert_capability(&self, operation: &str, artifact_access: &[&str]) {
        self.repo
            .record_capability(NewCapability {
                worker_id: self.worker_id,
                operation: worker_op(operation),
                codecs: vec![],
                hardware: vec![],
                artifact_access: artifact_access
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
                extra: serde_json::json!({}),
            })
            .await
            .unwrap();
    }

    async fn insert_grant(&self, can_execute: &[&str], denies: &[&str]) {
        self.insert_grant_with_limit(can_execute, denies, serde_json::json!({}))
            .await;
    }

    async fn insert_grant_with_limit(
        &self,
        can_execute: &[&str],
        denies: &[&str],
        max_parallel: serde_json::Value,
    ) {
        self.repo
            .record_grant(NewGrant {
                worker_id: self.worker_id,
                can_execute: can_execute
                    .iter()
                    .map(|operation| worker_op(operation))
                    .collect(),
                can_access_read: vec![],
                can_access_write: vec![],
                denies: denies
                    .iter()
                    .map(|operation| worker_op(operation))
                    .collect(),
                max_parallel,
            })
            .await
            .unwrap();
    }

    async fn set_status(&self, status: &str) {
        sqlx::query("UPDATE workers SET status = ? WHERE id = ?")
            .bind(status)
            .bind(i64::try_from(self.worker_id.0).unwrap())
            .execute(&self.repo.pool)
            .await
            .unwrap();
    }

    async fn set_retired(&self) {
        sqlx::query("UPDATE workers SET status = 'retired', retired_at = ? WHERE id = ?")
            .bind("1970-01-01T00:00:00Z")
            .bind(i64::try_from(self.worker_id.0).unwrap())
            .execute(&self.repo.pool)
            .await
            .unwrap();
    }
}

async fn worker_fixture() -> WorkerFixture {
    let (pool, tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool);
    let worker = repo
        .register(sample_new_worker("eligible-worker"))
        .await
        .unwrap();
    WorkerFixture {
        _tmp: tmp,
        repo,
        worker_id: worker.id,
    }
}

async fn insert_worker_with_missing_node(pool: &sqlx::SqlitePool) -> voom_core::WorkerId {
    let mut conn = pool.acquire().await.unwrap();
    conn.execute("PRAGMA foreign_keys = OFF").await.unwrap();
    let ts = T0
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap();
    let res = sqlx::query(
        "INSERT INTO workers \
         (name, kind, status, registered_at, last_seen_at, node_id) \
         VALUES (?, 'remote', 'registered', ?, ?, ?)",
    )
    .bind("missing-node")
    .bind(&ts)
    .bind(&ts)
    .bind(9_999_i64)
    .execute(&mut *conn)
    .await
    .unwrap();
    voom_core::WorkerId(u64::try_from(res.last_insert_rowid()).unwrap())
}
