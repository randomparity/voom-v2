//! `ControlPlane` use cases. Each method composes a repo `_in_tx` write
//! with `EventRepo::append_in_tx` inside one transaction so every M1
//! state transition produces exactly one event row.
//!
//! `begin_tx`, `commit_tx`, and `append_event` are the shared
//! transaction-and-event boilerplate used by every case file. They live
//! here rather than duplicated per file so the five case modules stay
//! consistent.

use sqlx::{Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_events::{Event, EventEnvelope, EventKind, SubjectType};
use voom_store::repo::events::{EventRepo, SqliteEventRepo};

pub mod artifacts;
pub mod jobs;
pub mod leases;
pub mod tickets;
pub mod workers;

pub(crate) async fn begin_tx(pool: &SqlitePool) -> Result<Transaction<'_, Sqlite>, VoomError> {
    pool.begin()
        .await
        .map_err(|e| VoomError::Database(format!("begin: {e}")))
}

pub(crate) async fn commit_tx(tx: Transaction<'_, Sqlite>) -> Result<(), VoomError> {
    tx.commit()
        .await
        .map_err(|e| VoomError::Database(format!("commit: {e}")))
}

pub(crate) async fn append_event(
    events: &SqliteEventRepo,
    tx: &mut Transaction<'_, Sqlite>,
    kind: EventKind,
    subject_type: SubjectType,
    subject_id: Option<u64>,
    occurred_at: OffsetDateTime,
    payload: Event,
) -> Result<(), VoomError> {
    events
        .append_in_tx(
            tx,
            EventEnvelope {
                kind,
                occurred_at,
                subject_type,
                subject_id,
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

#[cfg(test)]
pub(crate) async fn cp() -> (crate::ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();
    (cp, tmp)
}
