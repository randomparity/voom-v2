use super::*;

use time::OffsetDateTime;
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};

use crate::cases::cp;

#[tokio::test]
async fn open_job_emits_job_opened() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "ingest".to_owned(),
            priority: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::JobOpened),
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
    assert_eq!(page.items[0].envelope.subject_id, Some(job.id.0));
}

#[tokio::test]
async fn succeed_job_emits_job_succeeded() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "ingest".to_owned(),
            priority: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    cp.succeed_job(
        job.id,
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::JobSucceeded),
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
    assert_eq!(page.items[0].envelope.subject_id, Some(job.id.0));
}

#[tokio::test]
async fn fail_job_emits_job_failed_with_reason_in_payload() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "ingest".to_owned(),
            priority: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    cp.fail_job(
        job.id,
        "downstream broken".to_owned(),
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::JobFailed),
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
    let voom_events::Event::JobFailed(payload) = &page.items[0].envelope.payload else {
        panic!("expected JobFailed payload");
    };
    assert_eq!(payload.reason, "downstream broken");
}

#[tokio::test]
async fn cancel_job_emits_job_cancelled() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "ingest".to_owned(),
            priority: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    cp.cancel_job(
        job.id,
        "operator cancel".to_owned(),
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::JobCancelled),
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
