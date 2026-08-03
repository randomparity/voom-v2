use super::*;

use time::OffsetDateTime;
use voom_core::{JobId, VoomError};
use voom_events::EventKind;
use voom_store::repo::audit::events::{EventFilter, EventRepo, Page};
use voom_store::repo::execution::jobs::JobState;

use crate::cases::{count, cp};

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
    let reason = "  downstream\nbroken — 再試行  ";
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
        reason.to_owned(),
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
    assert_eq!(payload.reason.as_bytes(), reason.as_bytes());
}

#[tokio::test]
async fn fail_job_rejects_blank_reasons_without_durable_changes() {
    let (cp, _tmp) = cp().await;

    for reason in ["", " \t\n "] {
        let job = cp
            .open_job(NewJob {
                kind: "ingest".to_owned(),
                priority: 0,
                created_at: OffsetDateTime::UNIX_EPOCH,
            })
            .await
            .unwrap();
        let before_events = count(&cp, EventKind::JobFailed).await;

        let err = cp
            .fail_job(
                job.id,
                reason.to_owned(),
                OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code(), "CONFIG_INVALID");
        let VoomError::Config(message) = err else {
            panic!("expected configuration error");
        };
        assert_eq!(message, "reason must not be empty or whitespace");
        let stored = cp.get_job(job.id.0).await.unwrap().unwrap();
        assert_eq!(stored.state, JobState::Open);
        assert_eq!(stored.epoch, 0);
        assert_eq!(count(&cp, EventKind::JobFailed).await, before_events);
    }
}

#[tokio::test]
async fn fail_job_validates_blank_reason_before_database_access() {
    let (cp, _tmp) = cp().await;
    cp.pool.close().await;

    let err = cp
        .fail_job(JobId(1), "  \n".to_owned(), OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFIG_INVALID");
    let VoomError::Config(message) = err else {
        panic!("expected configuration error");
    };
    assert_eq!(message, "reason must not be empty or whitespace");
}

#[tokio::test]
async fn cancel_job_persists_state_and_reason_in_one_event() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "ingest".to_owned(),
            priority: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let cancelled = cp
        .cancel_job(
            job.id,
            "operator cancel".to_owned(),
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert_eq!(cancelled.epoch, 1);
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
    assert_eq!(page.items[0].envelope.subject_id, Some(job.id.0));
    let voom_events::Event::JobCancelled(payload) = &page.items[0].envelope.payload else {
        panic!("expected JobCancelled payload");
    };
    assert_eq!(payload.job_id, job.id);
    assert_eq!(payload.reason, "operator cancel");
}

#[tokio::test]
async fn cancel_job_rejects_blank_reason_without_durable_changes() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "ingest".to_owned(),
            priority: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let before_events = count(&cp, EventKind::JobCancelled).await;

    let err = cp
        .cancel_job(
            job.id,
            "   ".to_owned(),
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Config(_)), "got: {err:?}");
    let stored = cp.get_job(job.id.0).await.unwrap().unwrap();
    assert_eq!(stored.state, JobState::Open);
    assert_eq!(stored.epoch, 0);
    assert_eq!(count(&cp, EventKind::JobCancelled).await, before_events);
}

#[tokio::test]
async fn cancel_missing_job_is_not_found_without_event() {
    let (cp, _tmp) = cp().await;

    let err = cp
        .cancel_job(
            JobId(999),
            "operator cancel".to_owned(),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::NotFound(_)), "got: {err:?}");
    assert_eq!(count(&cp, EventKind::JobCancelled).await, 0);
}

#[tokio::test]
async fn cancel_job_rolls_back_when_event_append_fails() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "ingest".to_owned(),
            priority: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    sqlx::query("ALTER TABLE events RENAME TO unavailable_events")
        .execute(&cp.pool)
        .await
        .unwrap();

    let err = cp
        .cancel_job(
            job.id,
            "operator cancel".to_owned(),
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Database { .. }), "got: {err:?}");
    sqlx::query("ALTER TABLE unavailable_events RENAME TO events")
        .execute(&cp.pool)
        .await
        .unwrap();
    let stored = cp.get_job(job.id.0).await.unwrap().unwrap();
    assert_eq!(stored.state, JobState::Open);
    assert_eq!(stored.epoch, 0);
    assert_eq!(count(&cp, EventKind::JobCancelled).await, 0);
}
