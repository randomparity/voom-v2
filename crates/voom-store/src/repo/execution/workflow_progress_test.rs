use super::*;

use crate::test_support::fresh_initialized_pool_at;

async fn fixture() -> (
    SqliteWorkflowProgressRepo,
    sqlx::SqlitePool,
    voom_test_support::TempDatabase,
) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, kind, state, priority, created_at, updated_at) \
         VALUES (1, 'workflow', 'open', 0, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO file_assets (id, created_at) VALUES (1, '1970-01-01T00:00:00Z')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO file_versions \
         (id, file_asset_id, content_hash, size_bytes, produced_by, created_at) \
         VALUES (1, 1, 'hash', 1, 'ingest', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_file_run_starts \
         (job_id, branch_id, starting_file_version_id, starting_phase_ordinal) \
         VALUES (1, 'alpha', 1, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_file_progress \
         (job_id, branch_id, input_ordinal, admission_tier, state, next_phase_ordinal) \
         VALUES (1, 'alpha', 7, 1, 'pending', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    (SqliteWorkflowProgressRepo::new(pool.clone()), pool, tmp)
}

#[tokio::test]
async fn branch_for_input_ordinal_is_job_scoped_and_optional() {
    let (repo, _pool, _tmp) = fixture().await;

    assert_eq!(
        repo.branch_for_input_ordinal(JobId(1), 7).await.unwrap(),
        Some("alpha".to_owned())
    );
    assert_eq!(
        repo.branch_for_input_ordinal(JobId(1), 8).await.unwrap(),
        None
    );
    assert_eq!(
        repo.branch_for_input_ordinal(JobId(2), 7).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn rollback_leaves_branch_projection_unchanged() {
    let (repo, pool, _tmp) = fixture().await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE workflow_file_progress SET input_ordinal = 8 WHERE job_id = 1")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(
        repo.branch_for_input_ordinal(JobId(1), 7).await.unwrap(),
        Some("alpha".to_owned())
    );
}
