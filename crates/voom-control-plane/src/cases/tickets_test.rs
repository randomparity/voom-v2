use super::*;

use time::OffsetDateTime;
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::tickets::TicketRepo;

use crate::cases::cp;

fn ticket(kind: &str) -> NewTicket {
    NewTicket {
        job_id: None,
        kind: kind.to_owned(),
        priority: 0,
        payload: serde_json::json!({}),
        max_attempts: 1,
        created_at: OffsetDateTime::UNIX_EPOCH,
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
