//! Request-scoped `ControlPlane` use cases. Each audited mutation composes a
//! repository `_in_tx` write with `EventRepo::append_in_tx` inside one
//! transaction so the state transition and its event remain atomic.
//!
//! `begin_tx`, `commit_tx`, and `append_event` are the shared
//! transaction-and-event boilerplate used by every case file. They live
//! here rather than duplicated per folder so media, policy, execution, and
//! worker use cases stay consistent.

use sqlx::{Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_events::{Event, EventEnvelope, SubjectType};
use voom_store::repo::audit::events::{EventRepo, SqliteEventRepo};

pub(crate) mod config;
pub(crate) mod execution;
pub(crate) mod external;
pub(crate) mod media;
pub(crate) mod policy;
pub(crate) mod workers;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[expect(
    unused_imports,
    reason = "execution tests infer this fixture type but its crate-visible API must be preserved"
)]
pub(crate) use tests::{
    TerminalFailureIssueRow, count, cp, issue_link_targets, terminal_failure_issues,
    transcodable_input,
};

pub(crate) async fn begin_tx(pool: &SqlitePool) -> Result<Transaction<'_, Sqlite>, VoomError> {
    pool.begin()
        .await
        .map_err(|e| VoomError::database_context("begin", e))
}

/// Begin a transaction that takes `SQLite`'s write lock up front (`BEGIN
/// IMMEDIATE`) instead of lazily on the first write.
///
/// Use this for read-then-write transactions that run under contention. A
/// deferred `BEGIN` acquires the write lock only when the first write executes;
/// if another writer holds it by then, `SQLite` returns `SQLITE_BUSY` *without*
/// invoking the busy handler (to avoid a lock-upgrade deadlock), so the caller
/// fails instead of waiting. `BEGIN IMMEDIATE` lets `busy_timeout` serialize the
/// writers cleanly. Mirrors `begin_immediate` in the policy registry repo.
pub(crate) async fn begin_immediate_tx(
    pool: &SqlitePool,
) -> Result<Transaction<'_, Sqlite>, VoomError> {
    pool.begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| VoomError::database_context("begin immediate", e))
}

pub(crate) async fn commit_tx(tx: Transaction<'_, Sqlite>) -> Result<(), VoomError> {
    tx.commit()
        .await
        .map_err(|e| VoomError::database_context("commit", e))
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
