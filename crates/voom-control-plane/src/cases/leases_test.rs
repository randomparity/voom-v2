use super::*;

use time::{Duration as TDuration, OffsetDateTime};
use voom_core::{FailureClass, TicketId, VoomError};
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::tickets::{NewTicket, TicketRepo, TicketState};
use voom_store::repo::workers::{NewWorker, WorkerKind};

use crate::cases::{count, cp};

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
        node_id: None,
    }
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
        FailureClass::WorkerTimeout,
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
    // max_attempts=1: a single retriable failure exhausts the budget,
    // so the case handler emits TicketFailedTerminal even though the
    // class is retriable. Reuses the same call shape as the retriable
    // happy path.
    cp.fail_lease(
        lease.id,
        "fatal".to_owned(),
        FailureClass::WorkerTimeout,
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
    assert!(report.failed_expiries.is_empty());
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
    assert_eq!(report.failed_expiries.len(), 1);
    assert_eq!(report.failed_expiries[0].ticket_id, t.id);
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
async fn force_release_with_requeue_emits_ticket_requeued_after_force_release_when_attempts_remain()
{
    // max_attempts=2: after acquire, attempts remain (1 < 2).
    // also_requeue=true → ticket back to ready, and the case handler
    // emits TicketRequeuedAfterForceRelease (not TicketReady — the
    // distinct kind lets audit tell operator-driven requeue apart from
    // dependency-driven readiness).
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
    let outcome = cp
        .force_release_lease(
            lease.id,
            "operator".to_owned(),
            "manual cleanup".to_owned(),
            true,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap();
    assert!(outcome.ticket_requeued);
    assert_eq!(count(&cp, EventKind::LeaseForceReleased).await, 1);
    assert_eq!(
        count(&cp, EventKind::TicketRequeuedAfterForceRelease).await,
        1
    );
    assert_eq!(
        count(&cp, EventKind::TicketReady).await,
        ready_before,
        "force-release uses the dedicated event kind, not TicketReady"
    );
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

#[tokio::test]
async fn force_release_with_requeue_rejects_when_attempts_exhausted() {
    // §13 stranding regression. max_attempts=1: acquire consumes the
    // only attempt. Operator asks for requeue, but no attempts remain.
    // The repo now returns VoomError::Conflict with NO side effects on
    // the lease, ticket, or event log — the caller must retry with
    // also_requeue=false if they intend a terminal force-release.
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
    let force_released_before = count(&cp, EventKind::LeaseForceReleased).await;
    let requeued_before = count(&cp, EventKind::TicketRequeuedAfterForceRelease).await;
    let terminal_before = count(&cp, EventKind::TicketFailedTerminal).await;
    let err = cp
        .force_release_lease(
            lease.id,
            "operator".to_owned(),
            "manual cleanup".to_owned(),
            true,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    // No side effects: lease still held, ticket still leased, no events.
    let lease_after = cp.leases().get(lease.id).await.unwrap().unwrap();
    assert_eq!(
        lease_after.state,
        voom_store::repo::leases::LeaseState::Held,
        "rejected force_release must leave the lease held"
    );
    let ticket_after = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(
        ticket_after.state,
        TicketState::Leased,
        "rejected force_release must leave the ticket leased"
    );
    assert_eq!(
        count(&cp, EventKind::LeaseForceReleased).await,
        force_released_before
    );
    assert_eq!(
        count(&cp, EventKind::TicketRequeuedAfterForceRelease).await,
        requeued_before
    );
    assert_eq!(
        count(&cp, EventKind::TicketFailedTerminal).await,
        terminal_before
    );
    // The same fixture with also_requeue=false succeeds: lease force-released,
    // ticket parked in failed, single LeaseForceReleased + single
    // TicketFailedTerminal event.
    let outcome = cp
        .force_release_lease(
            lease.id,
            "operator".to_owned(),
            "manual cleanup".to_owned(),
            false,
            T0 + TDuration::seconds(6),
        )
        .await
        .unwrap();
    assert!(!outcome.ticket_requeued);
    assert_eq!(
        count(&cp, EventKind::LeaseForceReleased).await,
        force_released_before + 1
    );
    assert_eq!(
        count(&cp, EventKind::TicketFailedTerminal).await,
        terminal_before + 1
    );
    let _: TicketId = t.id;
}

#[tokio::test]
async fn release_lease_promotes_dependent_and_emits_ticket_ready() {
    // parent -> child. Releasing parent must promote child to ready and
    // emit exactly one ticket.ready for child.id.
    let (cp, _tmp) = cp().await;
    let parent = cp.create_ticket(ticket("parent", 1)).await.unwrap();
    let child = cp.create_ticket(ticket("child", 1)).await.unwrap();
    cp.tickets()
        .add_dependency(child.id, parent.id)
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(parent.id, T0).await.unwrap();
    // child cannot promote yet — parent is not succeeded.
    let none = cp.mark_ready_if_unblocked(child.id, T0).await.unwrap();
    assert!(none.is_empty(), "child must stay pending while parent runs");

    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: parent.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let ready_before = count(&cp, EventKind::TicketReady).await;
    cp.release_lease(lease.id, serde_json::json!({}), T0 + TDuration::seconds(5))
        .await
        .unwrap();

    let child_after = cp.tickets().get(child.id).await.unwrap().unwrap();
    assert_eq!(
        child_after.state,
        TicketState::Ready,
        "child must be promoted to ready when parent succeeds"
    );
    assert_eq!(
        count(&cp, EventKind::TicketReady).await,
        ready_before + 1,
        "exactly one ticket.ready emitted for the promoted child"
    );

    // Verify the emitted ticket.ready payload references the child.
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketReady),
                subject_id: Some(child.id.0),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1, "exactly one ticket.ready for child");
    let voom_events::Event::TicketReady(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketReady payload");
    };
    assert_eq!(payload.ticket_id, child.id.0);
}

#[tokio::test]
async fn release_lease_does_not_promote_child_with_outstanding_parent() {
    // Diamond: child depends on parent_a AND parent_b. Releasing parent_a
    // alone must not promote child (parent_b still leased), so no
    // ticket.ready is emitted for child.
    let (cp, _tmp) = cp().await;
    let parent_a = cp.create_ticket(ticket("parent_a", 1)).await.unwrap();
    let parent_b = cp.create_ticket(ticket("parent_b", 1)).await.unwrap();
    let child = cp.create_ticket(ticket("child", 1)).await.unwrap();
    cp.tickets()
        .add_dependency(child.id, parent_a.id)
        .await
        .unwrap();
    cp.tickets()
        .add_dependency(child.id, parent_b.id)
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(parent_a.id, T0).await.unwrap();
    cp.mark_ready_if_unblocked(parent_b.id, T0).await.unwrap();

    let w = cp.register_worker(worker("alpha")).await.unwrap();
    let lease_a = cp
        .acquire_lease(NewLease {
            ticket_id: parent_a.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // parent_b is also leased so it cannot succeed.
    let _lease_b = cp
        .acquire_lease(NewLease {
            ticket_id: parent_b.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    let ready_before = count(&cp, EventKind::TicketReady).await;
    cp.release_lease(
        lease_a.id,
        serde_json::json!({}),
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();

    let child_after = cp.tickets().get(child.id).await.unwrap().unwrap();
    assert_eq!(
        child_after.state,
        TicketState::Pending,
        "child must stay pending while a parent is still outstanding"
    );
    assert_eq!(
        count(&cp, EventKind::TicketReady).await,
        ready_before,
        "no ticket.ready when a dependent remains blocked"
    );
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

#[tokio::test]
async fn force_release_lease_rejects_empty_actor() {
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
    let before = count(&cp, EventKind::LeaseForceReleased).await;
    let err = cp
        .force_release_lease(
            lease.id,
            String::new(),
            "manual cleanup".to_owned(),
            false,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
    // Validation runs before the tx — the lease must still be held and
    // no audit event row must have been written.
    assert_eq!(count(&cp, EventKind::LeaseForceReleased).await, before);
    let still = cp.leases().get(lease.id).await.unwrap().unwrap();
    assert_eq!(still.state, voom_store::repo::leases::LeaseState::Held);
}

#[tokio::test]
async fn force_release_lease_rejects_whitespace_reason() {
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
    let before = count(&cp, EventKind::LeaseForceReleased).await;
    let err = cp
        .force_release_lease(
            lease.id,
            "operator".to_owned(),
            "   \t\n".to_owned(),
            false,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
    assert_eq!(count(&cp, EventKind::LeaseForceReleased).await, before);
    let still = cp.leases().get(lease.id).await.unwrap().unwrap();
    assert_eq!(still.state, voom_store::repo::leases::LeaseState::Held);
}
