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
use voom_events::{Event, EventEnvelope, SubjectType};
use voom_store::repo::events::{EventRepo, SqliteEventRepo};

#[cfg(test)]
use voom_events::EventKind;
#[cfg(test)]
use voom_store::repo::events::{EventFilter, Page};

pub mod artifacts;
pub mod bundles;
pub mod compliance;
pub mod identity;
pub mod jobs;
pub mod leases;
pub mod nodes;
pub mod plans;
pub mod policies;
pub mod policy_inputs;
pub mod remote_execution;
pub mod tickets;
pub mod use_leases;
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

/// Reject empty or whitespace-only audit strings. The `force_release` and
/// `recover_stale_issuer` paths exist specifically to record operator intent
/// (sprint-1 design §9.2) — a blank actor or reason would terminate a
/// lease and leave an audit row that carries no operator information.
pub(crate) fn require_audit_field(name: &str, value: &str) -> Result<(), VoomError> {
    if value.trim().is_empty() {
        return Err(VoomError::Config(format!(
            "{name} must not be empty or whitespace"
        )));
    }
    Ok(())
}

pub(crate) async fn append_event(
    events: &SqliteEventRepo,
    tx: &mut Transaction<'_, Sqlite>,
    subject_type: SubjectType,
    subject_id: Option<u64>,
    occurred_at: OffsetDateTime,
    payload: Event,
) -> Result<(), VoomError> {
    events
        .append_in_tx(
            tx,
            EventEnvelope {
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
pub(crate) async fn count(cp: &crate::ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 200,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

#[cfg(test)]
pub(crate) async fn cp() -> (crate::ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(
            voom_core::rng_test_support::FrozenRng::new(u32::MAX),
        )),
    )
    .await
    .unwrap();
    (cp, tmp)
}

/// Builds a single-video mp4/h264 input set whose snapshot is transcodable to
/// hevc, used by both the execute-path and dry-run-path resolution tests.
#[cfg(test)]
pub(crate) async fn transcodable_input(
    cp: &crate::ControlPlane,
    slug: &str,
) -> voom_core::PolicyInputSetId {
    let mut draft =
        voom_policy::load_fixture(voom_policy::FixtureName::SyntheticNoncompliantTranscodeNeeded)
            .unwrap();
    draft.slug = slug.to_owned();
    draft.fixture_labels = vec![slug.replace('-', "_")];
    let snapshot = &mut draft.media_snapshots[0];
    snapshot.container = Some("mp4".to_owned());
    snapshot.video_codec = Some("h264".to_owned());
    snapshot.stream_summary = serde_json::json!({ "video_stream_count": 1 });
    cp.create_policy_input_set(draft).await.unwrap().id
}
