use super::*;

use time::OffsetDateTime;
use voom_core::VoomError;

use crate::test_support::fresh_initialized_pool_at;

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
