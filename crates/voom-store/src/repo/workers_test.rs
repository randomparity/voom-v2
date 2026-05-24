use super::*;

use sqlx::Executor;
use time::OffsetDateTime;
use voom_core::VoomError;

use crate::repo::{NewNode, Node, NodeKind, NodeRepo, SqliteNodeRepo};
use crate::test_support::{T0, fresh_initialized_pool_at};

async fn pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let p = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (p, tmp)
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
}

#[tokio::test]
async fn register_with_duplicate_name_fails() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let _w = repo.register(sample_new_worker("w-1")).await.unwrap();
    let err = repo.register(sample_new_worker("w-1")).await.unwrap_err();
    assert!(matches!(err, VoomError::Database(_)));
}

#[tokio::test]
async fn record_capability_stores_arrays_as_json() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteWorkerRepo::new(pool.clone());
    let w = repo.register(sample_new_worker("w-1")).await.unwrap();
    let cap = repo
        .record_capability(NewCapability {
            worker_id: w.id,
            operation: "transcode_video".to_owned(),
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
            can_execute: vec!["transcode_video".to_owned()],
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
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let repo = SqliteWorkerRepo::new(pool.clone());
    let missing_node_worker = insert_worker_with_missing_node(&pool).await;

    let err = repo.get_inspection(missing_node_worker).await.unwrap_err();
    assert!(
        matches!(err, VoomError::Database(message) if message.contains("missing node context"))
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

async fn worker_repo_with_current_schema() -> (tempfile::NamedTempFile, SqliteWorkerRepo) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (tmp, SqliteWorkerRepo::new(pool))
}

async fn worker_repo_with_seeded_node() -> (tempfile::NamedTempFile, SqliteWorkerRepo, Node) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
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
