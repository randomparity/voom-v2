//! `request_scan_run`: mint one durable scan session plus its ready
//! `scan_library` ticket (ADR 0077).
//!
//! The control plane never touches the root's bytes here: it fail-closes on
//! effective-root availability, requests the session against the root's owner
//! node and current epoch, and creates exactly one ready namespaced
//! `scan_library` ticket whose payload carries the session id and the canonical
//! whole-root read declaration. The owner node's agent acquires the ticket and
//! drives the session from there.

use serde_json::json;
use voom_core::{
    OperationKind, ScanSessionId, StorageRootId, TicketId, TicketOperation, VoomError,
    WORKFLOW_OPERATION_NAMESPACE,
};
use voom_store::repo::execution::tickets::NewTicket;
use voom_store::repo::scan::sessions::NewScanSession;

use super::library::{RootBlockReason, RootScanBlocked};
use super::sessions::{append_lifecycle_event, progress_deadline, validate_idle_timeout};
use crate::ControlPlane;
use crate::cases::execution::remote_execution::is_remote_replayable_error;
use crate::cases::{begin_immediate_tx, commit_tx};
use crate::workflow::execution::timing::EffectiveTiming;
use crate::workflow::plan::access_declaration::{TicketStorageSource, declaration_for};
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;

/// Ticket-kind suffix for scan runs. The ticket kind itself MUST be the
/// namespaced form `{WORKFLOW_OPERATION_NAMESPACE}.scan_library`:
/// `WorkflowTicketPayload::parse_ticket` accepts only namespaced kinds, and a
/// bare `scan_library` kind would silently degrade acquire gating to
/// `NoDeclaration`.
pub(crate) const SCAN_RUN_TICKET_KIND: &str = "scan_library";

/// One requested run: the durable session plus its ready routing ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRunRequested {
    pub scan_session_id: ScanSessionId,
    pub ticket_id: TicketId,
}

/// Outcome of `request_scan_run`. `Blocked` means nothing was created because
/// the root (or its library) is not available.
#[derive(Debug)]
pub enum ScanRunOutcome {
    Requested(ScanRunRequested),
    Blocked(RootScanBlocked),
}

impl ControlPlane {
    /// Request a scan run for a storage root: fail-closed on availability,
    /// then session + ready ticket in one transaction.
    ///
    /// # Errors
    /// Returns `NOT_FOUND` for a missing root and propagates store or payload
    /// encoding errors; a duplicate active session conflicts without a ticket.
    pub async fn request_scan_run(
        &self,
        storage_root_id: StorageRootId,
        idle_timeout_seconds: u32,
    ) -> Result<ScanRunOutcome, VoomError> {
        validate_idle_timeout(idle_timeout_seconds)?;
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let now = self.clock().now();
        let expired = self.scan_sessions.stale_expired_in_tx(&mut tx, now).await?;
        for session in &expired {
            append_lifecycle_event(self, &mut tx, session, now).await?;
        }
        let effective = self
            .libraries
            .effective_library_root_in_tx(&mut tx, storage_root_id)
            .await?;
        let Some(effective) = effective else {
            commit_tx(tx).await?;
            return Err(VoomError::NotFound(format!(
                "library root {storage_root_id} not found"
            )));
        };
        let provider_locator = effective.root.provider_locator.as_str().to_owned();
        if let Some(reason) = RootBlockReason::from_availability(effective.reason) {
            commit_tx(tx).await?;
            return Ok(ScanRunOutcome::Blocked(RootScanBlocked {
                library_id: effective.root.library_id,
                storage_root_id,
                reason,
                provider_locator,
            }));
        }
        let owner_node_id = effective.root.owner_node_id.ok_or_else(|| {
            VoomError::database(format!(
                "available storage root {storage_root_id} has no owner"
            ))
        })?;

        let session = self
            .scan_sessions
            .insert_requested_in_tx(
                &mut tx,
                NewScanSession {
                    storage_root_id,
                    root_epoch: effective.root.root_epoch,
                    owner_node_id,
                    idle_timeout_seconds,
                    progress_deadline_at: progress_deadline(now, idle_timeout_seconds)?,
                    requested_at: now,
                },
            )
            .await;
        let session = match session {
            Ok(session) => session,
            Err(error) if is_remote_replayable_error(&error) => {
                commit_tx(tx).await?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };

        let kind = TicketOperation::new(format!(
            "{WORKFLOW_OPERATION_NAMESPACE}{SCAN_RUN_TICKET_KIND}"
        ))?;
        let payload = scan_run_ticket_payload(
            session.id,
            owner_node_id,
            storage_root_id,
            idle_timeout_seconds,
        )?;
        let ticket = self
            .create_ticket_in_tx(
                &mut tx,
                NewTicket {
                    job_id: None,
                    kind,
                    priority: 0,
                    payload,
                    max_attempts: 1,
                    created_at: now,
                },
            )
            .await?;
        self.mark_ready_if_unblocked_in_tx(&mut tx, ticket.id, now)
            .await?;

        append_lifecycle_event(self, &mut tx, &session, now).await?;
        commit_tx(tx).await?;
        Ok(ScanRunOutcome::Requested(ScanRunRequested {
            scan_session_id: session.id,
            ticket_id: ticket.id,
        }))
    }
}

/// Encode the scan-run ticket payload: strict `WorkflowTicketPayload` with the
/// session id in `rendered_payload`, no `source_location_id` (the declaration
/// addresses the whole root), and the canonical read-only declaration.
fn scan_run_ticket_payload(
    scan_session_id: ScanSessionId,
    owner_node_id: voom_core::NodeId,
    storage_root_id: StorageRootId,
    idle_timeout_seconds: u32,
) -> Result<serde_json::Value, VoomError> {
    let duration_ms = u64::from(idle_timeout_seconds) * 1_000;
    let declared_artifact_access = declaration_for(
        OperationKind::ScanLibrary,
        Some(&TicketStorageSource::Root { storage_root_id }),
    )?
    .ok_or_else(|| VoomError::database("scan_library is byte-touching and must declare access"))?;
    WorkflowTicketPayload {
        workflow_id: "scan-run".to_owned(),
        plan_id: scan_session_id.to_string(),
        node_id: owner_node_id.to_string(),
        branch_id: format!("scan-run-{scan_session_id}"),
        operation: OperationKind::ScanLibrary,
        rendered_payload: json!({
            "operation": "scan_library",
            "source_storage_root_id": storage_root_id.0,
            "scan_session_id": scan_session_id.to_string(),
        }),
        timing: EffectiveTiming {
            duration_ms,
            progress_interval_ms: 1_000,
        },
        source_file: None,
        declared_artifact_access: Some(declared_artifact_access),
    }
    .to_ticket_payload()
    .map_err(|error| VoomError::Config(error.to_string()))
}

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
