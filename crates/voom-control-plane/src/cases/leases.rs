//! Lease-lifecycle use cases. Each method composes the `LeaseRepo` `_in_tx`
//! call with one or more event-append calls inside the same transaction.

use serde_json::Value as JsonValue;
use sqlx::{Sqlite, Transaction};
use time::{Duration, OffsetDateTime};
use voom_core::{LeaseId, TicketId, VoomError};
use voom_events::payload::{
    LeaseAcquiredPayload, LeaseExpiredPayload, LeaseForceReleasedPayload, LeaseReleasedPayload,
    TicketFailedRetriablePayload, TicketFailedTerminalPayload, TicketLeasedPayload,
    TicketReadyPayload, TicketRequeuedAfterLeaseExpiryPayload, TicketSucceededPayload,
};
use voom_events::{Event, EventEnvelope, EventKind, SubjectType};
use voom_store::repo::events::EventRepo;
use voom_store::repo::leases::{ExpireReport, Lease, LeaseRepo, NewLease};
use voom_store::repo::tickets::{TicketRepo, TicketState};

use crate::ControlPlane;

impl ControlPlane {
    /// Acquire a worker lease. Emits `lease.acquired` + `ticket.leased` in
    /// the same transaction. The `ticket.leased` payload's `attempt` is the
    /// post-update value the repo bumped during acquisition.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn acquire_lease(&self, input: NewLease) -> Result<Lease, VoomError> {
        let mut tx = self.begin_tx().await?;
        let now = input.now;
        let lease = self.leases.acquire_in_tx(&mut tx, input).await?;
        self.append_event(
            &mut tx,
            EventKind::LeaseAcquired,
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
            .get_in_tx(&mut tx, lease.ticket_id)
            .await?
            .ok_or_else(|| {
                VoomError::Internal("acquire_lease: ticket vanished mid-tx".to_owned())
            })?;
        self.append_event(
            &mut tx,
            EventKind::TicketLeased,
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
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Heartbeat a lease. Emits no event per spec §7.5 — heartbeats are
    /// observable via `last_heartbeat_at` and produce too much volume.
    ///
    /// # Errors
    /// Propagates `LeaseRepo::heartbeat` errors.
    pub async fn heartbeat_lease(
        &self,
        lease_id: LeaseId,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        self.leases.heartbeat(lease_id, ttl, now).await
    }

    /// Release a lease successfully. Emits `lease.released` +
    /// `ticket.succeeded` in the same transaction.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn release_lease(
        &self,
        lease_id: LeaseId,
        result: JsonValue,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let mut tx = self.begin_tx().await?;
        let lease = self
            .leases
            .release_in_tx(&mut tx, lease_id, result, now)
            .await?;
        self.append_event(
            &mut tx,
            EventKind::LeaseReleased,
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
        self.append_event(
            &mut tx,
            EventKind::TicketSucceeded,
            SubjectType::Ticket,
            Some(lease.ticket_id.0),
            now,
            Event::TicketSucceeded(TicketSucceededPayload {
                ticket_id: lease.ticket_id.0,
                lease_id: lease.id.0,
            }),
        )
        .await?;
        commit_tx(tx).await?;
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
        retriable: bool,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let mut tx = self.begin_tx().await?;
        let lease = self
            .leases
            .fail_in_tx(&mut tx, lease_id, retriable, now)
            .await?;
        self.append_event(
            &mut tx,
            EventKind::LeaseReleased,
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
            .get_in_tx(&mut tx, lease.ticket_id)
            .await?
            .ok_or_else(|| VoomError::Internal("fail_lease: ticket vanished mid-tx".to_owned()))?;
        match ticket.state {
            TicketState::Ready => {
                self.append_event(
                    &mut tx,
                    EventKind::TicketFailedRetriable,
                    SubjectType::Ticket,
                    Some(ticket.id.0),
                    now,
                    Event::TicketFailedRetriable(TicketFailedRetriablePayload {
                        ticket_id: ticket.id.0,
                        attempt: ticket.attempt,
                        max_attempts: ticket.max_attempts,
                        reason,
                        next_eligible_at: ticket.next_eligible_at,
                    }),
                )
                .await?;
            }
            TicketState::Failed => {
                self.append_event(
                    &mut tx,
                    EventKind::TicketFailedTerminal,
                    SubjectType::Ticket,
                    Some(ticket.id.0),
                    now,
                    Event::TicketFailedTerminal(TicketFailedTerminalPayload {
                        ticket_id: ticket.id.0,
                        attempt: ticket.attempt,
                        max_attempts: ticket.max_attempts,
                        reason,
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
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Expire any held leases whose `expires_at < now`. Walks the repo's
    /// `ExpireReport::pairs` once, emitting one `lease.expired` per row plus
    /// the matching ticket-side event (`ticket.requeued_after_lease_expiry`
    /// for tickets in `requeued_tickets`, else `ticket.failed_terminal`).
    ///
    /// # Errors
    /// Propagates repo and event-append errors. The transaction aborts on
    /// any error and no events are persisted.
    pub async fn expire_due(&self, now: OffsetDateTime) -> Result<ExpireReport, VoomError> {
        let mut tx = self.begin_tx().await?;
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
        self.append_event(
            tx,
            EventKind::LeaseExpired,
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
            self.append_event(
                tx,
                EventKind::TicketRequeuedAfterLeaseExpiry,
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
            let ticket = self
                .tickets
                .get_in_tx(tx, ticket_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal("expire_due: ticket vanished mid-tx".to_owned())
                })?;
            self.append_event(
                tx,
                EventKind::TicketFailedTerminal,
                SubjectType::Ticket,
                Some(ticket.id.0),
                now,
                Event::TicketFailedTerminal(TicketFailedTerminalPayload {
                    ticket_id: ticket.id.0,
                    attempt: ticket.attempt,
                    max_attempts: ticket.max_attempts,
                    reason: "lease expired with no retries remaining".to_owned(),
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// Force-release a held lease. Emits `lease.force_released` +
    /// (`ticket.ready` | `ticket.failed_terminal`) based on `also_requeue`.
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
    ) -> Result<Lease, VoomError> {
        let mut tx = self.begin_tx().await?;
        let lease = self
            .leases
            .force_release_in_tx(&mut tx, lease_id, also_requeue, now)
            .await?;
        self.append_event(
            &mut tx,
            EventKind::LeaseForceReleased,
            SubjectType::Lease,
            Some(lease.id.0),
            now,
            Event::LeaseForceReleased(LeaseForceReleasedPayload {
                lease_id: lease.id.0,
                ticket_id: lease.ticket_id.0,
                actor,
                reason,
                also_requeue,
            }),
        )
        .await?;
        if also_requeue {
            self.append_event(
                &mut tx,
                EventKind::TicketReady,
                SubjectType::Ticket,
                Some(lease.ticket_id.0),
                now,
                Event::TicketReady(TicketReadyPayload {
                    ticket_id: lease.ticket_id.0,
                }),
            )
            .await?;
        } else {
            let ticket = self
                .tickets
                .get_in_tx(&mut tx, lease.ticket_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal("force_release_lease: ticket vanished mid-tx".to_owned())
                })?;
            self.append_event(
                &mut tx,
                EventKind::TicketFailedTerminal,
                SubjectType::Ticket,
                Some(ticket.id.0),
                now,
                Event::TicketFailedTerminal(TicketFailedTerminalPayload {
                    ticket_id: ticket.id.0,
                    attempt: ticket.attempt,
                    max_attempts: ticket.max_attempts,
                    reason: "force-released without requeue".to_owned(),
                }),
            )
            .await?;
        }
        commit_tx(tx).await?;
        Ok(lease)
    }

    async fn begin_tx(&self) -> Result<Transaction<'_, Sqlite>, VoomError> {
        self.pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))
    }

    async fn append_event(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        kind: EventKind,
        subject_type: SubjectType,
        subject_id: Option<u64>,
        occurred_at: OffsetDateTime,
        payload: Event,
    ) -> Result<(), VoomError> {
        self.events
            .append_in_tx(
                tx,
                EventEnvelope {
                    kind,
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
}

async fn commit_tx(tx: Transaction<'_, Sqlite>) -> Result<(), VoomError> {
    tx.commit()
        .await
        .map_err(|e| VoomError::Database(format!("commit: {e}")))
}

#[cfg(test)]
#[path = "leases_test.rs"]
mod tests;
