use super::*;

use time::{Duration as TDuration, OffsetDateTime};
use voom_core::{FailureClass, VoomError};
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventPage, EventRepo, Page};
use voom_store::repo::leases::NewLease;
use voom_store::repo::tickets::{TicketRepo, TicketState};
use voom_store::repo::workers::{NewWorker, WorkerKind};

use crate::cases::cp;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

fn ticket_with_max_attempts(kind: &str, max_attempts: u32) -> NewTicket {
    NewTicket {
        job_id: None,
        kind: kind.to_owned(),
        priority: 0,
        payload: serde_json::json!({}),
        max_attempts,
        created_at: T0,
    }
}

fn ticket(kind: &str) -> NewTicket {
    ticket_with_max_attempts(kind, 1)
}

fn worker(name: &str) -> NewWorker {
    NewWorker {
        name: name.to_owned(),
        kind: WorkerKind::Synthetic,
        registered_at: T0,
    }
}

#[tokio::test]
async fn create_ticket_emits_ticket_created() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("test.noop")).await.unwrap();
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketCreated),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].envelope.subject_id, Some(t.id.0));
}

#[tokio::test]
async fn mark_ready_emits_one_ticket_ready_per_promoted() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("test.noop")).await.unwrap();
    let promoted = cp
        .mark_ready_if_unblocked(t.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    assert_eq!(promoted.len(), 1);
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketReady),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
}

#[tokio::test]
async fn mark_ready_emits_nothing_when_not_eligible() {
    let (cp, _tmp) = cp().await;
    let parent = cp.create_ticket(ticket("parent")).await.unwrap();
    let child = cp.create_ticket(ticket("child")).await.unwrap();
    cp.tickets()
        .add_dependency(child.id, parent.id)
        .await
        .unwrap();
    let promoted = cp
        .mark_ready_if_unblocked(child.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    assert!(promoted.is_empty());
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketReady),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(page.items.is_empty());
}

#[tokio::test]
async fn pre_lease_no_eligible_worker_requeues_without_creating_lease() {
    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(ticket_with_max_attempts("test.noop", 3))
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();

    let now = T0 + TDuration::seconds(5);
    let outcome = cp
        .record_pre_lease_ticket_failure(t.id, FailureClass::NoEligibleWorker, now)
        .await
        .unwrap();

    assert!(!outcome.terminal);
    assert_eq!(outcome.ticket.state, TicketState::Ready);
    assert_eq!(outcome.ticket.attempt, 1);
    assert!(outcome.ticket.next_eligible_at >= now);
    assert!(outcome.ticket.next_eligible_at <= now + TDuration::seconds(10));
    assert_eq!(lease_count(&cp).await, 0);
    assert_eq!(event_count(&cp, EventKind::LeaseAcquired).await, 0);
    assert_eq!(event_count(&cp, EventKind::LeaseReleased).await, 0);

    let page = events(&cp, EventKind::TicketFailedRetriable).await;
    assert_eq!(page.items.len(), 1);
    let voom_events::Event::TicketFailedRetriable(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketFailedRetriable payload");
    };
    assert_eq!(payload.ticket_id, t.id.0);
    assert_eq!(payload.attempt, 1);
    assert_eq!(payload.max_attempts, 3);
    assert_eq!(payload.class, FailureClass::NoEligibleWorker);
    assert_eq!(
        payload.reason,
        "no eligible worker before lease acquisition"
    );
    assert_eq!(payload.next_eligible_at, outcome.ticket.next_eligible_at);
    assert_eq!(event_count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

#[tokio::test]
async fn pre_lease_ambiguous_worker_selection_terminal_fails_immediately() {
    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(ticket_with_max_attempts("test.noop", 3))
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();

    let outcome = cp
        .record_pre_lease_ticket_failure(
            t.id,
            FailureClass::AmbiguousWorkerSelection,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap();

    assert!(outcome.terminal);
    assert_eq!(outcome.ticket.state, TicketState::Failed);
    assert_eq!(outcome.ticket.attempt, 1);
    assert_eq!(lease_count(&cp).await, 0);
    assert_eq!(event_count(&cp, EventKind::TicketFailedRetriable).await, 0);

    let page = events(&cp, EventKind::TicketFailedTerminal).await;
    assert_eq!(page.items.len(), 1);
    let voom_events::Event::TicketFailedTerminal(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketFailedTerminal payload");
    };
    assert_eq!(payload.ticket_id, t.id.0);
    assert_eq!(payload.attempt, 1);
    assert_eq!(payload.max_attempts, 3);
    assert_eq!(payload.class, FailureClass::AmbiguousWorkerSelection);
    assert_eq!(
        payload.reason,
        "ambiguous worker selection before lease acquisition"
    );
    assert_eq!(payload.issue_id, None);
}

#[tokio::test]
async fn pre_lease_no_eligible_worker_terminal_fails_when_attempts_exhausted() {
    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(ticket_with_max_attempts("test.noop", 1))
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();

    let outcome = cp
        .record_pre_lease_ticket_failure(
            t.id,
            FailureClass::NoEligibleWorker,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap();

    assert!(outcome.terminal);
    assert_eq!(outcome.ticket.state, TicketState::Failed);
    assert_eq!(outcome.ticket.attempt, 1);
    assert_eq!(event_count(&cp, EventKind::TicketFailedRetriable).await, 0);

    let page = events(&cp, EventKind::TicketFailedTerminal).await;
    assert_eq!(page.items.len(), 1);
    let voom_events::Event::TicketFailedTerminal(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketFailedTerminal payload");
    };
    assert_eq!(payload.ticket_id, t.id.0);
    assert_eq!(payload.attempt, 1);
    assert_eq!(payload.max_attempts, 1);
    assert_eq!(payload.class, FailureClass::NoEligibleWorker);
}

#[tokio::test]
async fn pre_lease_failure_rejects_non_ready_ticket_without_mutation() {
    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(ticket_with_max_attempts("test.noop", 3))
        .await
        .unwrap();

    let err = cp
        .record_pre_lease_ticket_failure(
            t.id,
            FailureClass::NoEligibleWorker,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)));
    let unchanged = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(unchanged.state, TicketState::Pending);
    assert_eq!(unchanged.attempt, 0);
    assert_eq!(event_count(&cp, EventKind::TicketFailedRetriable).await, 0);
    assert_eq!(event_count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

#[tokio::test]
async fn pre_lease_failure_rejects_ticket_with_active_lease() {
    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(ticket_with_max_attempts("test.noop", 3))
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = cp.register_worker(worker("alpha")).await.unwrap();
    cp.acquire_lease(NewLease {
        ticket_id: t.id,
        worker_id: w.id,
        ttl: TDuration::seconds(60),
        now: T0,
    })
    .await
    .unwrap();

    let err = cp
        .record_pre_lease_ticket_failure(
            t.id,
            FailureClass::NoEligibleWorker,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)));
    assert_eq!(event_count(&cp, EventKind::TicketFailedRetriable).await, 0);
    assert_eq!(event_count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

#[tokio::test]
async fn pre_lease_failure_rejects_unsupported_failure_class_without_mutation() {
    let (cp, _tmp) = cp().await;
    let t = cp
        .create_ticket(ticket_with_max_attempts("test.noop", 3))
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();

    let err = cp
        .record_pre_lease_ticket_failure(
            t.id,
            FailureClass::WorkerTimeout,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Config(_)));
    let unchanged = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(unchanged.state, TicketState::Ready);
    assert_eq!(unchanged.attempt, 0);
    assert_eq!(event_count(&cp, EventKind::TicketFailedRetriable).await, 0);
    assert_eq!(event_count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

async fn events(cp: &crate::ControlPlane, kind: EventKind) -> EventPage {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap()
}

async fn event_count(cp: &crate::ControlPlane, kind: EventKind) -> usize {
    events(cp, kind).await.items.len()
}

async fn lease_count(cp: &crate::ControlPlane) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(&cp.pool)
        .await
        .unwrap()
}
