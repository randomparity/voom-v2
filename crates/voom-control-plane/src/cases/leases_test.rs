use super::*;

use time::{Duration as TDuration, OffsetDateTime};
use voom_core::TicketId;
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::tickets::NewTicket;
use voom_store::repo::workers::{NewWorker, WorkerKind};

use crate::cases::cp;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

fn ticket(kind: &str, max_attempts: u32) -> NewTicket {
    NewTicket {
        job_id: None,
        kind: kind.to_owned(),
        priority: 0,
        payload: serde_json::json!({}),
        max_attempts,
        created_at: T0,
    }
}

fn worker(name: &str) -> NewWorker {
    NewWorker {
        name: name.to_owned(),
        kind: WorkerKind::Synthetic,
        registered_at: T0,
    }
}

async fn count(cp: &crate::ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 100,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

#[tokio::test]
async fn acquire_lease_emits_lease_acquired_and_ticket_leased() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseAcquired).await, 1);
    assert_eq!(count(&cp, EventKind::TicketLeased).await, 1);
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketLeased),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let voom_events::Event::TicketLeased(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketLeased payload");
    };
    assert_eq!(payload.attempt, 1, "first dispatch bumps attempt to 1");
    assert_eq!(payload.lease_id, lease.id.0);
}

#[tokio::test]
async fn release_lease_emits_lease_released_and_ticket_succeeded() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 1)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.release_lease(lease.id, serde_json::json!({}), T0 + TDuration::seconds(5))
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(count(&cp, EventKind::TicketSucceeded).await, 1);
}

#[tokio::test]
async fn fail_lease_retriable_emits_lease_released_and_ticket_failed_retriable() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 3)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.fail_lease(
        lease.id,
        "transient".to_owned(),
        true,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedRetriable).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

#[tokio::test]
async fn fail_lease_terminal_emits_lease_released_and_ticket_failed_terminal() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 1)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.fail_lease(
        lease.id,
        "fatal".to_owned(),
        true,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedRetriable).await, 0);
}

#[tokio::test]
async fn expire_due_emits_paired_events_requeued() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 3)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let _lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(30),
            now: T0,
        })
        .await
        .unwrap();
    let report = cp.expire_due(T0 + TDuration::seconds(60)).await.unwrap();
    assert_eq!(report.pairs.len(), 1);
    assert_eq!(report.requeued_tickets, vec![t.id]);
    assert!(report.failed_tickets.is_empty());
    assert_eq!(count(&cp, EventKind::LeaseExpired).await, 1);
    assert_eq!(
        count(&cp, EventKind::TicketRequeuedAfterLeaseExpiry).await,
        1
    );
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

#[tokio::test]
async fn expire_due_emits_paired_events_terminal() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 1)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let _lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(30),
            now: T0,
        })
        .await
        .unwrap();
    let report = cp.expire_due(T0 + TDuration::seconds(60)).await.unwrap();
    assert_eq!(report.pairs.len(), 1);
    assert_eq!(report.failed_tickets, vec![t.id]);
    assert_eq!(count(&cp, EventKind::LeaseExpired).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 1);
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketFailedTerminal),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let voom_events::Event::TicketFailedTerminal(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketFailedTerminal payload");
    };
    assert!(payload.reason.contains("lease expired"));
    let _: TicketId = t.id;
}

#[tokio::test]
async fn force_release_with_requeue_emits_lease_force_released_and_ticket_ready() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let ready_before = count(&cp, EventKind::TicketReady).await;
    cp.force_release_lease(
        lease.id,
        "operator".to_owned(),
        "manual cleanup".to_owned(),
        true,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseForceReleased).await, 1);
    assert_eq!(count(&cp, EventKind::TicketReady).await, ready_before + 1);
}

#[tokio::test]
async fn force_release_without_requeue_emits_lease_force_released_and_ticket_failed_terminal() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.force_release_lease(
        lease.id,
        "operator".to_owned(),
        "manual cleanup".to_owned(),
        false,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseForceReleased).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 1);
}
