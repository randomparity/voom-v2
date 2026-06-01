//! Lease-lifecycle use cases. Each method composes the `SqliteLeaseRepo` `_in_tx`
//! call with one or more event-append calls inside the same transaction.

use serde_json::Value as JsonValue;
use sqlx::{Sqlite, Transaction};
use time::{Duration, OffsetDateTime};
use voom_core::{FailureClass, LeaseId, TicketId, VoomError};
use voom_events::payload::TicketReadyPayload;
use voom_events::payload::{
    LeaseAcquiredPayload, LeaseExpiredPayload, LeaseForceReleasedPayload, LeaseReleasedPayload,
    TicketFailedRetriablePayload, TicketFailedTerminalPayload, TicketLeasedPayload,
    TicketRequeuedAfterForceReleasePayload, TicketRequeuedAfterLeaseExpiryPayload,
    TicketSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::leases::{ExpireReport, ForceReleaseOutcome, Lease, NewLease};
use voom_store::repo::tickets::TicketState;

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx, require_audit_field};

impl ControlPlane {
    /// Acquire a worker lease. Emits `lease.acquired` + `ticket.leased` in
    /// the same transaction. The `ticket.leased` payload's `attempt` is the
    /// post-update value the repo bumped during acquisition.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn acquire_lease(&self, input: NewLease) -> Result<Lease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let lease = self.acquire_lease_in_tx(&mut tx, input).await?;
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Acquire a worker lease and emit lease/ticket events.
    ///
    /// The caller owns the transaction boundary.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub(crate) async fn acquire_lease_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewLease,
    ) -> Result<Lease, VoomError> {
        let now = input.now;
        let lease = self.leases.acquire_in_tx(tx, input).await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Lease,
            Some(lease.id.0),
            now,
            Event::LeaseAcquired(LeaseAcquiredPayload {
                lease_id: lease.id.0,
                ticket_id: lease.ticket_id.0,
                worker_id: lease.worker_id.0,
                ttl_seconds: lease.ttl_seconds,
                expires_at: lease.expires_at,
            }),
        )
        .await?;
        let ticket = self
            .tickets
            .get_in_tx(tx, lease.ticket_id)
            .await?
            .ok_or_else(|| {
                VoomError::Internal("acquire_lease: ticket vanished mid-tx".to_owned())
            })?;
        append_event(
            &self.events,
            tx,
            SubjectType::Ticket,
            Some(ticket.id.0),
            now,
            Event::TicketLeased(TicketLeasedPayload {
                ticket_id: ticket.id.0,
                lease_id: lease.id.0,
                worker_id: lease.worker_id.0,
                attempt: ticket.attempt,
            }),
        )
        .await?;
        Ok(lease)
    }

    /// Heartbeat a lease. Emits no event per spec §7.5 — heartbeats are
    /// observable via `last_heartbeat_at` and produce too much volume.
    ///
    /// # Errors
    /// Propagates `SqliteLeaseRepo::heartbeat` errors.
    pub async fn heartbeat_lease(
        &self,
        lease_id: LeaseId,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let lease = self
            .heartbeat_lease_in_tx(&mut tx, lease_id, ttl, now)
            .await?;
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Heartbeat a lease inside the caller's transaction. Emits no event.
    ///
    /// # Errors
    /// Propagates `SqliteLeaseRepo::heartbeat_in_tx` errors.
    pub(crate) async fn heartbeat_lease_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        lease_id: LeaseId,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        self.leases.heartbeat_in_tx(tx, lease_id, ttl, now).await
    }

    /// Release a lease successfully. Emits `lease.released` +
    /// `ticket.succeeded`, then walks the released ticket's dependents and
    /// promotes any that are now unblocked (one `ticket.ready` per promoted
    /// row), all inside the same transaction so the parent's `succeeded`
    /// state is visible to the dependency-count check.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn release_lease(
        &self,
        lease_id: LeaseId,
        result: JsonValue,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let lease = self
            .release_lease_in_tx(&mut tx, lease_id, result, now)
            .await?;
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Release a lease successfully, emit lease/ticket events, and promote
    /// newly unblocked dependents.
    ///
    /// The caller owns the transaction boundary.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub(crate) async fn release_lease_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        lease_id: LeaseId,
        result: JsonValue,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let lease = self.leases.release_in_tx(tx, lease_id, result, now).await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Lease,
            Some(lease.id.0),
            now,
            Event::LeaseReleased(LeaseReleasedPayload {
                lease_id: lease.id.0,
                ticket_id: lease.ticket_id.0,
                release_reason: lease
                    .release_reason
                    .map(|r| r.as_str().to_owned())
                    .unwrap_or_default(),
            }),
        )
        .await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Ticket,
            Some(lease.ticket_id.0),
            now,
            Event::TicketSucceeded(TicketSucceededPayload {
                ticket_id: lease.ticket_id.0,
                lease_id: lease.id.0,
            }),
        )
        .await?;
        let dependents = self
            .tickets
            .list_dependents_in_tx(tx, lease.ticket_id)
            .await?;
        for dep in dependents {
            let promoted = self
                .tickets
                .mark_ready_if_unblocked_in_tx(tx, dep.id, now)
                .await?;
            for t in &promoted {
                append_event(
                    &self.events,
                    tx,
                    SubjectType::Ticket,
                    Some(t.id.0),
                    now,
                    Event::TicketReady(TicketReadyPayload { ticket_id: t.id.0 }),
                )
                .await?;
            }
        }
        Ok(lease)
    }

    /// Fail a lease. Emits `lease.released` + (`ticket.failed_retriable` |
    /// `ticket.failed_terminal`) based on the post-update ticket state.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn fail_lease(
        &self,
        lease_id: LeaseId,
        reason: String,
        class: FailureClass,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let lease = self
            .fail_lease_in_tx(&mut tx, lease_id, reason, class, now)
            .await?;
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Fail a lease and emit lease/ticket failure events.
    ///
    /// The caller owns the transaction boundary.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub(crate) async fn fail_lease_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        lease_id: LeaseId,
        reason: String,
        class: FailureClass,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        // Snapshot one u32 from the shared RNG up front. Holding the
        // std Mutex across the awaits that follow would trip the
        // workspace-level `await_holding_lock` lint and the
        // `clippy::await_holding_lock` deny; `fail_in_tx` only consumes
        // a single jitter value per call so the snapshot is safe.
        let mut shot = self.snapshot_rng();
        let lease = self
            .leases
            .fail_in_tx(tx, lease_id, class, now, &*self.clock, &mut shot)
            .await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Lease,
            Some(lease.id.0),
            now,
            Event::LeaseReleased(LeaseReleasedPayload {
                lease_id: lease.id.0,
                ticket_id: lease.ticket_id.0,
                release_reason: lease
                    .release_reason
                    .map(|r| r.as_str().to_owned())
                    .unwrap_or_default(),
            }),
        )
        .await?;
        let ticket = self
            .tickets
            .get_in_tx(tx, lease.ticket_id)
            .await?
            .ok_or_else(|| VoomError::Internal("fail_lease: ticket vanished mid-tx".to_owned()))?;
        match ticket.state {
            TicketState::Ready => {
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
                        reason,
                        class,
                        next_eligible_at: ticket.next_eligible_at,
                    }),
                )
                .await?;
            }
            TicketState::Failed => {
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
                        reason,
                        class,
                        issue_id: None,
                    }),
                )
                .await?;
            }
            other => {
                return Err(VoomError::Internal(format!(
                    "fail_lease: unexpected post-update ticket state {other:?}"
                )));
            }
        }
        Ok(lease)
    }

    /// Expire any held leases whose `expires_at < now`. Walks the repo's
    /// `ExpireReport::pairs` once, emitting one `lease.expired` per row plus
    /// the matching ticket-side event (`ticket.requeued_after_lease_expiry`
    /// for tickets in `requeued_tickets`, else `ticket.failed_terminal`).
    ///
    /// Each call processes at most `LEASE_BATCH_LIMIT` candidates inside a
    /// single transaction so lock-hold time stays bounded under
    /// restart-backlog conditions. The Sprint 6+ daemon drains by
    /// re-invoking `expire_due` until `report.expired_leases.is_empty()`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors. The transaction aborts on
    /// any error and no events are persisted.
    pub async fn expire_due(&self, now: OffsetDateTime) -> Result<ExpireReport, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let report = self.leases.expire_due_in_tx(&mut tx, now).await?;
        for &(lease_id, ticket_id) in &report.pairs {
            self.emit_expire_pair(&mut tx, lease_id, ticket_id, &report, now)
                .await?;
        }
        commit_tx(tx).await?;
        Ok(report)
    }

    async fn emit_expire_pair(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        lease_id: LeaseId,
        ticket_id: TicketId,
        report: &ExpireReport,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        append_event(
            &self.events,
            tx,
            SubjectType::Lease,
            Some(lease_id.0),
            now,
            Event::LeaseExpired(LeaseExpiredPayload {
                lease_id: lease_id.0,
                ticket_id: ticket_id.0,
            }),
        )
        .await?;
        if report.requeued_tickets.contains(&ticket_id) {
            append_event(
                &self.events,
                tx,
                SubjectType::Ticket,
                Some(ticket_id.0),
                now,
                Event::TicketRequeuedAfterLeaseExpiry(TicketRequeuedAfterLeaseExpiryPayload {
                    ticket_id: ticket_id.0,
                    lease_id: lease_id.0,
                }),
            )
            .await?;
        } else {
            // Read attempt/max_attempts straight from the report — the repo
            // already had them in scope when it decided the ticket's fate
            // (see `FailedExpiry` in `voom_store::repo::leases`).
            let failed = report
                .failed_expiries
                .iter()
                .find(|f| f.ticket_id == ticket_id)
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "expire_due: ticket {ticket_id} missing from failed_expiries"
                    ))
                })?;
            append_event(
                &self.events,
                tx,
                SubjectType::Ticket,
                Some(ticket_id.0),
                now,
                Event::TicketFailedTerminal(TicketFailedTerminalPayload {
                    ticket_id: ticket_id.0,
                    attempt: failed.attempt,
                    max_attempts: failed.max_attempts,
                    reason: "lease expired with no retries remaining".to_owned(),
                    // Per spec §10.2: lease-expiry terminal failures
                    // implicitly classify as WorkerCrash.
                    class: FailureClass::WorkerCrash,
                    issue_id: None,
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// Force-release a held lease. Emits `lease.force_released` plus
    /// either `ticket.requeued_after_force_release` (when
    /// `also_requeue = true` succeeded — gated by the repo on
    /// `attempt < max_attempts`) or `ticket.failed_terminal` (when
    /// `also_requeue = false`).
    ///
    /// A retries-exhausted requeue request returns `VoomError::Conflict`
    /// from the repo with no side effects — the lease, ticket, and
    /// event log are all unchanged on rejection. The caller must
    /// retry with `also_requeue = false` if they intend a terminal
    /// force-release.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn force_release_lease(
        &self,
        lease_id: LeaseId,
        actor: String,
        reason: String,
        also_requeue: bool,
        now: OffsetDateTime,
    ) -> Result<ForceReleaseOutcome, VoomError> {
        require_audit_field("actor", &actor)?;
        require_audit_field("reason", &reason)?;
        let mut tx = begin_tx(&self.pool).await?;
        let outcome = self
            .leases
            .force_release_in_tx(&mut tx, lease_id, also_requeue, now)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Lease,
            Some(outcome.lease.id.0),
            now,
            Event::LeaseForceReleased(LeaseForceReleasedPayload {
                lease_id: outcome.lease.id.0,
                ticket_id: outcome.lease.ticket_id.0,
                actor: actor.clone(),
                reason: reason.clone(),
                also_requeue,
            }),
        )
        .await?;
        if outcome.ticket_requeued {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::Ticket,
                Some(outcome.lease.ticket_id.0),
                now,
                Event::TicketRequeuedAfterForceRelease(TicketRequeuedAfterForceReleasePayload {
                    ticket_id: outcome.lease.ticket_id.0,
                    lease_id: outcome.lease.id.0,
                    actor,
                    reason,
                }),
            )
            .await?;
        } else {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::Ticket,
                Some(outcome.lease.ticket_id.0),
                now,
                Event::TicketFailedTerminal(TicketFailedTerminalPayload {
                    ticket_id: outcome.lease.ticket_id.0,
                    attempt: outcome.attempt,
                    max_attempts: outcome.max_attempts,
                    reason: "force-released without requeue".to_owned(),
                    // Per spec §10.2: operator-initiated terminal
                    // force-release implicitly classifies as
                    // UserCancellation.
                    class: FailureClass::UserCancellation,
                    issue_id: None,
                }),
            )
            .await?;
        }
        commit_tx(tx).await?;
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "leases_test.rs"]
mod tests;
