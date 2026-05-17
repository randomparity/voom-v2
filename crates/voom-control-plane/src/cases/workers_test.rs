use super::*;

use time::OffsetDateTime;
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::workers::WorkerKind;

use crate::cases::cp;

fn page_filter(kind: EventKind) -> EventFilter {
    EventFilter {
        kind: Some(kind),
        ..EventFilter::default()
    }
}

#[tokio::test]
async fn register_worker_emits_worker_registered() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "alpha".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let page = cp
        .events()
        .list(
            page_filter(EventKind::WorkerRegistered),
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].envelope.subject_id, Some(w.id.0));
}

#[tokio::test]
async fn record_capability_emits_worker_capability_recorded() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "alpha".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let cap = cp
        .record_capability(NewCapability {
            worker_id: w.id,
            operation: "ingest".to_owned(),
            codecs: vec![],
            hardware: vec![],
            artifact_access: vec![],
            extra: serde_json::json!({}),
        })
        .await
        .unwrap();
    let page = cp
        .events()
        .list(
            page_filter(EventKind::WorkerCapabilityRecorded),
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let voom_events::Event::WorkerCapabilityRecorded(payload) = &page.items[0].envelope.payload
    else {
        panic!("expected WorkerCapabilityRecorded payload");
    };
    assert_eq!(payload.capability_id, cap.id);
    assert_eq!(payload.operation, "ingest");
}

#[tokio::test]
async fn record_grant_emits_worker_grant_recorded() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "alpha".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let grant = cp
        .record_grant(NewGrant {
            worker_id: w.id,
            can_execute: vec!["ingest".to_owned()],
            can_access_read: vec![],
            can_access_write: vec![],
            denies: vec![],
            max_parallel: serde_json::json!({"ingest": 2}),
        })
        .await
        .unwrap();
    let page = cp
        .events()
        .list(
            page_filter(EventKind::WorkerGrantRecorded),
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let voom_events::Event::WorkerGrantRecorded(payload) = &page.items[0].envelope.payload else {
        panic!("expected WorkerGrantRecorded payload");
    };
    assert_eq!(payload.grant_id, grant.id);
}

#[tokio::test]
async fn retire_worker_emits_worker_retired() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "alpha".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    cp.retire_worker(
        w.id,
        w.epoch,
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let page = cp
        .events()
        .list(
            page_filter(EventKind::WorkerRetired),
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].envelope.subject_id, Some(w.id.0));
}
