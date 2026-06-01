#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

//! `retire_worker` is the trust/lifecycle boundary that takes a worker
//! out of rotation. `LeaseRepo::acquire_in_tx` must reject acquires from
//! a retired worker — otherwise the FK alone (worker row exists) lets
//! retired workers continue to lease tickets. Exercises that ticket state
//! is preserved (`ready`) and no `leases` row is inserted on rejection.

use std::sync::Arc;

use serde_json::json;
use sqlx::Row;
use time::Duration;

use voom_control_plane::ControlPlane;
use voom_core::{SystemClock, TicketOperation, VoomError};
use voom_store::repo::leases::{LeaseRepo, NewLease};
use voom_store::repo::tickets::{NewTicket, TicketRepo, TicketState};
use voom_store::repo::workers::{NewWorker, WorkerKind};
use voom_store::test_support::T0;

async fn cp() -> (ControlPlane, sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool.clone(), Arc::new(SystemClock))
        .await
        .unwrap();
    (cp, pool, tmp)
}

fn ticket_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

#[tokio::test]
async fn acquire_rejects_retired_worker_and_leaves_ticket_ready() {
    let (cp, pool, _tmp) = cp().await;
    let worker = cp
        .register_worker(NewWorker {
            name: "w".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: ticket_op("k"),
            priority: 0,
            payload: json!({}),
            max_attempts: 3,
            created_at: T0,
        })
        .await
        .unwrap();
    let promoted = cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    assert_eq!(promoted.len(), 1);

    cp.retire_worker(worker.id, worker.epoch, T0 + Duration::seconds(1))
        .await
        .unwrap();

    let err = cp
        .leases()
        .acquire(NewLease {
            ticket_id: ticket.id,
            worker_id: worker.id,
            ttl: Duration::seconds(60),
            now: T0 + Duration::seconds(2),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, VoomError::Conflict(ref m) if m.contains("retired")),
        "expected Conflict citing retired worker, got: {err:?}"
    );

    // Ticket stays ready — retired-worker rejection happens before the
    // ticket UPDATE.
    let t = cp.tickets().get(ticket.id).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Ready);

    // And no leases row was inserted for this worker/ticket.
    let row = sqlx::query("SELECT COUNT(*) AS n FROM leases WHERE ticket_id = ? AND worker_id = ?")
        .bind(i64::try_from(ticket.id.0).unwrap())
        .bind(i64::try_from(worker.id.0).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    let n: i64 = row.try_get("n").unwrap();
    assert_eq!(
        n, 0,
        "no lease row should be inserted on retired-worker rejection"
    );
}

#[tokio::test]
async fn acquire_rejects_unknown_worker() {
    let (cp, _pool, _tmp) = cp().await;
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: ticket_op("k"),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    let promoted = cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    assert_eq!(promoted.len(), 1);

    let err = cp
        .leases()
        .acquire(NewLease {
            ticket_id: ticket.id,
            worker_id: voom_core::WorkerId(9_999_999),
            ttl: Duration::seconds(60),
            now: T0 + Duration::seconds(1),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, VoomError::NotFound(ref m) if m.contains("worker")),
        "expected NotFound citing worker, got: {err:?}"
    );
}
