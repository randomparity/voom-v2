//! Ticket-lifecycle use cases. `create_ticket` follows the standard pattern.
//! `mark_ready_if_unblocked` walks every newly-promoted ticket the repo
//! reports and emits one `ticket.ready` per row in the same transaction.

use time::OffsetDateTime;
use voom_core::{TicketId, VoomError};
use voom_events::payload::{TicketCreatedPayload, TicketReadyPayload};
use voom_events::{Event, EventEnvelope, EventKind, SubjectType};
use voom_store::repo::events::EventRepo;
use voom_store::repo::tickets::{NewTicket, Ticket, TicketRepo};

use crate::ControlPlane;

impl ControlPlane {
    /// Create a new ticket and emit `ticket.created`.
    ///
    /// # Errors
    /// Propagates `TicketRepo::create_in_tx` and event-append errors.
    pub async fn create_ticket(&self, input: NewTicket) -> Result<Ticket, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let ticket = self.tickets.create_in_tx(&mut tx, input.clone()).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::TicketCreated,
                    occurred_at: input.created_at,
                    subject_type: SubjectType::Ticket,
                    subject_id: Some(ticket.id.0),
                    trace_id: None,
                    payload: Event::TicketCreated(TicketCreatedPayload {
                        ticket_id: ticket.id.0,
                        job_id: input.job_id.map(|j| j.0),
                        kind: input.kind.clone(),
                        priority: input.priority,
                        max_attempts: input.max_attempts,
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(ticket)
    }

    /// Promote a ticket to `ready` if its dependencies are all `succeeded`.
    /// Emits one `ticket.ready` event per row the repo reports as promoted,
    /// all inside one transaction. Returns the list of promoted ticket rows
    /// (empty when nothing was eligible — no event emitted in that case).
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn mark_ready_if_unblocked(
        &self,
        ticket_id: TicketId,
        now: OffsetDateTime,
    ) -> Result<Vec<Ticket>, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let promoted = self
            .tickets
            .mark_ready_if_unblocked_in_tx(&mut tx, ticket_id, now)
            .await?;
        for t in &promoted {
            self.events
                .append_in_tx(
                    &mut tx,
                    EventEnvelope {
                        kind: EventKind::TicketReady,
                        occurred_at: now,
                        subject_type: SubjectType::Ticket,
                        subject_id: Some(t.id.0),
                        trace_id: None,
                        payload: Event::TicketReady(TicketReadyPayload { ticket_id: t.id.0 }),
                    },
                )
                .await?;
        }
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(promoted)
    }
}

#[cfg(test)]
#[path = "tickets_test.rs"]
mod tests;
