#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

//! Drives the §7.5 worked example through the `ControlPlane` use cases
//! added in Task 14. Every state transition is composed by the use case
//! (repo `_in_tx` write + matching `EventRepo::append_in_tx` in one tx),
//! so the test only asserts on the resulting ticket state and event-row
//! counts — it never appends events itself.

use std::sync::Arc;

use serde_json::json;
use time::Duration;

use std::sync::Mutex;

use voom_control_plane::ControlPlane;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{FailureClass, SystemClock, TicketOperation};
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::leases::{LeaseRepo, NewLease};
use voom_store::repo::tickets::{NewTicket, TicketRepo, TicketState};
use voom_store::repo::workers::{NewWorker, WorkerKind};
use voom_store::test_support::T0;

async fn cp() -> (ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    // FrozenRng(u32::MAX) pins jitter to the cap, making
    // `next_eligible_at` arithmetic deterministic (window equals
    // `min(cap, base * 2^attempt)` seconds, e.g. 10s at attempt=1).
    let rng = Arc::new(Mutex::new(FrozenRng::new(u32::MAX)));
    let cp = ControlPlane::open_with_pool_and_rng(pool, Arc::new(SystemClock), rng)
        .await
        .unwrap();
    (cp, tmp)
}

async fn count_kind(cp: &ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 1000,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

fn ticket_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

#[tokio::test]
async fn happy_path_ready_leased_succeeded_with_events() {
    let (cp, _tmp) = cp().await;

    let t = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: ticket_op("ingest.scan"),
            priority: 0,
            payload: json!({}),
            max_attempts: 2,
            created_at: T0,
        })
        .await
        .unwrap();
    let w = cp
        .register_worker(NewWorker {
            name: "w-happy".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();

    let promoted = cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    assert_eq!(promoted.len(), 1);

    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    cp.heartbeat_lease(lease.id, Duration::seconds(60), T0 + Duration::seconds(20))
        .await
        .unwrap();
    cp.heartbeat_lease(lease.id, Duration::seconds(60), T0 + Duration::seconds(40))
        .await
        .unwrap();

    cp.release_lease(lease.id, json!({"out": 42}), T0 + Duration::seconds(50))
        .await
        .unwrap();

    assert_eq!(
        cp.tickets().get(t.id).await.unwrap().unwrap().state,
        TicketState::Succeeded
    );
    assert_eq!(count_kind(&cp, EventKind::TicketCreated).await, 1);
    assert_eq!(count_kind(&cp, EventKind::TicketReady).await, 1);
    assert_eq!(count_kind(&cp, EventKind::TicketLeased).await, 1);
    assert_eq!(count_kind(&cp, EventKind::LeaseAcquired).await, 1);
    assert_eq!(count_kind(&cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(count_kind(&cp, EventKind::TicketSucceeded).await, 1);
}

#[tokio::test]
async fn max_attempts_2_via_fail_retriable_yields_two_dispatched_attempts() {
    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: ticket_op("test.noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 2,
            created_at: T0,
        })
        .await
        .unwrap();
    let w = cp
        .register_worker(NewWorker {
            name: "w-a".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();

    // attempt 1
    let l1 = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.fail_lease(
        l1.id,
        "transient".to_owned(),
        FailureClass::WorkerTimeout,
        T0 + Duration::seconds(5),
    )
    .await
    .unwrap();
    let after1 = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(after1.state, TicketState::Ready);
    assert_eq!(after1.attempt, 1);

    // attempt 2: ceiling jitter (cp uses FrozenRng(u32::MAX)) makes
    // default_backoff(attempt=1) = min(cap=300s, base=5s * 2^1) = 10s.
    // Advance `now` past next_eligible_at.
    let now2 = after1.next_eligible_at + Duration::seconds(1);
    let l2 = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(60),
            now: now2,
        })
        .await
        .unwrap();
    cp.fail_lease(
        l2.id,
        "still bad".to_owned(),
        FailureClass::WorkerTimeout,
        now2 + Duration::seconds(5),
    )
    .await
    .unwrap();
    let after2 = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(
        after2.state,
        TicketState::Failed,
        "must terminate after 2 dispatches"
    );
    assert_eq!(after2.attempt, 2);

    assert_eq!(count_kind(&cp, EventKind::TicketFailedRetriable).await, 1);
    assert_eq!(count_kind(&cp, EventKind::TicketFailedTerminal).await, 1);
}

#[tokio::test]
async fn max_attempts_2_via_expire_due_yields_two_dispatched_attempts() {
    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: ticket_op("test.noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 2,
            created_at: T0,
        })
        .await
        .unwrap();
    let w = cp
        .register_worker(NewWorker {
            name: "w-b".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();

    // attempt 1
    let _l1 = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(10),
            now: T0,
        })
        .await
        .unwrap();
    cp.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    assert_eq!(
        cp.tickets().get(t.id).await.unwrap().unwrap().state,
        TicketState::Ready
    );

    // attempt 2 — terminal
    let _l2 = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(10),
            now: T0 + Duration::seconds(20),
        })
        .await
        .unwrap();
    cp.expire_due(T0 + Duration::seconds(31)).await.unwrap();
    let after = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(after.state, TicketState::Failed);
    assert_eq!(after.attempt, 2);

    assert_eq!(count_kind(&cp, EventKind::LeaseExpired).await, 2);
    assert_eq!(
        count_kind(&cp, EventKind::TicketRequeuedAfterLeaseExpiry).await,
        1
    );
    assert_eq!(count_kind(&cp, EventKind::TicketFailedTerminal).await, 1);
}

#[tokio::test]
async fn max_attempts_3_mixed_fail_and_expire_due() {
    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: ticket_op("test.noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 3,
            created_at: T0,
        })
        .await
        .unwrap();
    let w = cp
        .register_worker(NewWorker {
            name: "w-c".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();

    // attempt 1 — fail retriable
    let l1 = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.fail_lease(
        l1.id,
        "x".to_owned(),
        FailureClass::WorkerTimeout,
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    let now2 = cp
        .tickets()
        .get(t.id)
        .await
        .unwrap()
        .unwrap()
        .next_eligible_at
        + Duration::seconds(1);

    // attempt 2 — expire
    let _l2 = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(10),
            now: now2,
        })
        .await
        .unwrap();
    cp.expire_due(now2 + Duration::seconds(11)).await.unwrap();
    assert_eq!(
        cp.tickets().get(t.id).await.unwrap().unwrap().state,
        TicketState::Ready
    );

    // attempt 3 — terminal via fail
    let now3 = now2 + Duration::seconds(60);
    let l3 = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(60),
            now: now3,
        })
        .await
        .unwrap();
    cp.fail_lease(
        l3.id,
        "final".to_owned(),
        FailureClass::WorkerTimeout,
        now3 + Duration::seconds(1),
    )
    .await
    .unwrap();
    let after = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(after.state, TicketState::Failed);
    assert_eq!(after.attempt, 3);
}

#[tokio::test]
async fn force_release_requeue_rejects_when_exhausted() {
    // §13 stranding regression: max_attempts=1. acquire consumes the
    // only attempt. force_release(_, _, _, true) must return Conflict
    // with NO side effects on the lease, ticket, or event log. The
    // operator must explicitly retry with also_requeue=false if they
    // intend a terminal force-release.
    use voom_core::VoomError;

    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: ticket_op("test.noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp
        .register_worker(NewWorker {
            name: "w-strand".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let force_released_before = count_kind(&cp, EventKind::LeaseForceReleased).await;
    let requeued_before = count_kind(&cp, EventKind::TicketRequeuedAfterForceRelease).await;
    let terminal_before = count_kind(&cp, EventKind::TicketFailedTerminal).await;

    let err = cp
        .force_release_lease(
            lease.id,
            "operator".to_owned(),
            "manual cleanup".to_owned(),
            true,
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");

    let lease_after = cp.leases().get(lease.id).await.unwrap().unwrap();
    assert_eq!(
        lease_after.state,
        voom_store::repo::leases::LeaseState::Held,
        "rejected force_release must leave the lease held"
    );
    let ticket_after = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(ticket_after.state, TicketState::Leased);
    assert_eq!(
        count_kind(&cp, EventKind::LeaseForceReleased).await,
        force_released_before
    );
    assert_eq!(
        count_kind(&cp, EventKind::TicketRequeuedAfterForceRelease).await,
        requeued_before
    );
    assert_eq!(
        count_kind(&cp, EventKind::TicketFailedTerminal).await,
        terminal_before
    );
}
