//! Read-only external-system sync and sync report (Sprint 17, T15).
//!
//! V1 sync is a read-only run: it re-probes health, records the run as a durable
//! `external_system.synced` event, and returns a report. It records no links
//! because V1 ships no external catalog-match engine (ADR 0029) — the fake's
//! worker read path is proven by an integration test, and the Sprint 20 daemon
//! adds the reconciliation loop over the same link primitives. `sync-report`
//! reconstructs the latest run from the durable event plus the active links.

use time::OffsetDateTime;
use voom_core::{ExternalSystemId, VoomError};
use voom_events::payload::ExternalSystemSyncedPayload;
use voom_events::{Event, EventKind, SubjectType};
use voom_store::repo::events::{EventFilter, EventRepo, Page};

use crate::ControlPlane;

use super::super::{append_event, begin_immediate_tx, commit_tx};

/// Summary of an external-system sync. `last_*` fields describe the most recent
/// run and are `None` when the system has never been synced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSyncReport {
    pub external_system_id: u64,
    pub health_status: String,
    pub active_link_count: u64,
    pub last_outcome: Option<String>,
    pub last_links_recorded: Option<u32>,
    pub last_links_retired: Option<u32>,
    pub last_started_at: Option<OffsetDateTime>,
    pub last_finished_at: Option<OffsetDateTime>,
}

impl ControlPlane {
    /// Run a read-only sync: re-probe health, record the run, and report.
    ///
    /// # Errors
    /// Returns `NotFound` for an unknown system; propagates probe, repository,
    /// and event-append errors.
    pub async fn sync_external_system(
        &self,
        id: ExternalSystemId,
    ) -> Result<ExternalSyncReport, VoomError> {
        let started_at = self.clock().now();
        let updated = self.health_check_external_system(id).await?;
        let finished_at = self.clock().now();
        let active_links = self.external_systems.list_links(id).await?;
        let outcome = sync_outcome(updated.health_status.as_str());

        let mut tx = begin_immediate_tx(&self.pool).await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::ExternalSystem,
            Some(id.0),
            finished_at,
            Event::ExternalSystemSynced(ExternalSystemSyncedPayload {
                external_system_id: id.0,
                outcome: outcome.clone(),
                links_recorded: 0,
                links_retired: 0,
                started_at,
                finished_at,
            }),
        )
        .await?;
        commit_tx(tx).await?;

        Ok(ExternalSyncReport {
            external_system_id: id.0,
            health_status: updated.health_status.as_str().to_owned(),
            active_link_count: active_links.len() as u64,
            last_outcome: Some(outcome),
            last_links_recorded: Some(0),
            last_links_retired: Some(0),
            last_started_at: Some(started_at),
            last_finished_at: Some(finished_at),
        })
    }

    /// Report the latest sync run for a system plus its current state.
    ///
    /// # Errors
    /// Returns `NotFound` for an unknown system; propagates repository errors.
    pub async fn external_sync_report(
        &self,
        id: ExternalSystemId,
    ) -> Result<ExternalSyncReport, VoomError> {
        let system = self
            .external_systems
            .get(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("external system id={id} not found")))?;
        let active_links = self.external_systems.list_links(id).await?;
        let latest = self
            .events()
            .tail(
                EventFilter {
                    kind: Some(EventKind::ExternalSystemSynced),
                    subject_type: Some(SubjectType::ExternalSystem),
                    subject_id: Some(id.0),
                },
                Page {
                    limit: 1,
                    cursor: None,
                },
            )
            .await?;

        let mut report = ExternalSyncReport {
            external_system_id: id.0,
            health_status: system.health_status.as_str().to_owned(),
            active_link_count: active_links.len() as u64,
            last_outcome: None,
            last_links_recorded: None,
            last_links_retired: None,
            last_started_at: None,
            last_finished_at: None,
        };
        if let Some(row) = latest.items.into_iter().next()
            && let Event::ExternalSystemSynced(p) = row.envelope.payload
        {
            report.last_outcome = Some(p.outcome);
            report.last_links_recorded = Some(p.links_recorded);
            report.last_links_retired = Some(p.links_retired);
            report.last_started_at = Some(p.started_at);
            report.last_finished_at = Some(p.finished_at);
        }
        Ok(report)
    }
}

/// Derive a sync run outcome from the recorded health status.
fn sync_outcome(health: &str) -> String {
    if health == "unreachable" {
        "failed".to_owned()
    } else {
        "ok".to_owned()
    }
}

#[cfg(test)]
#[path = "sync_test.rs"]
mod tests;
