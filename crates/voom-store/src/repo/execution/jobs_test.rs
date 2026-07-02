use super::*;

use time::OffsetDateTime;
use voom_core::VoomError;

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let p = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (p, tmp)
}

fn sample_new_job() -> NewJob {
    NewJob {
        kind: "ingest_scan".to_owned(),
        priority: 5,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn create_returns_job_in_open_state() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteJobRepo::new(pool.clone());
    let job = repo.create(sample_new_job()).await.unwrap();
    assert!(job.id.0 > 0);
    assert_eq!(job.state, JobState::Open);
    assert_eq!(job.kind, "ingest_scan");
    assert_eq!(job.epoch, 0);
}

#[tokio::test]
async fn get_returns_created_row() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteJobRepo::new(pool.clone());
    let job = repo.create(sample_new_job()).await.unwrap();
    let fetched = repo.get(job.id).await.unwrap().expect("present");
    assert_eq!(fetched.id, job.id);
}

#[tokio::test]
async fn keyset_list_is_newest_first_and_pages_by_after_id() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteJobRepo::new(pool.clone());
    let first = repo.create(sample_new_job()).await.unwrap();
    let second = repo.create(sample_new_job()).await.unwrap();
    let third = repo.create(sample_new_job()).await.unwrap();

    // Newest first (id DESC), ADR 0031.
    let all = repo.list(JobFilter::default(), None, 10).await.unwrap();
    assert_eq!(
        all.iter().map(|j| j.id).collect::<Vec<_>>(),
        vec![third.id, second.id, first.id]
    );

    // Full page hands back a cursor; `after_id` continues past it.
    let page1 = repo.list(JobFilter::default(), None, 2).await.unwrap();
    assert_eq!(
        page1.iter().map(|j| j.id).collect::<Vec<_>>(),
        vec![third.id, second.id]
    );
    let page2 = repo
        .list(JobFilter::default(), Some(second.id.0), 2)
        .await
        .unwrap();
    assert_eq!(
        page2.iter().map(|j| j.id).collect::<Vec<_>>(),
        vec![first.id]
    );
}

#[tokio::test]
async fn keyset_list_filters_by_state() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteJobRepo::new(pool.clone());
    let open = repo.create(sample_new_job()).await.unwrap();
    let done = repo.create(sample_new_job()).await.unwrap();
    repo.succeed(done.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();

    let filter = JobFilter {
        state: Some(JobState::Open),
    };
    let rows = repo.list(filter, None, 10).await.unwrap();
    assert_eq!(rows.iter().map(|j| j.id).collect::<Vec<_>>(), vec![open.id]);
}

#[tokio::test]
async fn list_by_state_returns_open_jobs_ordered_by_priority_desc() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteJobRepo::new(pool.clone());
    let _low = repo
        .create(NewJob {
            priority: 1,
            ..sample_new_job()
        })
        .await
        .unwrap();
    let high = repo
        .create(NewJob {
            priority: 9,
            ..sample_new_job()
        })
        .await
        .unwrap();
    let _mid = repo
        .create(NewJob {
            priority: 5,
            ..sample_new_job()
        })
        .await
        .unwrap();
    let open = repo.list_by_state(JobState::Open, 10).await.unwrap();
    assert_eq!(open.len(), 3);
    assert_eq!(open[0].id, high.id, "highest priority first");
}

#[tokio::test]
async fn succeed_open_job_bumps_epoch_and_transitions() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteJobRepo::new(pool.clone());
    let job = repo.create(sample_new_job()).await.unwrap();
    let updated = repo
        .succeed(job.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    assert_eq!(updated.state, JobState::Succeeded);
    assert_eq!(updated.epoch, job.epoch + 1);
}

#[tokio::test]
async fn fail_open_job_bumps_epoch_and_transitions() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteJobRepo::new(pool.clone());
    let job = repo.create(sample_new_job()).await.unwrap();
    let updated = repo.fail(job.id, OffsetDateTime::UNIX_EPOCH).await.unwrap();
    assert_eq!(updated.state, JobState::Failed);
}

#[tokio::test]
async fn cancel_open_job_bumps_epoch_and_transitions() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteJobRepo::new(pool.clone());
    let job = repo.create(sample_new_job()).await.unwrap();
    let updated = repo
        .cancel(job.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    assert_eq!(updated.state, JobState::Cancelled);
}

#[tokio::test]
async fn succeed_rejects_terminal_job() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteJobRepo::new(pool.clone());
    let job = repo.create(sample_new_job()).await.unwrap();
    repo.succeed(job.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    // succeeded is terminal — second transition rejected.
    let err = repo
        .fail(job.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));
}
