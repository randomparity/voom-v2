//! Asset use-lease use cases (M3 Phase 1). Each method composes a
//! `UseLeaseRepo` `_in_tx` write with `EventRepo::append_in_tx` inside
//! the same transaction so the row write and its event row are atomic.
//!
//! `heartbeat_use_lease` is the single exception: per sprint-1 design
//! §9.2 last paragraph, heartbeats emit no event in Sprint 1 (volume is
//! too high; `last_heartbeat_at` is the observable state instead).

use time::OffsetDateTime;
use voom_core::{FileLocationId, UseLeaseId, VoomError};
use voom_events::payload::{
    UseLeaseAcquiredPayload, UseLeaseExpiredPayload, UseLeaseForceReleasedPayload,
    UseLeaseReanchoredByMovePayload, UseLeaseRecoveredStaleIssuerPayload, UseLeaseReleasedPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::use_leases::{
    ExpireReport, NewUseLease, ReanchorReport, UseLease, UseLeaseReleaseReason, UseLeaseRepo,
};

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx};

/// Reject empty or whitespace-only audit strings. The `force_release` and
/// `recover_stale_issuer` paths exist specifically to record operator intent
/// (sprint-1 design §9.2) — a blank actor or reason would terminate a lease
/// and leave an audit row that carries no operator information.
fn require_audit_field(name: &str, value: &str) -> Result<(), VoomError> {
    if value.trim().is_empty() {
        return Err(VoomError::Config(format!(
            "{name} must not be empty or whitespace"
        )));
    }
    Ok(())
}

impl ControlPlane {
    /// Acquire an `AssetUseLease`. Emits `use_lease.acquired`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn acquire_use_lease(&self, input: NewUseLease) -> Result<UseLease, VoomError> {
        let acquired_at = input.acquired_at;
        let mut tx = begin_tx(&self.pool).await?;
        let lease = self.use_leases.acquire_in_tx(&mut tx, input).await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::AssetUseLease,
            Some(lease.id.0),
            acquired_at,
            Event::UseLeaseAcquired(UseLeaseAcquiredPayload {
                lease_id: lease.id.0,
                kind: lease.kind.as_str().to_owned(),
                scope_type: lease.scope.type_str().to_owned(),
                scope_id: lease.scope.id_u64(),
                issuer_kind: lease.issuer_kind.as_str().to_owned(),
                issuer_ref: lease.issuer_ref.clone(),
                blocking_mode: lease.blocking_mode.as_str().to_owned(),
                ttl_bound: lease.ttl_bound,
                acquired_at: lease.acquired_at,
                expires_at: lease.expires_at,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Heartbeat a use lease — extends `expires_at` by the original TTL.
    /// Emits no event in Sprint 1 (per sprint-1 design §9.2 last paragraph:
    /// heartbeat volume is too high; `last_heartbeat_at` is the observable).
    ///
    /// # Errors
    /// Propagates `UseLeaseRepo::heartbeat` errors.
    pub async fn heartbeat_use_lease(
        &self,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self
            .use_leases
            .heartbeat_in_tx(&mut tx, lease_id, now)
            .await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    /// Release a use lease with the given reason. Emits `use_lease.released`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn release_use_lease(
        &self,
        lease_id: UseLeaseId,
        reason: UseLeaseReleaseReason,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let lease = self
            .use_leases
            .release_in_tx(&mut tx, lease_id, reason, now)
            .await?;
        let released_at = lease.released_at.unwrap_or(now);
        append_event(
            &self.events,
            &mut tx,
            SubjectType::AssetUseLease,
            Some(lease.id.0),
            now,
            Event::UseLeaseReleased(UseLeaseReleasedPayload {
                lease_id: lease.id.0,
                release_reason: reason.as_str().to_owned(),
                released_at,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Force-release a use lease (operator action). Emits
    /// `use_lease.force_released` with the actor and reason on the payload.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn force_release_use_lease(
        &self,
        lease_id: UseLeaseId,
        actor: String,
        reason: String,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        require_audit_field("actor", &actor)?;
        require_audit_field("reason", &reason)?;
        let mut tx = begin_tx(&self.pool).await?;
        let lease = self
            .use_leases
            .force_release_in_tx(&mut tx, lease_id, now)
            .await?;
        let released_at = lease.released_at.unwrap_or(now);
        append_event(
            &self.events,
            &mut tx,
            SubjectType::AssetUseLease,
            Some(lease.id.0),
            now,
            Event::UseLeaseForceReleased(UseLeaseForceReleasedPayload {
                lease_id: lease.id.0,
                actor,
                reason,
                released_at,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Expire all TTL-bound use leases whose `expires_at < now`. Emits one
    /// `use_lease.expired` per affected lease, all inside the same transaction
    /// as the bulk update.
    ///
    /// # Errors
    /// Propagates repo and event-append errors. The transaction aborts on any
    /// error and no events are persisted.
    pub async fn expire_due_use_leases(
        &self,
        now: OffsetDateTime,
    ) -> Result<ExpireReport, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let report = self.use_leases.expire_due_in_tx(&mut tx, now).await?;
        for &lease_id in &report.expired {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::AssetUseLease,
                Some(lease_id.0),
                now,
                Event::UseLeaseExpired(UseLeaseExpiredPayload {
                    lease_id: lease_id.0,
                    released_at: now,
                }),
            )
            .await?;
        }
        commit_tx(tx).await?;
        Ok(report)
    }

    /// Recover a manual lock whose issuer is known to be gone. Emits
    /// `use_lease.recovered_stale_issuer` with the actor and reason on the payload.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn recover_use_lease_stale_issuer(
        &self,
        lease_id: UseLeaseId,
        actor: String,
        reason: String,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        require_audit_field("actor", &actor)?;
        require_audit_field("reason", &reason)?;
        let mut tx = begin_tx(&self.pool).await?;
        let lease = self
            .use_leases
            .recover_stale_issuer_in_tx(&mut tx, lease_id, now)
            .await?;
        let released_at = lease.released_at.unwrap_or(now);
        append_event(
            &self.events,
            &mut tx,
            SubjectType::AssetUseLease,
            Some(lease.id.0),
            now,
            Event::UseLeaseRecoveredStaleIssuer(UseLeaseRecoveredStaleIssuerPayload {
                lease_id: lease.id.0,
                actor,
                reason,
                released_at,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(lease)
    }

    /// Re-anchor all live `Location`-scoped use leases from a retired
    /// `FileLocation` to its replacement. Emits one
    /// `use_lease.reanchored_by_move` per re-anchored lease, all inside the
    /// same transaction as the bulk update.
    ///
    /// # Errors
    /// Propagates repo and event-append errors. The transaction aborts on any
    /// error and no events are persisted.
    pub async fn reanchor_use_leases_on_move(
        &self,
        retired: FileLocationId,
        new: FileLocationId,
        now: OffsetDateTime,
    ) -> Result<ReanchorReport, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let report = self
            .use_leases
            .reanchor_on_move_in_tx(&mut tx, retired, new, now)
            .await?;
        for &lease_id in &report.reanchored {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::AssetUseLease,
                Some(lease_id.0),
                now,
                Event::UseLeaseReanchoredByMove(UseLeaseReanchoredByMovePayload {
                    lease_id: lease_id.0,
                    retired_location_id: retired.0,
                    new_location_id: new.0,
                    reanchored_at: now,
                }),
            )
            .await?;
        }
        commit_tx(tx).await?;
        Ok(report)
    }
}

#[cfg(test)]
#[path = "use_leases_test.rs"]
mod tests;
