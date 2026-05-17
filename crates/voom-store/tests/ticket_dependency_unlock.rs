#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

//! Linear ten-ticket chain plus a cycle-attempt sub-test. Exercises
//! `TicketRepo::add_dependency` cycle detection and the
//! `mark_ready_if_unblocked` downstream-unlock cascade that the
//! `ControlPlane` release use case will call after each ticket succeeds.

use std::sync::Arc;

use serde_json::json;
use time::Duration;

use voom_control_plane::ControlPlane;
use voom_core::{SystemClock, VoomError};
use voom_store::repo::leases::NewLease;
use voom_store::repo::tickets::{NewTicket, TicketRepo, TicketState};
use voom_store::repo::workers::{NewWorker, WorkerKind};
use voom_store::test_support::T0;

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
async fn linear_chain_unlocks_in_order() {
    // step-0 -> step-1 -> ... -> step-9 (10 tickets in a chain)
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "w".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
        })
        .await
        .unwrap();

    let mut ids = Vec::with_capacity(10);
    for i in 0..10 {
        let t = cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: format!("step-{i}"),
                priority: 0,
                payload: json!({}),
                max_attempts: 1,
                created_at: T0,
            })
            .await
            .unwrap();
        ids.push(t.id);
    }
    // step-i depends on step-(i-1). add_dependency does not emit an event
    // in M1, so it is acceptable to call the bare repo method directly.
    for i in 1..10 {
        cp.tickets()
            .add_dependency(ids[i], ids[i - 1])
            .await
            .unwrap();
    }

    // Only the first ticket should be promotable initially.
    let first = cp.mark_ready_if_unblocked(ids[0], T0).await.unwrap();
    assert_eq!(first.len(), 1, "step-0 promotes on its own");
    assert_eq!(
        cp.tickets().get(ids[0]).await.unwrap().unwrap().state,
        TicketState::Ready
    );
    for (i, &id) in ids.iter().enumerate().skip(1) {
        let t = cp.tickets().get(id).await.unwrap().unwrap();
        assert_eq!(
            t.state,
            TicketState::Pending,
            "ticket {i} should still be pending"
        );
        let promoted = cp.mark_ready_if_unblocked(id, T0).await.unwrap();
        assert!(promoted.is_empty(), "still blocked by upstream");
    }

    // Walk the chain to completion.
    for (i, &id) in ids.iter().enumerate() {
        let offset = Duration::seconds(i64::try_from(i).unwrap() * 10);
        let l = cp
            .acquire_lease(NewLease {
                ticket_id: id,
                worker_id: w.id,
                ttl: Duration::seconds(60),
                now: T0 + offset,
            })
            .await
            .unwrap();
        cp.release_lease(l.id, json!({}), T0 + offset + Duration::seconds(5))
            .await
            .unwrap();
        if let Some(&next_id) = ids.get(i + 1) {
            let next = cp.mark_ready_if_unblocked(next_id, T0).await.unwrap();
            assert_eq!(next.len(), 1, "ticket {} should now be ready", i + 1);
        }
    }
}

#[tokio::test]
async fn cycle_attempt_is_rejected() {
    let (cp, _tmp) = cp().await;
    let a = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: "a".to_owned(),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    let b = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: "b".to_owned(),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    let c = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: "c".to_owned(),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    cp.tickets().add_dependency(a.id, b.id).await.unwrap();
    cp.tickets().add_dependency(b.id, c.id).await.unwrap();
    let err = cp.tickets().add_dependency(c.id, a.id).await.unwrap_err();
    assert!(matches!(err, VoomError::DependencyCycle(_)));
}
