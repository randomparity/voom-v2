use super::*;

use std::sync::{Arc, Mutex};

use time::OffsetDateTime;
use voom_core::clock_test_support::ManualClock;
use voom_core::rng_test_support::FrozenRng;
use voom_store::repo::workers::{NewGrant, NewWorker, WorkerKind, WorkerStatus};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn ensure_builtin_verify_artifact_worker_reuses_existing_live_row() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;

    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let first = ensure_builtin_verify_artifact_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    let second = ensure_builtin_verify_artifact_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(first.name, "builtin.verify_artifact");
    assert_eq!(first.status, WorkerStatus::Registered);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workers WHERE name = ?")
        .bind("builtin.verify_artifact")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(count, 1);

    let eligibility = cp
        .workers()
        .operation_eligibility(
            first.id,
            &TicketOperation::from(OperationKind::VerifyArtifact),
        )
        .await
        .unwrap();
    assert!(eligibility.has_capability);
    assert!(eligibility.has_grant);
    assert!(!eligibility.is_denied);
    assert_eq!(eligibility.artifact_access, vec!["local_path"]);
}

#[tokio::test]
async fn concurrent_first_bootstrap_reuses_one_builtin_worker_row() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let left_cp = cp.clone();
    let right_cp = cp.clone();

    let (left, right) = tokio::join!(
        async move {
            let mut tx = left_cp.pool_for_test().begin().await.unwrap();
            let worker = ensure_builtin_verify_artifact_worker_in_tx(&left_cp, &mut tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            worker
        },
        async move {
            let mut tx = right_cp.pool_for_test().begin().await.unwrap();
            let worker = ensure_builtin_verify_artifact_worker_in_tx(&right_cp, &mut tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            worker
        }
    );

    assert_eq!(left.id, right.id);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workers WHERE name = ?")
        .bind("builtin.verify_artifact")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn retired_builtin_verify_artifact_worker_fails_loudly() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;

    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let worker = ensure_builtin_verify_artifact_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    cp.workers()
        .retire(worker.id, worker.epoch, T0 + time::Duration::seconds(1))
        .await
        .unwrap();

    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let err = ensure_builtin_verify_artifact_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn conflicting_builtin_verify_artifact_worker_shape_fails_loudly() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    cp.workers()
        .register(NewWorker {
            name: "builtin.verify_artifact".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();

    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let err = ensure_builtin_verify_artifact_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn denied_builtin_verify_artifact_execute_grant_fails_loudly() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;

    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let worker = ensure_builtin_verify_artifact_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    cp.workers()
        .record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec![TicketOperation::from(OperationKind::VerifyArtifact)],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: vec![TicketOperation::from(OperationKind::VerifyArtifact)],
            max_parallel: serde_json::json!({}),
        })
        .await
        .unwrap();

    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let err = ensure_builtin_verify_artifact_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

async fn cp_with_manual_clock(
    now: OffsetDateTime,
) -> (crate::ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(now));
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        clock,
        Arc::new(Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, tmp)
}
