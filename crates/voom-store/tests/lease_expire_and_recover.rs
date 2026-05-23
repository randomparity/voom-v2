#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

//! Bulk lease-expiry recovery: drive 500 tickets to `Leased`, then a single
//! `expire_due` sweep past their TTLs requeues every ticket and emits one
//! `lease.expired` + one `ticket.requeued_after_lease_expiry` per row. A
//! second call against the same `now` is a no-op. Exercises the filtered
//! index `leases_held_by_expires_at` and the per-row requeue transition the
//! `ControlPlane::expire_due` use case composes.

use std::sync::Arc;

use serde_json::json;
use time::Duration;

use voom_control_plane::ControlPlane;
use voom_core::SystemClock;
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::leases::{LEASE_BATCH_LIMIT, NewLease};
use voom_store::repo::tickets::NewTicket;
use voom_store::repo::workers::{NewWorker, WorkerKind};
use voom_store::test_support::T0;

const N: usize = 500;

async fn cp() -> (ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, Arc::new(SystemClock))
        .await
        .unwrap();
    (cp, tmp)
}

#[tokio::test]
async fn expire_due_handles_bulk_overdue_leases() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "w-bulk".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    for i in 0..N {
        let t = cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: format!("k-{i}"),
                priority: 0,
                payload: json!({}),
                max_attempts: 3,
                created_at: T0,
            })
            .await
            .unwrap();
        cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
        let _l = cp
            .acquire_lease(NewLease {
                ticket_id: t.id,
                worker_id: w.id,
                ttl: Duration::seconds(10),
                now: T0,
            })
            .await
            .unwrap();
    }

    let report = cp.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    assert_eq!(report.expired_leases.len(), N);
    assert_eq!(report.requeued_tickets.len(), N);
    assert!(
        report.failed_expiries.is_empty(),
        "with max_attempts=3 and attempt=1, all should requeue"
    );

    // Second call is a no-op.
    let second = cp.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    assert!(second.expired_leases.is_empty());
    assert!(second.requeued_tickets.is_empty());
    assert!(second.failed_expiries.is_empty());

    // One lease.expired + one ticket.requeued_after_lease_expiry per row.
    let expired = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::LeaseExpired),
                ..EventFilter::default()
            },
            Page {
                limit: 1000,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let requeued = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketRequeuedAfterLeaseExpiry),
                ..EventFilter::default()
            },
            Page {
                limit: 1000,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(expired.items.len(), N);
    assert_eq!(requeued.items.len(), N);
}

/// Regression for the unbounded `IN (?,…,?)` prefetch in
/// `expire_due_in_tx`: on a restart backlog larger than the chunk size
/// (and the `SQLite` historical 999-variable floor), the per-ticket
/// attempt prefetch must still succeed by splitting into multiple
/// chunks rather than building one oversized statement that fails
/// before any lease transitions. `TICKET_ATTEMPT_CHUNK` is an internal
/// 500-row constant, so 501 tickets is the smallest size that
/// exercises a second chunk.
const N_BACKLOG: usize = 501;

#[tokio::test]
async fn expire_due_handles_backlog_above_chunk_size() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "w-backlog".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    for i in 0..N_BACKLOG {
        let t = cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: format!("k-{i}"),
                priority: 0,
                payload: json!({}),
                max_attempts: 3,
                created_at: T0,
            })
            .await
            .unwrap();
        cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
        let _l = cp
            .acquire_lease(NewLease {
                ticket_id: t.id,
                worker_id: w.id,
                ttl: Duration::seconds(10),
                now: T0,
            })
            .await
            .unwrap();
    }

    let report = cp.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    assert_eq!(report.expired_leases.len(), N_BACKLOG);
    assert_eq!(report.requeued_tickets.len(), N_BACKLOG);
    assert_eq!(report.pairs.len(), N_BACKLOG);
    assert!(
        report.failed_expiries.is_empty(),
        "with max_attempts=3 and attempt=1, all should requeue"
    );
}

/// Regression for the unbounded `expire_due` candidate scan: a backlog
/// larger than `LEASE_BATCH_LIMIT` must drain across multiple per-call
/// transactions rather than a single oversized one. Each `cp.expire_due`
/// call is capped at `LEASE_BATCH_LIMIT` candidates; callers drain by
/// re-invoking until the report is empty. If a future regression removes
/// the SQL `LIMIT` (or the drain loop), the iteration count drops to 1
/// and this test fails.
#[tokio::test]
async fn expire_due_drains_backlog_above_batch_limit() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "w-drain".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    let limit = usize::try_from(LEASE_BATCH_LIMIT).unwrap();
    let total = limit + 50;
    for i in 0..total {
        let t = cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: format!("k-{i}"),
                priority: 0,
                payload: json!({}),
                max_attempts: 3,
                created_at: T0,
            })
            .await
            .unwrap();
        cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
        let _l = cp
            .acquire_lease(NewLease {
                ticket_id: t.id,
                worker_id: w.id,
                ttl: Duration::seconds(10),
                now: T0,
            })
            .await
            .unwrap();
    }

    let now = T0 + Duration::seconds(11);
    let mut total_expired = 0usize;
    let mut total_requeued = 0usize;
    let mut iterations = 0usize;
    loop {
        let report = cp.expire_due(now).await.unwrap();
        if report.expired_leases.is_empty() {
            break;
        }
        iterations += 1;
        total_expired += report.expired_leases.len();
        total_requeued += report.requeued_tickets.len();
        assert!(
            report.failed_expiries.is_empty(),
            "with max_attempts=3 and attempt=1, all should requeue"
        );
    }
    assert_eq!(
        total_expired, total,
        "drain must process every overdue lease"
    );
    assert_eq!(total_requeued, total);
    assert!(
        iterations >= 2,
        "backlog > LEASE_BATCH_LIMIT must take at least two iterations to drain; got {iterations}"
    );

    // One lease.expired + one ticket.requeued_after_lease_expiry per row,
    // emitted across the drain iterations.
    let page_limit = u32::try_from(total).unwrap() + 10;
    let expired_events = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::LeaseExpired),
                ..EventFilter::default()
            },
            Page {
                limit: page_limit,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let requeued_events = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketRequeuedAfterLeaseExpiry),
                ..EventFilter::default()
            },
            Page {
                limit: page_limit,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(expired_events.items.len(), total);
    assert_eq!(requeued_events.items.len(), total);
}
