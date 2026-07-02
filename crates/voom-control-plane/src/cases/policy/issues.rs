//! Operator issue use cases (T14, #283). Read and transition durable issues of
//! any kind (`policy_noncompliant`, `terminal_failure`, …) for the `voom issue`
//! CLI. Reads delegate straight to the repo; each transition composes the repo
//! `_in_tx` write with one `issue.*` event in the same transaction, matching the
//! one-transition-one-event rule the other case files follow.
//!
//! The event taxonomy has three issue kinds (`opened`/`updated`/`resolved`). A
//! `resolve` maps to `issue.resolved`; `update` (priority override), `suppress`,
//! and `accept` all map to `issue.updated` — the lifecycle payload's `status`
//! field carries the specific new state.

use time::{Duration, OffsetDateTime};
use voom_core::{IssueId, IssuePriority, VoomError};
use voom_events::payload::IssueLifecyclePayload;
use voom_events::{Event, SubjectType};
use voom_store::repo::issues::{IssueFilter, IssueListPage, IssueRecord};

use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};

impl ControlPlane {
    /// List issues, filtered and keyset-paginated by ascending id.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn list_issues(
        &self,
        filter: &IssueFilter,
        cursor: Option<u64>,
        limit: u32,
    ) -> Result<IssueListPage, VoomError> {
        self.issues.list_issues(filter, cursor, limit).await
    }

    /// Read one issue by id.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn get_issue(&self, id: IssueId) -> Result<Option<IssueRecord>, VoomError> {
        self.issues.get_issue(id).await
    }

    /// Override an issue's priority (and optionally its reason), emitting
    /// `issue.updated`. `Ok(None)` when no issue has that id.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn update_issue_priority(
        &self,
        id: IssueId,
        priority: IssuePriority,
        priority_reason: Option<String>,
    ) -> Result<Option<IssueRecord>, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let record = self
            .issues
            .update_priority_in_tx(&mut tx, id, priority, priority_reason.as_deref(), now)
            .await?;
        if let Some(record) = &record {
            self.emit_issue_event(&mut tx, record, false, now).await?;
        }
        commit_tx(tx).await?;
        Ok(record)
    }

    /// Transition an issue to `resolved`, emitting `issue.resolved`. `Ok(None)`
    /// when no issue has that id.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn resolve_issue(&self, id: IssueId) -> Result<Option<IssueRecord>, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let record = self.issues.resolve_in_tx(&mut tx, id, now).await?;
        if let Some(record) = &record {
            self.emit_issue_event(&mut tx, record, true, now).await?;
        }
        commit_tx(tx).await?;
        Ok(record)
    }

    /// Transition an issue to `suppressed` for `days` from now, emitting
    /// `issue.updated`. `Ok(None)` when no issue has that id.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn suppress_issue(
        &self,
        id: IssueId,
        days: u32,
    ) -> Result<Option<IssueRecord>, VoomError> {
        let now = self.clock().now();
        let until = now + Duration::days(i64::from(days));
        let mut tx = begin_tx(&self.pool).await?;
        let record = self.issues.suppress_in_tx(&mut tx, id, until, now).await?;
        if let Some(record) = &record {
            self.emit_issue_event(&mut tx, record, false, now).await?;
        }
        commit_tx(tx).await?;
        Ok(record)
    }

    /// Transition an issue to `accepted`, emitting `issue.updated`. `Ok(None)`
    /// when no issue has that id.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn accept_issue(&self, id: IssueId) -> Result<Option<IssueRecord>, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let record = self.issues.accept_in_tx(&mut tx, id, now).await?;
        if let Some(record) = &record {
            self.emit_issue_event(&mut tx, record, false, now).await?;
        }
        commit_tx(tx).await?;
        Ok(record)
    }

    async fn emit_issue_event(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        record: &IssueRecord,
        is_resolution: bool,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let payload = IssueLifecyclePayload {
            issue_id: record.id,
            kind: record.kind.clone(),
            status: record.status.as_str().to_owned(),
            dedupe_key: record.dedupe_key.clone(),
            policy_version_id: None,
            report_id: None,
        };
        let event = if is_resolution {
            Event::IssueResolved(payload)
        } else {
            Event::IssueUpdated(payload)
        };
        append_event(
            &self.events,
            tx,
            SubjectType::System,
            Some(record.id.0),
            now,
            event,
        )
        .await
    }
}

#[cfg(test)]
#[path = "issues_test.rs"]
mod tests;
