//! Couple failed canonical routing tickets to their active scan sessions.

use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{
    NodeId, OperationKind, ScanSessionId, ScanSessionStatus, ScanTerminalReason, StorageRootId,
    VoomError,
};
use voom_store::repo::execution::tickets::{Ticket, TicketState};
use voom_store::tx::begin_read_then_write;

use super::sessions::append_lifecycle_event;
use crate::ControlPlane;
use crate::cases::commit_tx;
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;

impl ControlPlane {
    pub(crate) async fn fail_scan_for_failed_ticket_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        ticket: &Ticket,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        // Workflow names alone are opaque metadata on unrelated ticket kinds.
        if ticket.state != TicketState::Failed
            || ticket.kind.as_str() != "synthetic.workflow.operation.scan_library"
            || ticket.payload["workflow_id"] != "scan-run"
        {
            return Ok(());
        }
        let context = format!("scan routing ticket {}", ticket.id);
        let payload =
            WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload.clone())
                .map_err(|error| VoomError::database_context(&context, error))?;
        let session_id = ScanSessionId(canonical_text_id(
            payload.rendered_payload["scan_session_id"].as_str(),
            &context,
        )?);
        let owner = NodeId(canonical_text_id(Some(&payload.node_id), &context)?);
        let root = StorageRootId(
            payload.rendered_payload["source_storage_root_id"]
                .as_u64()
                .filter(|id| *id > 0 && i64::try_from(*id).is_ok())
                .ok_or_else(|| {
                    VoomError::database(format!("{context}: invalid storage root id"))
                })?,
        );
        if payload.operation != OperationKind::ScanLibrary
            || payload.plan_id != session_id.to_string()
            || payload.branch_id != format!("scan-run-{session_id}")
        {
            return Err(VoomError::database(format!(
                "{context}: incoherent scan linkage"
            )));
        }
        let session = self
            .scan_sessions
            .get_in_tx(tx, session_id)
            .await?
            .ok_or_else(|| {
                VoomError::database(format!("{context}: linked scan session is missing"))
            })?;
        if session.storage_root_id != root || session.owner_node_id != owner {
            return Err(VoomError::database(format!(
                "{context}: scan root or owner mismatch"
            )));
        }
        if matches!(
            session.status,
            ScanSessionStatus::Requested | ScanSessionStatus::Running
        ) {
            let failed = self
                .scan_sessions
                .terminalize_in_tx(
                    tx,
                    session_id,
                    ScanSessionStatus::Failed,
                    ScanTerminalReason::new(format!("routing ticket {} failed", ticket.id))?,
                    now,
                )
                .await?;
            append_lifecycle_event(self, tx, &failed, now).await?;
        }
        Ok(())
    }

    pub(crate) async fn recover_failed_scan_runs(
        &self,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let mut tx =
            begin_read_then_write(&self.pool, "scan: recover failed routing tickets").await?;
        let candidates = self
            .tickets
            .failed_scan_run_candidates_in_tx(&mut tx)
            .await?;
        for ticket in &candidates {
            self.fail_scan_for_failed_ticket_in_tx(&mut tx, ticket, now)
                .await?;
        }
        commit_tx(tx).await
    }
}

fn canonical_text_id(value: Option<&str>, context: &str) -> Result<u64, VoomError> {
    value
        .and_then(|value| {
            value
                .parse::<u64>()
                .ok()
                .filter(|id| *id > 0 && i64::try_from(*id).is_ok() && id.to_string() == value)
        })
        .ok_or_else(|| VoomError::database(format!("{context}: invalid scan routing identifier")))
}
