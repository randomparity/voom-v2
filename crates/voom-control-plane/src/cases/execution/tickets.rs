//! Ticket-lifecycle use cases. `create_ticket` follows the standard pattern.
//! `mark_ready_if_unblocked` walks every newly-promoted ticket the repo
//! reports and emits one `ticket.ready` per row in the same transaction.

use time::OffsetDateTime;
use voom_core::{FailureClass, TicketId, VoomError};
use voom_events::payload::{
    TicketCreatedPayload, TicketFailedRetriablePayload, TicketFailedTerminalPayload,
    TicketReadyPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::execution::tickets::{
    NewTicket, PreLeaseFailureTransition, Ticket, TicketState,
};

use crate::ControlPlane;
use crate::cases::begin_immediate_tx;

use super::{append_event, begin_tx, commit_tx};

#[cfg(test)]
#[derive(Default)]
pub(crate) struct MarkReadyTransactionObserver {
    pub(crate) begun: tokio::sync::Notify,
    pub(crate) release: tokio::sync::Notify,
}

#[derive(Debug, Clone)]
pub struct PreLeaseFailureOutcome {
    pub ticket: Ticket,
    pub terminal: bool,
}

impl ControlPlane {
    /// Create a new ticket and emit `ticket.created`.
    ///
    /// # Errors
    /// Propagates `SqliteTicketRepo::create_in_tx` and event-append errors.
    pub async fn create_ticket(&self, input: NewTicket) -> Result<Ticket, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let ticket = self.create_ticket_in_tx(&mut tx, input).await?;
        commit_tx(tx).await?;
        Ok(ticket)
    }

    pub(crate) async fn create_ticket_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewTicket,
    ) -> Result<Ticket, VoomError> {
        let ticket = self.tickets.create_in_tx(tx, input.clone()).await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Ticket,
            Some(ticket.id.0),
            input.created_at,
            Event::TicketCreated(TicketCreatedPayload {
                ticket_id: ticket.id,
                job_id: input.job_id,
                kind: input.kind.clone(),
                priority: input.priority,
                max_attempts: input.max_attempts,
            }),
        )
        .await?;
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
        self.mark_ready_if_unblocked_observed(
            ticket_id,
            now,
            #[cfg(test)]
            None,
        )
        .await
    }

    async fn mark_ready_if_unblocked_observed(
        &self,
        ticket_id: TicketId,
        now: OffsetDateTime,
        #[cfg(test)] observer: Option<&MarkReadyTransactionObserver>,
    ) -> Result<Vec<Ticket>, VoomError> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        #[cfg(test)]
        if let Some(observer) = observer {
            observer.begun.notify_one();
            observer.release.notified().await;
        }
        let promoted = self
            .mark_ready_if_unblocked_in_tx(&mut tx, ticket_id, now)
            .await?;
        commit_tx(tx).await?;
        Ok(promoted)
    }

    pub(crate) async fn mark_ready_if_unblocked_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        ticket_id: TicketId,
        now: OffsetDateTime,
    ) -> Result<Vec<Ticket>, VoomError> {
        let promoted = self
            .tickets
            .mark_ready_if_unblocked_in_tx(tx, ticket_id, now)
            .await?;
        for t in &promoted {
            append_event(
                &self.events,
                tx,
                SubjectType::Ticket,
                Some(t.id.0),
                now,
                Event::TicketReady(TicketReadyPayload { ticket_id: t.id }),
            )
            .await?;
        }
        Ok(promoted)
    }

    /// Record a scheduler/selector failure that happened before a lease was
    /// created. Emits a ticket failure event only; lease-side events are
    /// intentionally absent because no lease exists yet.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing ticket, `Conflict` when the ticket is
    /// not ready or already has a held lease, `Config` for failure classes that
    /// do not belong to the pre-lease selection path, and propagates database
    /// and event-append errors.
    pub async fn record_pre_lease_ticket_failure(
        &self,
        ticket_id: TicketId,
        class: FailureClass,
        now: OffsetDateTime,
    ) -> Result<PreLeaseFailureOutcome, VoomError> {
        let reason = pre_lease_failure_reason(class)?;
        let mut tx = begin_tx(&self.pool).await?;
        let ticket = self
            .tickets
            .get_in_tx(&mut tx, ticket_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("ticket {ticket_id}")))?;
        self.require_no_held_lease(&mut tx, ticket_id).await?;
        if ticket.state != TicketState::Ready {
            return Err(VoomError::Conflict(format!(
                "pre-lease failure rejected: ticket {ticket_id} is {:?}, not ready",
                ticket.state
            )));
        }
        if ticket.next_eligible_at > now {
            return Err(VoomError::Conflict(format!(
                "pre-lease failure rejected: ticket {ticket_id} is not eligible until {}",
                ticket.next_eligible_at
            )));
        }

        let next_attempt = ticket.attempt.checked_add(1).ok_or_else(|| {
            VoomError::Internal(format!(
                "pre-lease failure: ticket {ticket_id} attempt overflow"
            ))
        })?;
        let terminal =
            class == FailureClass::AmbiguousWorkerSelection || next_attempt >= ticket.max_attempts;
        let ticket = self
            .transition_pre_lease_failure_ticket(&mut tx, &ticket, next_attempt, terminal, now)
            .await?;
        self.emit_pre_lease_failure_event(&mut tx, &ticket, terminal, reason, class, now)
            .await?;
        commit_tx(tx).await?;
        Ok(PreLeaseFailureOutcome { ticket, terminal })
    }

    async fn transition_pre_lease_failure_ticket(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        ticket: &Ticket,
        next_attempt: u32,
        terminal: bool,
        now: OffsetDateTime,
    ) -> Result<Ticket, VoomError> {
        let transition = if terminal {
            PreLeaseFailureTransition::Terminal
        } else {
            let mut shot = self.snapshot_rng();
            let backoff = voom_store::repo::execution::tickets::SqliteTicketRepo::default_backoff(
                next_attempt,
                &mut shot,
            );
            PreLeaseFailureTransition::RetryAt(now + backoff)
        };
        self.tickets
            .transition_ready_before_lease_failure_in_tx(
                tx,
                ticket.id,
                ticket.attempt,
                next_attempt,
                transition,
                now,
            )
            .await
    }

    async fn emit_pre_lease_failure_event(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        ticket: &Ticket,
        terminal: bool,
        reason: &str,
        class: FailureClass,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        if terminal {
            // No lease exists on the pre-lease selection-failure path.
            let issue_id = self
                .open_terminal_failure_issue_in_tx(tx, ticket.id, None, class, reason, now)
                .await?;
            append_event(
                &self.events,
                tx,
                SubjectType::Ticket,
                Some(ticket.id.0),
                now,
                Event::TicketFailedTerminal(TicketFailedTerminalPayload {
                    ticket_id: ticket.id,
                    attempt: ticket.attempt,
                    max_attempts: ticket.max_attempts,
                    reason: reason.to_owned(),
                    class,
                    issue_id: Some(issue_id),
                }),
            )
            .await
        } else {
            append_event(
                &self.events,
                tx,
                SubjectType::Ticket,
                Some(ticket.id.0),
                now,
                Event::TicketFailedRetriable(TicketFailedRetriablePayload {
                    ticket_id: ticket.id,
                    attempt: ticket.attempt,
                    max_attempts: ticket.max_attempts,
                    reason: reason.to_owned(),
                    class,
                    next_eligible_at: ticket.next_eligible_at,
                }),
            )
            .await
        }
    }
}

impl ControlPlane {
    async fn require_no_held_lease(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        ticket_id: TicketId,
    ) -> Result<(), VoomError> {
        if self.leases.has_held_for_ticket_in_tx(tx, ticket_id).await? {
            return Err(VoomError::Conflict(format!(
                "pre-lease failure rejected: ticket {ticket_id} has an active lease"
            )));
        }
        Ok(())
    }
}

fn pre_lease_failure_reason(class: FailureClass) -> Result<&'static str, VoomError> {
    match class {
        FailureClass::NoEligibleWorker => Ok("no eligible worker before lease acquisition"),
        FailureClass::AmbiguousWorkerSelection => {
            Ok("ambiguous worker selection before lease acquisition")
        }
        other => Err(VoomError::Config(format!(
            "failure class {other:?} is not supported for pre-lease ticket failure"
        ))),
    }
}

#[cfg(test)]
#[path = "tickets_test.rs"]
mod tests;
