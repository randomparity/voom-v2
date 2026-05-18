//! `LeaseRepo` — worker-execution lease lifecycle.

use async_trait::async_trait;
use rand::RngCore;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::{Duration, OffsetDateTime};
use voom_core::{Clock, FailureClass, LeaseId, TicketId, VoomError, WorkerId};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};
use super::tickets::{SqliteTicketRepo, TicketRepo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseState {
    Held,
    Released,
    Expired,
    ForceReleased,
}

impl LeaseState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Released => "released",
            Self::Expired => "expired",
            Self::ForceReleased => "force_released",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "held" => Ok(Self::Held),
            "released" => Ok(Self::Released),
            "expired" => Ok(Self::Expired),
            "force_released" => Ok(Self::ForceReleased),
            other => Err(VoomError::Database(format!(
                "leases.state {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseReason {
    Released,
    FailedRetriable,
    FailedTerminal,
    IssuerLost,
    ForceReleased,
}

impl ReleaseReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::FailedRetriable => "failed_retriable",
            Self::FailedTerminal => "failed_terminal",
            Self::IssuerLost => "issuer_lost",
            Self::ForceReleased => "force_released",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "released" => Ok(Self::Released),
            "failed_retriable" => Ok(Self::FailedRetriable),
            "failed_terminal" => Ok(Self::FailedTerminal),
            "issuer_lost" => Ok(Self::IssuerLost),
            "force_released" => Ok(Self::ForceReleased),
            other => Err(VoomError::Database(format!(
                "leases.release_reason {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewLease {
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub ttl: Duration,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct Lease {
    pub id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub state: LeaseState,
    pub acquired_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub last_heartbeat_at: OffsetDateTime,
    pub ttl_seconds: i64,
    pub release_reason: Option<ReleaseReason>,
    pub released_at: Option<OffsetDateTime>,
    pub epoch: u64,
}

/// Outcome of `force_release_in_tx` — surfaces the post-update ticket fate
/// so the case handler can emit `TicketReady` or `TicketFailedTerminal`
/// based on what actually happened, not just the caller's `also_requeue`
/// flag. `also_requeue` is suppressed when the ticket has no attempts
/// remaining (the caller asked for requeue but the ticket is out of
/// retries, so it's parked in `failed` instead — same pattern sibling
/// `fail_in_tx` / `expire_due_in_tx` already use).
#[derive(Debug, Clone)]
pub struct ForceReleaseOutcome {
    pub lease: Lease,
    pub ticket_requeued: bool,
    pub attempt: u32,
    pub max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpireReport {
    /// All expired leases, in id-order.
    pub expired_leases: Vec<LeaseId>,
    /// Tickets whose lease expired and were requeued for retry.
    pub requeued_tickets: Vec<TicketId>,
    /// Tickets whose lease expired and exhausted all attempts.
    pub failed_tickets: Vec<TicketId>,
    /// Per-row (`lease_id`, `ticket_id`) pairs in the order they were processed.
    /// Lets the `ControlPlane` emit `lease.expired` events whose payload
    /// carries the matching `ticket_id`, and
    /// `ticket.requeued_after_lease_expiry` / `ticket.failed_terminal`
    /// whose payload carries the matching `lease_id`. Each pair classifies
    /// as requeued or failed depending on which of `requeued_tickets` /
    /// `failed_tickets` the `ticket_id` appears in.
    pub pairs: Vec<(LeaseId, TicketId)>,
}

#[async_trait]
pub trait LeaseRepo: Repository {
    async fn acquire_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewLease,
    ) -> Result<Lease, VoomError>;
    async fn acquire(&self, input: NewLease) -> Result<Lease, VoomError>;

    async fn heartbeat_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: LeaseId,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError>;
    async fn heartbeat(
        &self,
        lease_id: LeaseId,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError>;

    async fn release_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: LeaseId,
        result: JsonValue,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError>;
    async fn release(
        &self,
        lease_id: LeaseId,
        result: JsonValue,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError>;

    async fn fail_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: LeaseId,
        class: FailureClass,
        now: OffsetDateTime,
        clock: &dyn Clock,
        rng: &mut (dyn RngCore + Send),
    ) -> Result<Lease, VoomError>;
    async fn fail(
        &self,
        lease_id: LeaseId,
        class: FailureClass,
        now: OffsetDateTime,
        clock: &dyn Clock,
        rng: &mut (dyn RngCore + Send),
    ) -> Result<Lease, VoomError>;

    async fn expire_due_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        now: OffsetDateTime,
    ) -> Result<ExpireReport, VoomError>;
    async fn expire_due(&self, now: OffsetDateTime) -> Result<ExpireReport, VoomError>;

    async fn force_release_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: LeaseId,
        also_requeue: bool,
        now: OffsetDateTime,
    ) -> Result<ForceReleaseOutcome, VoomError>;
    async fn force_release(
        &self,
        lease_id: LeaseId,
        also_requeue: bool,
        now: OffsetDateTime,
    ) -> Result<ForceReleaseOutcome, VoomError>;

    async fn get(&self, id: LeaseId) -> Result<Option<Lease>, VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqliteLeaseRepo {
    pool: SqlitePool,
}

impl SqliteLeaseRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteLeaseRepo {}

#[async_trait]
impl LeaseRepo for SqliteLeaseRepo {
    // Capability / grant / deny / max_parallel gating is deferred to
    // Sprint 3 (policy) and Sprint 4 (remote workers). The supporting
    // tables exist now so Sprint 1 use cases can populate them. See
    // docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md §7.5.
    async fn acquire_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewLease,
    ) -> Result<Lease, VoomError> {
        let ttl_secs = input.ttl.whole_seconds();
        if ttl_secs <= 0 {
            return Err(VoomError::Config(format!(
                "ttl must be positive, got {ttl_secs}s"
            )));
        }
        // Worker must exist and not be retired. Retired workers must never
        // acquire — checked here so the worker lifecycle is an effective
        // trust boundary. CHECK constraint on workers.status guarantees the
        // returned string is one of the four-value vocab.
        let worker_status: Option<String> =
            sqlx::query_scalar("SELECT status FROM workers WHERE id = ?")
                .bind(i64_from_u64(input.worker_id.0))
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| VoomError::Database(format!("workers status read: {e}")))?;
        let status = worker_status
            .ok_or_else(|| VoomError::NotFound(format!("worker {}", input.worker_id)))?;
        if status == "retired" {
            return Err(VoomError::Conflict(format!(
                "acquire rejected: worker {} retired",
                input.worker_id
            )));
        }
        let now_str = iso8601(input.now)?;
        // Promote ticket: assert ready + eligible + retries remain; bump attempt.
        let res = sqlx::query(
            "UPDATE tickets \
             SET state = 'leased', state_changed_at = ?, attempt = attempt + 1, \
                 epoch = epoch + 1 \
             WHERE id = ? AND state = 'ready' AND next_eligible_at <= ? \
                   AND attempt < max_attempts",
        )
        .bind(&now_str)
        .bind(i64_from_u64(input.ticket_id.0))
        .bind(&now_str)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("tickets transition to leased: {e}")))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "acquire rejected for ticket {}: not ready, not eligible, or out of attempts",
                input.ticket_id
            )));
        }
        // Insert lease.
        let expires = input.now + input.ttl;
        let expires_str = iso8601(expires)?;
        let res2 = sqlx::query(
            "INSERT INTO leases \
             (ticket_id, worker_id, state, acquired_at, expires_at, \
              last_heartbeat_at, ttl_seconds) \
             VALUES (?, ?, 'held', ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.ticket_id.0))
        .bind(i64_from_u64(input.worker_id.0))
        .bind(&now_str)
        .bind(&expires_str)
        .bind(&now_str)
        .bind(ttl_secs)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("leases insert: {e}")))?;
        get_lease_in_tx(tx, LeaseId(u64_from_i64(res2.last_insert_rowid())))
            .await?
            .ok_or_else(|| VoomError::Internal("acquire: post-insert get vanished".to_owned()))
    }

    async fn acquire(&self, input: NewLease) -> Result<Lease, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.acquire_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn heartbeat_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: LeaseId,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let now_str = iso8601(now)?;
        let expires_str = iso8601(now + ttl)?;
        let res = sqlx::query(
            "UPDATE leases SET last_heartbeat_at = ?, expires_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND state = 'held'",
        )
        .bind(&now_str)
        .bind(&expires_str)
        .bind(i64_from_u64(lease_id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("leases heartbeat: {e}")))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "heartbeat rejected: lease {lease_id} not held"
            )));
        }
        get_lease_in_tx(tx, lease_id)
            .await?
            .ok_or_else(|| VoomError::Internal("heartbeat: post-update get vanished".to_owned()))
    }

    async fn heartbeat(
        &self,
        lease_id: LeaseId,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.heartbeat_in_tx(&mut tx, lease_id, ttl, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn release_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: LeaseId,
        result: JsonValue,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let now_str = iso8601(now)?;
        let lease = get_lease_in_tx(tx, lease_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("lease {lease_id}")))?;
        if lease.state != LeaseState::Held {
            return Err(VoomError::Conflict(format!(
                "release rejected: lease {lease_id} not held (state {:?})",
                lease.state
            )));
        }
        // Transition lease. Row-count gate catches the racy window where a
        // concurrent writer flipped the lease state between the read above
        // and this update.
        let lease_res = sqlx::query(
            "UPDATE leases \
             SET state = 'released', release_reason = ?, released_at = ?, \
                 epoch = epoch + 1 \
             WHERE id = ? AND state = 'held'",
        )
        .bind(ReleaseReason::Released.as_str())
        .bind(&now_str)
        .bind(i64_from_u64(lease_id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("leases release: {e}")))?;
        if lease_res.rows_affected() != 1 {
            tracing::warn!(
                lease_id = i64_from_u64(lease_id.0),
                "release aborting: lease no longer held"
            );
            return Err(VoomError::Conflict(format!(
                "release rejected: lease {lease_id} no longer held"
            )));
        }
        // Transition ticket.
        let result_json = serialize_json(&result, "result")?;
        let ticket_res = sqlx::query(
            "UPDATE tickets SET state = 'succeeded', result = ?, \
             state_changed_at = ?, epoch = epoch + 1 WHERE id = ? AND state = 'leased'",
        )
        .bind(result_json)
        .bind(&now_str)
        .bind(i64_from_u64(lease.ticket_id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("tickets release: {e}")))?;
        if ticket_res.rows_affected() != 1 {
            tracing::warn!(
                lease_id = i64_from_u64(lease_id.0),
                ticket_id = i64_from_u64(lease.ticket_id.0),
                "release aborting: ticket no longer leased"
            );
            return Err(VoomError::Conflict(format!(
                "release rejected: ticket {} not in expected state",
                lease.ticket_id
            )));
        }
        get_lease_in_tx(tx, lease_id)
            .await?
            .ok_or_else(|| VoomError::Internal("release: post-update get vanished".to_owned()))
    }

    async fn release(
        &self,
        lease_id: LeaseId,
        result: JsonValue,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.release_in_tx(&mut tx, lease_id, result, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn fail_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: LeaseId,
        class: FailureClass,
        now: OffsetDateTime,
        clock: &dyn Clock,
        rng: &mut (dyn RngCore + Send),
    ) -> Result<Lease, VoomError> {
        let lease = get_lease_in_tx(tx, lease_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("lease {lease_id}")))?;
        if lease.state != LeaseState::Held {
            return Err(VoomError::Conflict(format!(
                "fail rejected: lease {lease_id} not held"
            )));
        }
        // Inspect the ticket: how many attempts remain after this one?
        let (attempt, max_attempts): (i64, i64) =
            sqlx::query_as("SELECT attempt, max_attempts FROM tickets WHERE id = ?")
                .bind(i64_from_u64(lease.ticket_id.0))
                .fetch_one(&mut **tx)
                .await
                .map_err(|e| VoomError::Database(format!("tickets read: {e}")))?;
        let attempts_remain = attempt < max_attempts;
        let retriable = class.is_retriable();
        let now_str = iso8601(now)?;
        let release_reason = if retriable && attempts_remain {
            ReleaseReason::FailedRetriable
        } else {
            ReleaseReason::FailedTerminal
        };
        // Transition lease to released with the matching reason.
        let lease_res = sqlx::query(
            "UPDATE leases \
             SET state = 'released', release_reason = ?, released_at = ?, \
                 epoch = epoch + 1 \
             WHERE id = ? AND state = 'held'",
        )
        .bind(release_reason.as_str())
        .bind(&now_str)
        .bind(i64_from_u64(lease_id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("leases release on fail: {e}")))?;
        if lease_res.rows_affected() != 1 {
            tracing::warn!(
                lease_id = i64_from_u64(lease_id.0),
                "fail aborting: lease no longer held"
            );
            return Err(VoomError::Conflict(format!(
                "fail rejected: lease {lease_id} no longer held"
            )));
        }
        // Transition ticket: ready (with backoff) or failed.
        if retriable && attempts_remain {
            // attempt is already incremented to reflect "this dispatch"; backoff
            // factor is the current attempt number per §7.5.
            let attempt_u32 = u32_from_i64(attempt)?;
            let next_eligible = now + SqliteTicketRepo::default_backoff(attempt_u32, clock, rng);
            let ticket_res = sqlx::query(
                "UPDATE tickets SET state = 'ready', state_changed_at = ?, \
                 next_eligible_at = ?, epoch = epoch + 1 \
                 WHERE id = ? AND state = 'leased'",
            )
            .bind(&now_str)
            .bind(iso8601(next_eligible)?)
            .bind(i64_from_u64(lease.ticket_id.0))
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("tickets requeue: {e}")))?;
            if ticket_res.rows_affected() != 1 {
                tracing::warn!(
                    lease_id = i64_from_u64(lease_id.0),
                    ticket_id = i64_from_u64(lease.ticket_id.0),
                    "fail aborting: ticket no longer leased on requeue"
                );
                return Err(VoomError::Conflict(format!(
                    "fail rejected (retriable): ticket {} not in expected state",
                    lease.ticket_id
                )));
            }
        } else {
            let ticket_res = sqlx::query(
                "UPDATE tickets SET state = 'failed', state_changed_at = ?, \
                 epoch = epoch + 1 WHERE id = ? AND state = 'leased'",
            )
            .bind(&now_str)
            .bind(i64_from_u64(lease.ticket_id.0))
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("tickets fail terminal: {e}")))?;
            if ticket_res.rows_affected() != 1 {
                tracing::warn!(
                    lease_id = i64_from_u64(lease_id.0),
                    ticket_id = i64_from_u64(lease.ticket_id.0),
                    "fail aborting: ticket no longer leased on terminal fail"
                );
                return Err(VoomError::Conflict(format!(
                    "fail rejected (terminal): ticket {} not in expected state",
                    lease.ticket_id
                )));
            }
        }
        get_lease_in_tx(tx, lease_id)
            .await?
            .ok_or_else(|| VoomError::Internal("fail: post-update get vanished".to_owned()))
    }

    async fn fail(
        &self,
        lease_id: LeaseId,
        class: FailureClass,
        now: OffsetDateTime,
        clock: &dyn Clock,
        rng: &mut (dyn RngCore + Send),
    ) -> Result<Lease, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self
            .fail_in_tx(&mut tx, lease_id, class, now, clock, rng)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn expire_due_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        now: OffsetDateTime,
    ) -> Result<ExpireReport, VoomError> {
        let now_str = iso8601(now)?;
        // Find candidates first so we can return their IDs in the report.
        let rows = sqlx::query(
            "SELECT id, ticket_id FROM leases \
             WHERE state = 'held' AND expires_at < ? \
             ORDER BY id ASC",
        )
        .bind(&now_str)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("expire_due scan: {e}")))?;
        let mut report = ExpireReport {
            expired_leases: Vec::new(),
            requeued_tickets: Vec::new(),
            failed_tickets: Vec::new(),
            pairs: Vec::new(),
        };
        for row in &rows {
            let lease_id_i: i64 = row.try_get("id").map_err(|e| map_row_err("leases", &e))?;
            let ticket_id_i: i64 = row
                .try_get("ticket_id")
                .map_err(|e| map_row_err("leases", &e))?;
            let lease_id = LeaseId(u64_from_i64(lease_id_i));
            let ticket_id = TicketId(u64_from_i64(ticket_id_i));
            // Mark lease expired.
            let lease_res = sqlx::query(
                "UPDATE leases SET state = 'expired', release_reason = ?, \
                 released_at = ?, epoch = epoch + 1 \
                 WHERE id = ? AND state = 'held'",
            )
            .bind(ReleaseReason::IssuerLost.as_str())
            .bind(&now_str)
            .bind(lease_id_i)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("lease expire: {e}")))?;
            if lease_res.rows_affected() != 1 {
                tracing::warn!(
                    lease_id = lease_id_i,
                    ticket_id = ticket_id_i,
                    "expire_due aborting: lease no longer held"
                );
                return Err(VoomError::Conflict(format!(
                    "expire_due aborted: lease {lease_id} no longer held"
                )));
            }
            // Decide ticket fate.
            let (attempt, max_attempts): (i64, i64) =
                sqlx::query_as("SELECT attempt, max_attempts FROM tickets WHERE id = ?")
                    .bind(ticket_id_i)
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(|e| VoomError::Database(format!("ticket lookup: {e}")))?;
            if attempt < max_attempts {
                let ticket_res = sqlx::query(
                    "UPDATE tickets SET state = 'ready', state_changed_at = ?, \
                     epoch = epoch + 1 WHERE id = ? AND state = 'leased'",
                )
                .bind(&now_str)
                .bind(ticket_id_i)
                .execute(&mut **tx)
                .await
                .map_err(|e| VoomError::Database(format!("ticket requeue: {e}")))?;
                if ticket_res.rows_affected() != 1 {
                    tracing::warn!(
                        lease_id = lease_id_i,
                        ticket_id = ticket_id_i,
                        "expire_due aborting: ticket not leased on requeue"
                    );
                    return Err(VoomError::Conflict(format!(
                        "expire_due aborted: ticket {ticket_id} not leased on requeue"
                    )));
                }
                report.requeued_tickets.push(ticket_id);
            } else {
                let ticket_res = sqlx::query(
                    "UPDATE tickets SET state = 'failed', state_changed_at = ?, \
                     epoch = epoch + 1 WHERE id = ? AND state = 'leased'",
                )
                .bind(&now_str)
                .bind(ticket_id_i)
                .execute(&mut **tx)
                .await
                .map_err(|e| VoomError::Database(format!("ticket fail: {e}")))?;
                if ticket_res.rows_affected() != 1 {
                    tracing::warn!(
                        lease_id = lease_id_i,
                        ticket_id = ticket_id_i,
                        "expire_due aborting: ticket not leased on terminal fail"
                    );
                    return Err(VoomError::Conflict(format!(
                        "expire_due aborted: ticket {ticket_id} not leased on fail"
                    )));
                }
                report.failed_tickets.push(ticket_id);
            }
            report.expired_leases.push(lease_id);
            report.pairs.push((lease_id, ticket_id));
        }
        Ok(report)
    }

    async fn expire_due(&self, now: OffsetDateTime) -> Result<ExpireReport, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.expire_due_in_tx(&mut tx, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn force_release_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: LeaseId,
        also_requeue: bool,
        now: OffsetDateTime,
    ) -> Result<ForceReleaseOutcome, VoomError> {
        let lease = get_lease_in_tx(tx, lease_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("lease {lease_id}")))?;
        if lease.state != LeaseState::Held {
            return Err(VoomError::Conflict(format!(
                "force_release rejected: lease {lease_id} not held"
            )));
        }
        let (attempt, max_attempts): (i64, i64) =
            sqlx::query_as("SELECT attempt, max_attempts FROM tickets WHERE id = ?")
                .bind(i64_from_u64(lease.ticket_id.0))
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| VoomError::Database(format!("ticket attempts probe: {e}")))?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "force_release: ticket {} vanished",
                        lease.ticket_id
                    ))
                })?;
        // Operator asked for requeue but the ticket is already out of
        // attempts: refuse the call entirely. Promoting back to `ready`
        // would strand the ticket — `acquire` refuses it (out of
        // attempts) and no held lease remains to expire — and
        // demote-to-terminal would mask the operator's request. The
        // caller must explicitly pass `also_requeue = false` if they
        // intend a terminal force-release.
        if also_requeue && attempt >= max_attempts {
            return Err(VoomError::Conflict(format!(
                "force_release requeue rejected: ticket {tid} attempt {a} >= \
                 max_attempts {m}; use also_requeue = false",
                tid = lease.ticket_id,
                a = attempt,
                m = max_attempts,
            )));
        }
        let ticket_requeued = also_requeue;
        let now_str = iso8601(now)?;
        let lease_res = sqlx::query(
            "UPDATE leases \
             SET state = 'force_released', release_reason = ?, \
                 released_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND state = 'held'",
        )
        .bind(ReleaseReason::ForceReleased.as_str())
        .bind(&now_str)
        .bind(i64_from_u64(lease_id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("lease force_release: {e}")))?;
        if lease_res.rows_affected() != 1 {
            tracing::warn!(
                lease_id = i64_from_u64(lease_id.0),
                "force_release aborting: lease no longer held"
            );
            return Err(VoomError::Conflict(format!(
                "force_release rejected: lease {lease_id} no longer held"
            )));
        }
        // On requeue, set next_eligible_at = now (operator-driven, no
        // backoff). On terminal, the column is irrelevant.
        let ticket_res = if ticket_requeued {
            sqlx::query(
                "UPDATE tickets SET state = 'ready', state_changed_at = ?, \
                 next_eligible_at = ?, epoch = epoch + 1 \
                 WHERE id = ? AND state = 'leased'",
            )
            .bind(&now_str)
            .bind(&now_str)
            .bind(i64_from_u64(lease.ticket_id.0))
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("tickets force_release: {e}")))?
        } else {
            sqlx::query(
                "UPDATE tickets SET state = 'failed', state_changed_at = ?, \
                 epoch = epoch + 1 WHERE id = ? AND state = 'leased'",
            )
            .bind(&now_str)
            .bind(i64_from_u64(lease.ticket_id.0))
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("tickets force_release: {e}")))?
        };
        if ticket_res.rows_affected() != 1 {
            tracing::warn!(
                lease_id = i64_from_u64(lease_id.0),
                ticket_id = i64_from_u64(lease.ticket_id.0),
                "force_release aborting: ticket no longer leased"
            );
            return Err(VoomError::Conflict(format!(
                "force_release rejected: ticket {} not in expected state",
                lease.ticket_id
            )));
        }
        let lease = get_lease_in_tx(tx, lease_id).await?.ok_or_else(|| {
            VoomError::Internal("force_release: post-update get vanished".to_owned())
        })?;
        Ok(ForceReleaseOutcome {
            lease,
            ticket_requeued,
            attempt: u32_from_i64(attempt)?,
            max_attempts: u32_from_i64(max_attempts)?,
        })
    }

    async fn force_release(
        &self,
        lease_id: LeaseId,
        also_requeue: bool,
        now: OffsetDateTime,
    ) -> Result<ForceReleaseOutcome, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self
            .force_release_in_tx(&mut tx, lease_id, also_requeue, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn get(&self, id: LeaseId) -> Result<Option<Lease>, VoomError> {
        let row = sqlx::query(SELECT_LEASE_COLS)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("leases get: {e}")))?;
        row.as_ref().map(row_to_lease).transpose()
    }
}

const SELECT_LEASE_COLS: &str = "SELECT id, ticket_id, worker_id, state, acquired_at, expires_at, \
            last_heartbeat_at, ttl_seconds, release_reason, released_at, epoch \
     FROM leases WHERE id = ?";

async fn get_lease_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: LeaseId,
) -> Result<Option<Lease>, VoomError> {
    let row = sqlx::query(SELECT_LEASE_COLS)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("leases get_in_tx: {e}")))?;
    row.as_ref().map(row_to_lease).transpose()
}

fn row_to_lease(row: &sqlx::sqlite::SqliteRow) -> Result<Lease, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("leases", &e))?;
    let ticket_id: i64 = row
        .try_get("ticket_id")
        .map_err(|e| map_row_err("leases", &e))?;
    let worker_id: i64 = row
        .try_get("worker_id")
        .map_err(|e| map_row_err("leases", &e))?;
    let state: String = row
        .try_get("state")
        .map_err(|e| map_row_err("leases", &e))?;
    let acquired: String = row
        .try_get("acquired_at")
        .map_err(|e| map_row_err("leases", &e))?;
    let expires: String = row
        .try_get("expires_at")
        .map_err(|e| map_row_err("leases", &e))?;
    let last_hb: String = row
        .try_get("last_heartbeat_at")
        .map_err(|e| map_row_err("leases", &e))?;
    let ttl: i64 = row
        .try_get("ttl_seconds")
        .map_err(|e| map_row_err("leases", &e))?;
    let reason: Option<String> = row
        .try_get("release_reason")
        .map_err(|e| map_row_err("leases", &e))?;
    let released: Option<String> = row
        .try_get("released_at")
        .map_err(|e| map_row_err("leases", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("leases", &e))?;
    Ok(Lease {
        id: LeaseId(u64_from_i64(id)),
        ticket_id: TicketId(u64_from_i64(ticket_id)),
        worker_id: WorkerId(u64_from_i64(worker_id)),
        state: LeaseState::parse(&state)?,
        acquired_at: parse_iso8601(&acquired)?,
        expires_at: parse_iso8601(&expires)?,
        last_heartbeat_at: parse_iso8601(&last_hb)?,
        ttl_seconds: ttl,
        release_reason: reason.map(|s| ReleaseReason::parse(&s)).transpose()?,
        released_at: released.map(|s| parse_iso8601(&s)).transpose()?,
        epoch: u64_from_i64(epoch),
    })
}

#[cfg(test)]
#[path = "leases_test.rs"]
mod tests;
