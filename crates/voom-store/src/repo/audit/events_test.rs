use super::*;

use time::OffsetDateTime;
use voom_events::{
    Event, EventEnvelope, EventKind, SubjectType, payload::SchemaInitializedPayload,
};

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let p = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (p, tmp)
}

fn sample_envelope() -> EventEnvelope {
    EventEnvelope {
        occurred_at: OffsetDateTime::UNIX_EPOCH,
        subject_type: SubjectType::System,
        subject_id: None,
        trace_id: None,
        payload: Event::SchemaInitialized(SchemaInitializedPayload {
            migrations_applied: 2,
            schema_init_at: OffsetDateTime::UNIX_EPOCH,
        }),
    }
}

#[tokio::test]
async fn append_in_tx_returns_assigned_event_id() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteEventRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let id = repo.append_in_tx(&mut tx, sample_envelope()).await.unwrap();
    tx.commit().await.unwrap();
    assert!(id.0 > 0);
}

#[tokio::test]
async fn get_returns_appended_row() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteEventRepo::new(pool.clone());
    let env = sample_envelope();
    let mut tx = pool.begin().await.unwrap();
    let id = repo.append_in_tx(&mut tx, env.clone()).await.unwrap();
    tx.commit().await.unwrap();
    let row = repo.get(id).await.unwrap().expect("row exists");
    assert_eq!(row.envelope.payload.kind(), env.payload.kind());
    assert_eq!(row.envelope.payload, env.payload);
}

#[tokio::test]
async fn list_filters_by_kind() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteEventRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    repo.append_in_tx(&mut tx, sample_envelope()).await.unwrap();
    tx.commit().await.unwrap();
    let page = repo
        .list(
            EventFilter {
                kind: Some(EventKind::SchemaInitialized),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    // Two rows: one seeded by init() (the schema.initialized contract from
    // Task 12) plus the one appended above. Both share the same kind, so
    // the kind-filter must return both.
    assert_eq!(page.items.len(), 2);
}

#[tokio::test]
async fn tail_returns_non_null_cursor_at_end_of_stream() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteEventRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    repo.append_in_tx(&mut tx, sample_envelope()).await.unwrap();
    tx.commit().await.unwrap();
    let page = repo
        .tail(
            EventFilter::default(),
            Page {
                limit: 1,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(
        page.next_cursor.is_some(),
        "tail cursor must persist even at EOS"
    );
}

#[tokio::test]
async fn list_cursor_signals_exhaustion_with_none() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteEventRepo::new(pool.clone());
    // init() seeds one schema.initialized event; append one more → 2 rows.
    let mut tx = pool.begin().await.unwrap();
    repo.append_in_tx(&mut tx, sample_envelope()).await.unwrap();
    tx.commit().await.unwrap();

    // First page covers every event.
    let first = repo
        .list(
            EventFilter::default(),
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let cursor = first.next_cursor.expect("cursor after a non-empty page");

    // Second page starts past the last event: it is empty and must signal
    // exhaustion with next_cursor == None, not echo back the caller's cursor.
    let second = repo
        .list(
            EventFilter::default(),
            Page {
                limit: 10,
                cursor: Some(cursor),
            },
        )
        .await
        .unwrap();
    assert!(second.items.is_empty(), "second page should be empty");
    assert_eq!(
        second.next_cursor, None,
        "exhausted list must return next_cursor None"
    );
}

#[tokio::test]
async fn append_then_get_round_trips_every_m1_kind() {
    use voom_core::{TicketOperation, WorkerKind};
    use voom_events::payload::{
        ArtifactHandleCreatedPayload, ArtifactLineageRecordedPayload,
        ArtifactLocationRecordedPayload, ArtifactLocationRetiredPayload, JobCancelledPayload,
        JobFailedPayload, JobOpenedPayload, JobSucceededPayload, LeaseAcquiredPayload,
        LeaseExpiredPayload, LeaseForceReleasedPayload, LeaseReleasedPayload, TicketCreatedPayload,
        TicketFailedRetriablePayload, TicketFailedTerminalPayload, TicketLeasedPayload,
        TicketReadyPayload, TicketRequeuedAfterLeaseExpiryPayload, TicketSucceededPayload,
        WorkerCapabilityRecordedPayload, WorkerGrantRecordedPayload, WorkerRegisteredPayload,
        WorkerRetiredPayload,
    };

    let (pool, _tmp) = pool().await;
    let repo = SqliteEventRepo::new(pool.clone());

    // Pair each M1 EventKind with a minimal Event payload. The list MUST
    // stay aligned with the M1 subset enumerated in
    // `each_kind_has_distinct_wire_string`.
    let pairs: Vec<(EventKind, Event)> = vec![
        (
            EventKind::SchemaInitialized,
            Event::SchemaInitialized(SchemaInitializedPayload {
                migrations_applied: 2,
                schema_init_at: OffsetDateTime::UNIX_EPOCH,
            }),
        ),
        (
            EventKind::JobOpened,
            Event::JobOpened(JobOpenedPayload {
                job_id: 1,
                kind: "k".to_owned(),
                priority: 0,
            }),
        ),
        (
            EventKind::JobSucceeded,
            Event::JobSucceeded(JobSucceededPayload { job_id: 1 }),
        ),
        (
            EventKind::JobFailed,
            Event::JobFailed(JobFailedPayload {
                job_id: 1,
                reason: "r".to_owned(),
            }),
        ),
        (
            EventKind::JobCancelled,
            Event::JobCancelled(JobCancelledPayload {
                job_id: 1,
                reason: "r".to_owned(),
            }),
        ),
        (
            EventKind::TicketCreated,
            Event::TicketCreated(TicketCreatedPayload {
                ticket_id: 1,
                job_id: None,
                kind: TicketOperation::new("k").unwrap(),
                priority: 0,
                max_attempts: 1,
            }),
        ),
        (
            EventKind::TicketReady,
            Event::TicketReady(TicketReadyPayload { ticket_id: 1 }),
        ),
        (
            EventKind::TicketLeased,
            Event::TicketLeased(TicketLeasedPayload {
                ticket_id: 1,
                lease_id: 1,
                worker_id: 1,
                attempt: 1,
            }),
        ),
        (
            EventKind::TicketSucceeded,
            Event::TicketSucceeded(TicketSucceededPayload {
                ticket_id: 1,
                lease_id: 1,
            }),
        ),
        (
            EventKind::TicketFailedRetriable,
            Event::TicketFailedRetriable(TicketFailedRetriablePayload {
                ticket_id: 1,
                attempt: 1,
                max_attempts: 3,
                reason: "r".to_owned(),
                class: voom_core::FailureClass::WorkerTimeout,
                next_eligible_at: OffsetDateTime::UNIX_EPOCH,
            }),
        ),
        (
            EventKind::TicketFailedTerminal,
            Event::TicketFailedTerminal(TicketFailedTerminalPayload {
                ticket_id: 1,
                attempt: 3,
                max_attempts: 3,
                reason: "r".to_owned(),
                class: voom_core::FailureClass::MalformedWorkerResult,
                issue_id: None,
            }),
        ),
        (
            EventKind::TicketRequeuedAfterLeaseExpiry,
            Event::TicketRequeuedAfterLeaseExpiry(TicketRequeuedAfterLeaseExpiryPayload {
                ticket_id: 1,
                lease_id: 1,
            }),
        ),
        (
            EventKind::LeaseAcquired,
            Event::LeaseAcquired(LeaseAcquiredPayload {
                lease_id: 1,
                ticket_id: 1,
                worker_id: 1,
                ttl_seconds: 60,
                expires_at: OffsetDateTime::UNIX_EPOCH,
            }),
        ),
        (
            EventKind::LeaseReleased,
            Event::LeaseReleased(LeaseReleasedPayload {
                lease_id: 1,
                ticket_id: 1,
                release_reason: "released".to_owned(),
            }),
        ),
        (
            EventKind::LeaseExpired,
            Event::LeaseExpired(LeaseExpiredPayload {
                lease_id: 1,
                ticket_id: 1,
            }),
        ),
        (
            EventKind::LeaseForceReleased,
            Event::LeaseForceReleased(LeaseForceReleasedPayload {
                lease_id: 1,
                ticket_id: 1,
                actor: "a".to_owned(),
                reason: "r".to_owned(),
                also_requeue: false,
            }),
        ),
        (
            EventKind::WorkerRegistered,
            Event::WorkerRegistered(WorkerRegisteredPayload {
                worker_id: 1,
                name: "w".to_owned(),
                kind: WorkerKind::Synthetic,
            }),
        ),
        (
            EventKind::WorkerCapabilityRecorded,
            Event::WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload {
                worker_id: 1,
                capability_id: 1,
                operation: TicketOperation::new("op").unwrap(),
            }),
        ),
        (
            EventKind::WorkerGrantRecorded,
            Event::WorkerGrantRecorded(WorkerGrantRecordedPayload {
                worker_id: 1,
                grant_id: 1,
            }),
        ),
        (
            EventKind::WorkerRetired,
            Event::WorkerRetired(WorkerRetiredPayload { worker_id: 1 }),
        ),
        (
            EventKind::ArtifactHandleCreated,
            Event::ArtifactHandleCreated(ArtifactHandleCreatedPayload {
                artifact_handle_id: 1,
                privacy_class: "internal".to_owned(),
                durability_class: "durable".to_owned(),
                mutability: "immutable".to_owned(),
            }),
        ),
        (
            EventKind::ArtifactLocationRecorded,
            Event::ArtifactLocationRecorded(ArtifactLocationRecordedPayload {
                artifact_location_id: 1,
                artifact_handle_id: 1,
                kind: "local_path".to_owned(),
                value: "/tmp/x".to_owned(),
            }),
        ),
        (
            EventKind::ArtifactLocationRetired,
            Event::ArtifactLocationRetired(ArtifactLocationRetiredPayload {
                artifact_location_id: 1,
                artifact_handle_id: 1,
            }),
        ),
        (
            EventKind::ArtifactLineageRecorded,
            Event::ArtifactLineageRecorded(ArtifactLineageRecordedPayload {
                artifact_lineage_id: 1,
                parent_artifact_id: 1,
                child_artifact_id: 2,
                operation: "transcode".to_owned(),
            }),
        ),
    ];

    for (kind, payload) in pairs {
        let env = EventEnvelope {
            occurred_at: OffsetDateTime::UNIX_EPOCH,
            subject_type: SubjectType::System,
            subject_id: None,
            trace_id: None,
            payload,
        };
        let mut tx = pool.begin().await.unwrap();
        let id = repo.append_in_tx(&mut tx, env.clone()).await.unwrap();
        tx.commit().await.unwrap();
        let row = repo
            .get(id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("row for {kind:?} not found"));
        assert_eq!(
            row.envelope.payload.kind(),
            kind,
            "kind mismatch on round-trip for {kind:?}"
        );
        assert_eq!(
            row.envelope.payload, env.payload,
            "payload mismatch on round-trip for {kind:?}"
        );
    }
}
