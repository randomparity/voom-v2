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
use voom_store::repo::tickets::{NewTicket, Ticket, TicketState};

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx};

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
        let ticket = self.tickets.create_in_tx(&mut tx, input.clone()).await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Ticket,
            Some(ticket.id.0),
            input.created_at,
            Event::TicketCreated(TicketCreatedPayload {
                ticket_id: ticket.id.0,
                job_id: input.job_id.map(|j| j.0),
                kind: input.kind.clone(),
                priority: input.priority,
                max_attempts: input.max_attempts,
            }),
        )
        .await?;
        commit_tx(tx).await?;
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
        let mut tx = begin_tx(&self.pool).await?;
        let promoted = self
            .tickets
            .mark_ready_if_unblocked_in_tx(&mut tx, ticket_id, now)
            .await?;
        for t in &promoted {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::Ticket,
                Some(t.id.0),
                now,
                Event::TicketReady(TicketReadyPayload { ticket_id: t.id.0 }),
            )
            .await?;
        }
        commit_tx(tx).await?;
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
        require_no_held_lease(&mut tx, ticket_id).await?;
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
        let ticket_id_i = sqlite_i64(ticket.id.0, "ticket id")?;
        let now_str = iso8601(now)?;
        let updated = if terminal {
            terminal_fail_ready_ticket(tx, ticket_id_i, ticket.attempt, next_attempt, &now_str, now)
                .await?
        } else {
            let mut shot = self.snapshot_rng();
            let backoff = voom_store::repo::tickets::SqliteTicketRepo::default_backoff(
                next_attempt,
                &*self.clock,
                &mut shot,
            );
            requeue_ready_ticket(
                tx,
                ticket_id_i,
                ticket.attempt,
                next_attempt,
                &now_str,
                now + backoff,
            )
            .await?
        };
        if updated.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "pre-lease failure rejected: ticket {} changed concurrently",
                ticket.id
            )));
        }
        self.tickets.get_in_tx(tx, ticket.id).await?.ok_or_else(|| {
            VoomError::Internal(format!(
                "pre-lease failure: ticket {} vanished mid-tx",
                ticket.id
            ))
        })
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
                    ticket_id: ticket.id.0,
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
                    ticket_id: ticket.id.0,
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

async fn require_no_held_lease(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ticket_id: TicketId,
) -> Result<(), VoomError> {
    let ticket_id_i = sqlite_i64(ticket_id.0, "ticket id")?;
    let held_lease: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM leases WHERE ticket_id = ? AND state = 'held' LIMIT 1")
            .bind(ticket_id_i)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("pre-lease held lease probe", e))?;
    if held_lease.is_some() {
        return Err(VoomError::Conflict(format!(
            "pre-lease failure rejected: ticket {ticket_id} has an active lease"
        )));
    }
    Ok(())
}

async fn terminal_fail_ready_ticket(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ticket_id_i: i64,
    previous_attempt: u32,
    next_attempt: u32,
    now_str: &str,
    now: OffsetDateTime,
) -> Result<sqlx::sqlite::SqliteQueryResult, VoomError> {
    sqlx::query(
        "UPDATE tickets SET state = 'failed', state_changed_at = ?, \
         attempt = ?, epoch = epoch + 1 WHERE id = ? AND state = 'ready' \
         AND attempt = ? AND next_eligible_at <= ?",
    )
    .bind(now_str)
    .bind(i64::from(next_attempt))
    .bind(ticket_id_i)
    .bind(i64::from(previous_attempt))
    .bind(iso8601(now)?)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("pre-lease terminal fail", e))
}

async fn requeue_ready_ticket(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ticket_id_i: i64,
    previous_attempt: u32,
    next_attempt: u32,
    now_str: &str,
    next_eligible_at: OffsetDateTime,
) -> Result<sqlx::sqlite::SqliteQueryResult, VoomError> {
    sqlx::query(
        "UPDATE tickets SET state = 'ready', state_changed_at = ?, \
         attempt = ?, next_eligible_at = ?, epoch = epoch + 1 \
         WHERE id = ? AND state = 'ready' AND attempt = ? AND next_eligible_at <= ?",
    )
    .bind(now_str)
    .bind(i64::from(next_attempt))
    .bind(iso8601(next_eligible_at)?)
    .bind(ticket_id_i)
    .bind(i64::from(previous_attempt))
    .bind(now_str)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("pre-lease requeue", e))
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

fn sqlite_i64(value: u64, field: &str) -> Result<i64, VoomError> {
    i64::try_from(value).map_err(|e| {
        VoomError::database_context(format!("{field} {value} does not fit SQLite i64"), e)
    })
}

fn iso8601(t: OffsetDateTime) -> Result<String, VoomError> {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::Internal(format!("format iso8601: {e}")))
}

#[cfg(test)]
#[path = "tickets_test.rs"]
mod tests;
