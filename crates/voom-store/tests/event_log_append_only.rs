#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

//! UPDATE and DELETE on the `events` table both hit the BEFORE triggers
//! defined in migration 0002 and surface the "events are append-only"
//! message. Exercises the architectural-spec invariant that the event
//! log is immutable to even direct-SQL writers (no `ControlPlane` here
//! — we want a raw `EventRepo::append_in_tx` followed by raw
//! `sqlx::query` writes against the row).

use time::OffsetDateTime;

use voom_events::payload::SchemaInitializedPayload;
use voom_events::{Event, EventEnvelope, SubjectType};
use voom_store::repo::events::{EventRepo, SqliteEventRepo};
use voom_store::test_support::fresh_initialized_pool_at;

#[tokio::test]
async fn update_on_events_row_is_rejected() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let repo = SqliteEventRepo::new(pool.clone());

    let mut tx = pool.begin().await.unwrap();
    let id = repo
        .append_in_tx(
            &mut tx,
            EventEnvelope {
                occurred_at: OffsetDateTime::UNIX_EPOCH,
                subject_type: SubjectType::System,
                subject_id: None,
                trace_id: None,
                payload: Event::SchemaInitialized(SchemaInitializedPayload {
                    migrations_applied: 1,
                    schema_init_at: OffsetDateTime::UNIX_EPOCH,
                }),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let err = sqlx::query("UPDATE events SET kind = 'evil' WHERE event_id = ?")
        .bind(i64::try_from(id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("events are append-only"), "got: {msg}");
}

#[tokio::test]
async fn delete_on_events_row_is_rejected() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let repo = SqliteEventRepo::new(pool.clone());

    let mut tx = pool.begin().await.unwrap();
    let id = repo
        .append_in_tx(
            &mut tx,
            EventEnvelope {
                occurred_at: OffsetDateTime::UNIX_EPOCH,
                subject_type: SubjectType::System,
                subject_id: None,
                trace_id: None,
                payload: Event::SchemaInitialized(SchemaInitializedPayload {
                    migrations_applied: 1,
                    schema_init_at: OffsetDateTime::UNIX_EPOCH,
                }),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let err = sqlx::query("DELETE FROM events WHERE event_id = ?")
        .bind(i64::try_from(id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("events are append-only"), "got: {msg}");
}
