use super::*;

use std::sync::{Arc, Mutex};

use time::OffsetDateTime;
use voom_core::clock_test_support::ManualClock;
use voom_core::rng_test_support::FrozenRng;
use voom_store::repo::execution::workers::{NewGrant, NewWorker, WorkerKind, WorkerStatus};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn ensure_builtin_ffprobe_worker_creates_unique_row_and_reuses_it() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;

    let mut tx = crate::cases::begin_immediate_tx(cp.pool_for_test())
        .await
        .unwrap();
    let first = ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    let second = ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(first.id, second.id);
    assert_ne!(first.name, "builtin.ffprobe");
    assert!(first.name.starts_with("builtin.ffprobe-"));
    assert_eq!(first.status, WorkerStatus::Registered);

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workers WHERE name LIKE 'builtin.ffprobe%'")
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(count, 1);

    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE subject_type = 'worker' AND subject_id = ?",
    )
    .bind(i64::try_from(first.id.0).unwrap())
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(event_count, 1);
}

#[tokio::test]
async fn retired_builtin_ffprobe_worker_is_replaced_by_unique_live_row() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;

    let mut tx = crate::cases::begin_immediate_tx(cp.pool_for_test())
        .await
        .unwrap();
    let worker = ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    cp.workers()
        .retire(worker.id, worker.epoch, T0 + time::Duration::seconds(1))
        .await
        .unwrap();

    let mut tx = crate::cases::begin_immediate_tx(cp.pool_for_test())
        .await
        .unwrap();
    let replacement = ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_ne!(replacement.id, worker.id);
    assert!(replacement.name.starts_with("builtin.ffprobe-"));
    assert_eq!(replacement.status, WorkerStatus::Registered);
}

#[tokio::test]
async fn conflicting_builtin_ffprobe_worker_shape_fails_loudly() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    cp.workers()
        .register(NewWorker {
            name: "builtin.ffprobe".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();

    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let err = ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn sole_live_unique_builtin_ffprobe_worker_is_adopted() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let existing = cp
        .workers()
        .register(NewWorker {
            name: "builtin.ffprobe-existing".to_owned(),
            kind: WorkerKind::Local,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();

    let mut tx = crate::cases::begin_immediate_tx(cp.pool_for_test())
        .await
        .unwrap();
    let adopted = ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(adopted.id, existing.id);
}

#[tokio::test]
async fn multiple_live_builtin_ffprobe_workers_fail_loudly() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    for name in ["builtin.ffprobe", "builtin.ffprobe-other"] {
        cp.workers()
            .register(NewWorker {
                name: name.to_owned(),
                kind: WorkerKind::Local,
                registered_at: T0,
                node_id: None,
            })
            .await
            .unwrap();
    }

    let mut tx = crate::cases::begin_immediate_tx(cp.pool_for_test())
        .await
        .unwrap();
    let err = ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn concurrent_absent_bootstraps_converge_on_one_live_worker() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;

    let (first, second) = tokio::join!(
        ensure_with_immediate_transaction(&cp),
        ensure_with_immediate_transaction(&cp)
    );
    let first = first.unwrap();
    let second = second.unwrap();

    assert_eq!(first.id, second.id);
    let live_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workers \
         WHERE name LIKE 'builtin.ffprobe-%' AND status IN ('registered', 'active')",
    )
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(live_count, 1);
}

#[tokio::test]
async fn denied_builtin_ffprobe_execute_grant_fails_loudly() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;

    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let worker = ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    cp.workers()
        .record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec![TicketOperation::from(OperationKind::ProbeFile)],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: vec![TicketOperation::from(OperationKind::ProbeFile)],
            max_parallel: serde_json::json!({}),
        })
        .await
        .unwrap();

    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let err = ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

async fn ensure_with_immediate_transaction(cp: &crate::ControlPlane) -> Result<Worker, VoomError> {
    let mut tx = crate::cases::begin_immediate_tx(cp.pool_for_test()).await?;
    let worker = ensure_builtin_ffprobe_worker_in_tx(cp, &mut tx).await?;
    tx.commit()
        .await
        .map_err(|error| VoomError::database_context("test bootstrap commit", error))?;
    Ok(worker)
}

async fn cp_with_manual_clock(
    now: OffsetDateTime,
) -> (crate::ControlPlane, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
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
