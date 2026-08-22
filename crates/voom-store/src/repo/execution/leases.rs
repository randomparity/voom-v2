//! `SqliteLeaseRepo` — worker-execution lease lifecycle.

use rand::RngCore;
use serde_json::Value as JsonValue;
use sqlx::{Acquire, Row, SqlitePool};
use time::{Duration, OffsetDateTime};
use voom_core::{
    FailureClass, JobId, LeaseId, NodeId, NormalizedTicketOperation, TicketId, TicketOperation,
    VoomError, WorkerId,
};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};
use super::tickets::SqliteTicketRepo;
use super::workers::{SqliteWorkerRepo, WorkerOperationEligibility, WorkerStatus};

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

    pub(crate) fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "held" => Ok(Self::Held),
            "released" => Ok(Self::Released),
            "expired" => Ok(Self::Expired),
            "force_released" => Ok(Self::ForceReleased),
            other => Err(VoomError::database(format!(
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
            other => Err(VoomError::database(format!(
                "leases.release_reason {other:?} not in vocab"
            ))),
        }
    }
}

/// Filter for the keyset-paginated `SqliteLeaseRepo::list` inspection read.
#[derive(Debug, Clone, Default)]
pub struct LeaseFilter {
    pub state: Option<LeaseState>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseInterval {
    pub worker_id: WorkerId,
    pub acquired_at: OffsetDateTime,
    pub released_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseDispatchContext {
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
    pub expires_at: OffsetDateTime,
}

/// Durable operation-capacity observation that rejected a lease acquisition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCapacitySaturation {
    /// Worker whose operation slot was requested.
    pub worker_id: WorkerId,
    /// Normalized worker operation used by the store-owned capacity predicate.
    pub operation: TicketOperation,
    /// Held leases observed for the worker and operation.
    pub active_leases: u32,
    /// Effective grant limit for the worker and operation.
    pub max_parallel: u32,
}

impl WorkerCapacitySaturation {
    fn into_error(self) -> VoomError {
        VoomError::NoEligibleWorker(format!(
            "acquire rejected: worker {} capacity full for {} (active {}, limit {})",
            self.worker_id, self.operation, self.active_leases, self.max_parallel
        ))
    }
}

/// Why a worker could not take the operation it tried to acquire, classified
/// from the same facts [`WorkerOperationEligibility`]
/// reports. The set is closed: every ineligible shape maps to exactly one
/// reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseIneligibilityReason {
    /// The worker row does not exist.
    WorkerMissing,
    /// The worker is alive but stale.
    WorkerStale,
    /// The worker has been retired.
    WorkerRetired,
    /// A grant explicitly denies the operation.
    OperationDenied,
    /// No capability row advertises the operation.
    MissingCapability,
    /// No grant authorizes the operation.
    MissingGrant,
}

/// Store-owned result of an otherwise valid lease-acquisition attempt.
///
/// Every guarded recheck — readiness, worker eligibility, capacity — resolves
/// to an outcome rather than an error, so callers that handle one acquisition
/// can turn a changed gate into a documented decision while callers that
/// cannot (`into_lease_result`) still get today's error classification. All
/// non-`Acquired` outcomes roll back the acquire savepoint and mutate nothing.
#[derive(Debug, Clone)]
pub enum LeaseAcquireOutcome {
    /// The ticket transition and held lease committed inside the savepoint.
    Acquired(Lease),
    /// Capacity was full and the savepoint was rolled back without side effects.
    CapacityFull(WorkerCapacitySaturation),
    /// The ticket was not ready, not yet eligible, its parent job was not
    /// open, or its attempt budget was exhausted; nothing was mutated.
    TicketNotReady { ticket_id: TicketId },
    /// The worker could not be credited with the operation; nothing was
    /// mutated.
    WorkerIneligible {
        worker_id: WorkerId,
        operation: TicketOperation,
        reason: LeaseIneligibilityReason,
    },
}

impl LeaseAcquireOutcome {
    /// Convert to the legacy acquisition result and public error classification.
    ///
    /// The messages below are public-observable behavior of the standalone and
    /// local acquisition paths and are pinned by tests; change them only
    /// deliberately.
    ///
    /// # Errors
    ///
    /// Returns `NoEligibleWorker` when the typed outcome is capacity
    /// saturation, `NotFound`/`Conflict` when a guarded recheck rejected the
    /// acquisition.
    pub fn into_lease_result(self) -> Result<Lease, VoomError> {
        match self {
            Self::Acquired(lease) => Ok(lease),
            Self::CapacityFull(saturation) => Err(saturation.into_error()),
            Self::TicketNotReady { ticket_id } => Err(VoomError::Conflict(format!(
                "acquire rejected for ticket {ticket_id}: not ready, not eligible, \
                 parent job not open, or out of attempts"
            ))),
            Self::WorkerIneligible {
                worker_id,
                operation,
                reason,
            } => Err(ineligibility_error(worker_id, &operation, reason)),
        }
    }
}

fn ineligibility_error(
    worker_id: WorkerId,
    operation: &TicketOperation,
    reason: LeaseIneligibilityReason,
) -> VoomError {
    match reason {
        LeaseIneligibilityReason::WorkerMissing => {
            VoomError::NotFound(format!("worker {worker_id}"))
        }
        LeaseIneligibilityReason::WorkerStale => {
            VoomError::Conflict(format!("acquire rejected: worker {worker_id} stale"))
        }
        LeaseIneligibilityReason::WorkerRetired => {
            VoomError::Conflict(format!("acquire rejected: worker {worker_id} retired"))
        }
        LeaseIneligibilityReason::OperationDenied => VoomError::Conflict(format!(
            "acquire rejected: worker {worker_id} denied operation {operation}"
        )),
        LeaseIneligibilityReason::MissingCapability => VoomError::Conflict(format!(
            "acquire rejected: worker {worker_id} missing capability {operation}"
        )),
        LeaseIneligibilityReason::MissingGrant => VoomError::Conflict(format!(
            "acquire rejected: worker {worker_id} missing grant {operation}"
        )),
    }
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

/// Per-row outcome for a lease whose ticket exhausted its retry budget
/// during `expire_due_in_tx`. Carries the `attempt` / `max_attempts`
/// the repo already had in scope when it decided the ticket's fate,
/// so the case handler can build the `TicketFailedTerminal` payload
/// without a redundant `tickets.get_in_tx` round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedExpiry {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub attempt: u32,
    pub max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpireReport {
    /// All expired leases, in id-order.
    pub expired_leases: Vec<LeaseId>,
    /// Tickets whose lease expired and were requeued for retry.
    pub requeued_tickets: Vec<TicketId>,
    /// Per-row outcomes for leases whose ticket exhausted its retry
    /// budget. Carries the `attempt` / `max_attempts` snapshot the
    /// repo already had in scope at the decision point so the case
    /// handler can build `TicketFailedTerminal` payloads without a
    /// second `tickets.get_in_tx` round-trip.
    pub failed_expiries: Vec<FailedExpiry>,
    /// Per-row (`lease_id`, `ticket_id`) pairs in the order they were processed.
    /// Lets the `ControlPlane` emit `lease.expired` events whose payload
    /// carries the matching `ticket_id`, and
    /// `ticket.requeued_after_lease_expiry` / `ticket.failed_terminal`
    /// whose payload carries the matching `lease_id`. Each pair classifies
    /// as requeued or failed depending on which of `requeued_tickets` /
    /// `failed_expiries` the `ticket_id` appears in.
    pub pairs: Vec<(LeaseId, TicketId)>,
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

impl SqliteLeaseRepo {
    /// Acquire a lease or return typed durable-capacity backpressure.
    ///
    /// Capacity rejection rolls back the method's savepoint, including the
    /// provisional ticket transition. Other rejection reasons remain errors.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] for a non-positive TTL,
    /// [`VoomError::NotFound`] when the ticket or worker is missing,
    /// [`VoomError::Conflict`] when ticket state or worker eligibility rejects
    /// acquisition, [`VoomError::Database`] for storage failures, and
    /// [`VoomError::Internal`] when required state cannot be encoded or re-read.
    pub async fn try_acquire_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewLease,
    ) -> Result<LeaseAcquireOutcome, VoomError> {
        let ttl_secs = input.ttl.whole_seconds();
        if ttl_secs <= 0 {
            return Err(VoomError::Config(format!(
                "ttl must be positive, got {ttl_secs}s"
            )));
        }

        let mut savepoint = tx
            .begin()
            .await
            .map_err(|e| VoomError::database_context("lease acquire savepoint begin", e))?;
        let result = self.acquire_guarded(&mut savepoint, &input, ttl_secs).await;
        match result {
            Ok(outcome @ LeaseAcquireOutcome::Acquired(_)) => {
                savepoint.commit().await.map_err(|e| {
                    VoomError::database_context("lease acquire savepoint release", e)
                })?;
                Ok(outcome)
            }
            Ok(rejected) => {
                // Every rejection rolls the provisional ticket transition
                // back: a changed gate mutates nothing.
                savepoint.rollback().await.map_err(|rollback_error| {
                    VoomError::database(format!(
                        "lease acquire rollback after {rejected:?}: {rollback_error}"
                    ))
                })?;
                Ok(rejected)
            }
            Err(error) => {
                savepoint.rollback().await.map_err(|rollback_error| {
                    VoomError::database(format!(
                        "lease acquire rollback after {error}: {rollback_error}"
                    ))
                })?;
                Err(error)
            }
        }
    }

    /// Acquire a lease, mapping capacity saturation to `NoEligibleWorker`.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::try_acquire_in_tx`], and
    /// [`VoomError::NoEligibleWorker`] when the worker is at capacity.
    pub async fn acquire_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewLease,
    ) -> Result<Lease, VoomError> {
        self.try_acquire_in_tx(tx, input).await?.into_lease_result()
    }

    async fn acquire_guarded(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: &NewLease,
        ttl_secs: i64,
    ) -> Result<LeaseAcquireOutcome, VoomError> {
        let ticket = SqliteTicketRepo::new(self.pool.clone())
            .get_in_tx(tx, input.ticket_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("ticket {}", input.ticket_id)))?;
        // Fail closed. This handles exactly one ticket, so raising here denies
        // that ticket alone — unlike the capability lookups, which run inside a
        // candidate loop and must stay total.
        let normalized = ticket.kind.normalize();
        if matches!(normalized, NormalizedTicketOperation::UnknownNamespaced(_)) {
            return Err(VoomError::database(format!(
                "ticket kind {:?} names no known operation",
                ticket.kind.as_str()
            )));
        }
        let operation = normalized.matching_token();
        let now_str = iso8601(input.now)?;
        let res = sqlx::query(
            "UPDATE tickets \
             SET state = 'leased', state_changed_at = ?, attempt = attempt + 1, \
                 epoch = epoch + 1 \
             WHERE id = ? AND state = 'ready' AND next_eligible_at <= ? \
                   AND attempt < max_attempts \
                   AND (job_id IS NULL OR EXISTS ( \
                       SELECT 1 FROM jobs \
                       WHERE jobs.id = tickets.job_id AND jobs.state = 'open' \
                   ))",
        )
        .bind(&now_str)
        .bind(i64_from_u64(
            input.ticket_id.0,
            concat!(module_path!(), ": ", stringify!(input.ticket_id.0)),
        )?)
        .bind(&now_str)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("tickets transition to leased", e))?;
        if res.rows_affected() == 0 {
            return Ok(LeaseAcquireOutcome::TicketNotReady {
                ticket_id: input.ticket_id,
            });
        }
        let workers = SqliteWorkerRepo::new(self.pool.clone());
        let eligibility = workers
            .operation_eligibility_in_tx(tx, input.worker_id, &operation)
            .await?;
        if let Some(reason) = ineligibility_reason(&eligibility) {
            return Ok(LeaseAcquireOutcome::WorkerIneligible {
                worker_id: input.worker_id,
                operation,
                reason,
            });
        }
        let capacity = workers
            .operation_capacity_in_tx(tx, input.worker_id, &operation)
            .await?;
        if !capacity.has_capacity() {
            return Ok(LeaseAcquireOutcome::CapacityFull(
                WorkerCapacitySaturation {
                    worker_id: input.worker_id,
                    operation,
                    active_leases: capacity.active_leases,
                    max_parallel: capacity.max_parallel,
                },
            ));
        }

        let expires = input.now + input.ttl;
        let expires_str = iso8601(expires)?;
        let res2 = sqlx::query(
            "INSERT INTO leases \
             (ticket_id, worker_id, state, acquired_at, expires_at, \
              last_heartbeat_at, ttl_seconds) \
             VALUES (?, ?, 'held', ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(
            input.ticket_id.0,
            concat!(module_path!(), ": ", stringify!(input.ticket_id.0)),
        )?)
        .bind(i64_from_u64(
            input.worker_id.0,
            concat!(module_path!(), ": ", stringify!(input.worker_id.0)),
        )?)
        .bind(&now_str)
        .bind(&expires_str)
        .bind(&now_str)
        .bind(ttl_secs)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("leases insert", e))?;
        let lease = get_lease_in_tx(
            tx,
            LeaseId(u64_from_i64(
                res2.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res2.last_insert_rowid())),
            )?),
        )
        .await?
        .ok_or_else(|| VoomError::Internal("acquire: post-insert get vanished".to_owned()))?;
        Ok(LeaseAcquireOutcome::Acquired(lease))
    }

    /// Acquire and commit a lease.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::acquire_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn acquire(&self, input: NewLease) -> Result<Lease, VoomError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| VoomError::database_context("lease acquire begin immediate", e))?;
        let out = self.acquire_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Extend a held lease without shortening its existing deadline.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when the lease is missing or not held,
    /// [`VoomError::Database`] for query or row-decoding failures, and
    /// [`VoomError::Internal`] if timestamps cannot be encoded or the updated
    /// row cannot be re-read.
    pub async fn heartbeat_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        lease_id: LeaseId,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let now_str = iso8601(now)?;
        let expires_str = iso8601(now + ttl)?;
        // Clamp the deadline forward only. A heartbeat carrying a shorter TTL
        // than the lease's current deadline must never move expires_at
        // backwards — a shortened deadline could let expire_due reap a lease
        // whose worker just proved it is alive. `max(expires_at, ?)` keeps the
        // later of the two; ISO8601 timestamps sort lexicographically, the same
        // ordering expire_due relies on. last_heartbeat_at is still recorded
        // unconditionally so the liveness signal is never dropped.
        let res = sqlx::query(
            "UPDATE leases SET last_heartbeat_at = ?, \
                 expires_at = max(expires_at, ?), epoch = epoch + 1 \
             WHERE id = ? AND state = 'held' AND expires_at > ?",
        )
        .bind(&now_str)
        .bind(&expires_str)
        .bind(i64_from_u64(
            lease_id.0,
            concat!(module_path!(), ": ", stringify!(lease_id.0)),
        )?)
        .bind(&now_str)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("leases heartbeat", e))?;
        if res.rows_affected() == 0 {
            let lease = get_lease_in_tx(tx, lease_id).await?;
            let Some(lease) = lease else {
                return Err(VoomError::Conflict(format!(
                    "heartbeat rejected: lease {lease_id} not found"
                )));
            };
            if lease.state != LeaseState::Held {
                return Err(VoomError::Conflict(format!(
                    "heartbeat rejected: lease {lease_id} not held"
                )));
            }
            if lease.expires_at <= now {
                return Err(VoomError::Conflict(format!(
                    "heartbeat rejected: lease {lease_id} expired at {}",
                    lease.expires_at
                )));
            }
            return Err(VoomError::Conflict(format!(
                "heartbeat rejected: lease {lease_id} changed concurrently"
            )));
        }
        get_lease_in_tx(tx, lease_id)
            .await?
            .ok_or_else(|| VoomError::Internal("heartbeat: post-update get vanished".to_owned()))
    }

    /// Extend and commit a held lease heartbeat.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::heartbeat_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn heartbeat(
        &self,
        lease_id: LeaseId,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.heartbeat_in_tx(&mut tx, lease_id, ttl, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Release a held lease.
    ///
    /// The lease-side transition is one `UPDATE … RETURNING` round-trip
    /// (no pre-read, no post-read). When the RETURNING matches nothing
    /// the lease was already absent or in a non-`held` state — both
    /// outcomes surface as `VoomError::Conflict`. Callers that need to
    /// distinguish "missing" from "wrong state" should `get` first.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when the lease or ticket is not in its
    /// expected state, [`VoomError::Database`] for storage or row-decoding
    /// failures, and [`VoomError::Internal`] if the result or timestamp cannot
    /// be encoded.
    pub async fn release_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        lease_id: LeaseId,
        result: JsonValue,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let now_str = iso8601(now)?;
        let lease_row = sqlx::query(&format!(
            "UPDATE leases \
              SET state = 'released', release_reason = ?, released_at = ?, \
                  epoch = epoch + 1 \
              WHERE id = ? AND state = 'held' \
            RETURNING {LEASE_RETURNING_COLS}"
        ))
        .bind(ReleaseReason::Released.as_str())
        .bind(&now_str)
        .bind(i64_from_u64(
            lease_id.0,
            concat!(module_path!(), ": ", stringify!(lease_id.0)),
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("leases release", e))?;
        let Some(lease) = lease_row.as_ref().map(row_to_lease).transpose()? else {
            tracing::warn!(lease_id = lease_id.0, "release rejected: lease not held");
            return Err(VoomError::Conflict(format!(
                "release rejected: lease {lease_id} not held or not found"
            )));
        };
        let result_json = serialize_json(&result, "result")?;
        let ticket_res = sqlx::query(
            "UPDATE tickets SET state = 'succeeded', result = ?, \
             state_changed_at = ?, epoch = epoch + 1 WHERE id = ? AND state = 'leased'",
        )
        .bind(result_json)
        .bind(&now_str)
        .bind(i64_from_u64(
            lease.ticket_id.0,
            concat!(module_path!(), ": ", stringify!(lease.ticket_id.0)),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("tickets release", e))?;
        if ticket_res.rows_affected() != 1 {
            tracing::warn!(
                lease_id = lease_id.0,
                ticket_id = lease.ticket_id.0,
                "release aborting: ticket no longer leased"
            );
            return Err(VoomError::Conflict(format!(
                "release rejected: ticket {} not in expected state",
                lease.ticket_id
            )));
        }
        Ok(lease)
    }

    /// Release and commit a held lease as succeeded.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::release_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn release(
        &self,
        lease_id: LeaseId,
        result: JsonValue,
        now: OffsetDateTime,
    ) -> Result<Lease, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.release_in_tx(&mut tx, lease_id, result, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Fail a held lease.
    ///
    /// The lease-side transition is one `UPDATE … RETURNING` round-trip
    /// after a single JOIN pre-read that fetches ticket attempts gated on
    /// `state = 'held'` (replaces the previous wide `get_lease_in_tx`).
    /// On a missing lease, on a non-`held` lease, or on a lost race the
    /// caller sees `VoomError::Conflict`.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when lease or ticket state prevents the
    /// transition, [`VoomError::Database`] for storage or row-decoding failures,
    /// and [`VoomError::Internal`] if timestamps cannot be encoded.
    pub async fn fail_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        lease_id: LeaseId,
        class: FailureClass,
        now: OffsetDateTime,
        rng: &mut (dyn RngCore + Send),
    ) -> Result<Lease, VoomError> {
        // Single JOIN read: ticket attempts gated on the lease being held.
        // Replaces the wide `get_lease_in_tx` pre-read; also gives us
        // ticket_id, attempt, and max_attempts in one round-trip.
        let probe: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT t.id, t.attempt, t.max_attempts \
             FROM tickets t JOIN leases l ON l.ticket_id = t.id \
             WHERE l.id = ? AND l.state = 'held'",
        )
        .bind(i64_from_u64(
            lease_id.0,
            concat!(module_path!(), ": ", stringify!(lease_id.0)),
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("fail probe", e))?;
        let Some((ticket_id_i, attempt, max_attempts)) = probe else {
            tracing::warn!(lease_id = lease_id.0, "fail rejected: lease not held");
            return Err(VoomError::Conflict(format!(
                "fail rejected: lease {lease_id} not held or not found"
            )));
        };
        let ticket_id = TicketId(u64_from_i64(
            ticket_id_i,
            concat!(module_path!(), ": ", stringify!(ticket_id_i)),
        )?);
        let attempts_remain = attempt < max_attempts;
        let retriable = class.is_retriable();
        let now_str = iso8601(now)?;
        let release_reason = if retriable && attempts_remain {
            ReleaseReason::FailedRetriable
        } else {
            ReleaseReason::FailedTerminal
        };
        let lease = release_lease_for_failure_in_tx(tx, lease_id, release_reason, &now_str).await?;
        // Transition ticket: ready (with backoff) or failed.
        if retriable && attempts_remain {
            // attempt is already incremented to reflect "this dispatch"; backoff
            // factor is the current attempt number per §7.5.
            let attempt_u32 = u32_from_i64(attempt)?;
            let next_eligible = now + SqliteTicketRepo::default_backoff(attempt_u32, rng);
            let ticket_res = sqlx::query(
                "UPDATE tickets SET state = 'ready', state_changed_at = ?, \
                 next_eligible_at = ?, epoch = epoch + 1 \
                 WHERE id = ? AND state = 'leased'",
            )
            .bind(&now_str)
            .bind(iso8601(next_eligible)?)
            .bind(ticket_id_i)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("tickets requeue", e))?;
            if ticket_res.rows_affected() != 1 {
                tracing::warn!(
                    lease_id = lease_id.0,
                    ticket_id = ticket_id_i,
                    "fail aborting: ticket no longer leased on requeue"
                );
                return Err(VoomError::Conflict(format!(
                    "fail rejected (retriable): ticket {ticket_id} not in expected state"
                )));
            }
        } else {
            let ticket_res = sqlx::query(
                "UPDATE tickets SET state = 'failed', state_changed_at = ?, \
                 epoch = epoch + 1 WHERE id = ? AND state = 'leased'",
            )
            .bind(&now_str)
            .bind(ticket_id_i)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("tickets fail terminal", e))?;
            if ticket_res.rows_affected() != 1 {
                tracing::warn!(
                    lease_id = lease_id.0,
                    ticket_id = ticket_id_i,
                    "fail aborting: ticket no longer leased on terminal fail"
                );
                return Err(VoomError::Conflict(format!(
                    "fail rejected (terminal): ticket {ticket_id} not in expected state"
                )));
            }
        }
        Ok(lease)
    }

    /// Fail and commit a held lease, requeuing it when retries remain.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::fail_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn fail(
        &self,
        lease_id: LeaseId,
        class: FailureClass,
        now: OffsetDateTime,
        rng: &mut (dyn RngCore + Send),
    ) -> Result<Lease, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.fail_in_tx(&mut tx, lease_id, class, now, rng).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Expire one bounded batch of overdue leases in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when a lease or ticket changes during
    /// expiry, [`VoomError::Database`] for storage or row-decoding failures, and
    /// [`VoomError::Internal`] if the timestamp cannot be encoded.
    pub async fn expire_due_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        now: OffsetDateTime,
    ) -> Result<ExpireReport, VoomError> {
        let now_str = iso8601(now)?;
        // Find candidates first so we can return their IDs in the report.
        // `LIMIT` caps lock-hold time per call; the Sprint 6+ daemon
        // drains by re-invoking `expire_due` until the report is empty.
        let rows = sqlx::query(
            "SELECT id, ticket_id FROM leases \
             WHERE state = 'held' AND expires_at < ? \
             ORDER BY id ASC \
             LIMIT ?",
        )
        .bind(&now_str)
        .bind(LEASE_BATCH_LIMIT)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("expire_due scan", e))?;
        let mut report = ExpireReport {
            expired_leases: Vec::new(),
            requeued_tickets: Vec::new(),
            failed_expiries: Vec::new(),
            pairs: Vec::new(),
        };
        // Pre-fetch every candidate ticket's (attempt, max_attempts) in
        // one query so the per-row loop below stays O(N) instead of
        // O(2N) round-trips. At the documented bulk scale (500 leases
        // in tests/lease_expire_and_recover.rs) this saves 500 SELECTs
        // inside a single transaction.
        let ticket_attempts =
            fetch_ticket_attempts(tx, rows.iter().map(extract_ticket_id_i)).await?;
        for row in &rows {
            process_expired_lease(tx, row, &ticket_attempts, &now_str, &mut report).await?;
        }
        Ok(report)
    }

    /// Expire and commit one bounded batch of overdue leases.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::expire_due_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn expire_due(&self, now: OffsetDateTime) -> Result<ExpireReport, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.expire_due_in_tx(&mut tx, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Force-release a held lease and either requeue or fail its ticket.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when the lease or ticket is not in its
    /// expected state, or when requeue was requested after retry exhaustion;
    /// returns [`VoomError::Database`] for storage or row-decoding failures and
    /// [`VoomError::Internal`] if the timestamp cannot be encoded.
    pub async fn force_release_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        lease_id: LeaseId,
        also_requeue: bool,
        now: OffsetDateTime,
    ) -> Result<ForceReleaseOutcome, VoomError> {
        // Single JOIN read: ticket attempts gated on the lease being held.
        // Replaces the wide `get_lease_in_tx` pre-read; also gives us
        // ticket_id, attempt, and max_attempts in one round-trip.
        let probe: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT t.id, t.attempt, t.max_attempts \
             FROM tickets t JOIN leases l ON l.ticket_id = t.id \
             WHERE l.id = ? AND l.state = 'held'",
        )
        .bind(i64_from_u64(
            lease_id.0,
            concat!(module_path!(), ": ", stringify!(lease_id.0)),
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("force_release probe", e))?;
        let Some((ticket_id_i, attempt, max_attempts)) = probe else {
            tracing::warn!(
                lease_id = lease_id.0,
                "force_release rejected: lease not held"
            );
            return Err(VoomError::Conflict(format!(
                "force_release rejected: lease {lease_id} not held or not found"
            )));
        };
        let ticket_id = TicketId(u64_from_i64(
            ticket_id_i,
            concat!(module_path!(), ": ", stringify!(ticket_id_i)),
        )?);
        // Operator asked for requeue but the ticket is already out of
        // attempts: refuse the call entirely. Promoting back to `ready`
        // would strand the ticket — `acquire` refuses it (out of
        // attempts) and no held lease remains to expire — and
        // demote-to-terminal would mask the operator's request. The
        // caller must explicitly pass `also_requeue = false` if they
        // intend a terminal force-release.
        if also_requeue && attempt >= max_attempts {
            return Err(VoomError::Conflict(format!(
                "force_release requeue rejected: ticket {ticket_id} attempt {attempt} >= \
                 max_attempts {max_attempts}; use also_requeue = false"
            )));
        }
        let ticket_requeued = also_requeue;
        let now_str = iso8601(now)?;
        let lease = force_release_lease_in_tx(tx, lease_id, &now_str).await?;
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
            .bind(ticket_id_i)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("tickets force_release", e))?
        } else {
            sqlx::query(
                "UPDATE tickets SET state = 'failed', state_changed_at = ?, \
                 epoch = epoch + 1 WHERE id = ? AND state = 'leased'",
            )
            .bind(&now_str)
            .bind(ticket_id_i)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("tickets force_release", e))?
        };
        if ticket_res.rows_affected() != 1 {
            tracing::warn!(
                lease_id = lease_id.0,
                ticket_id = ticket_id_i,
                "force_release aborting: ticket no longer leased"
            );
            return Err(VoomError::Conflict(format!(
                "force_release rejected: ticket {ticket_id} not in expected state"
            )));
        }
        Ok(ForceReleaseOutcome {
            lease,
            ticket_requeued,
            attempt: u32_from_i64(attempt)?,
            max_attempts: u32_from_i64(max_attempts)?,
        })
    }

    /// Force-release and commit a held lease.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::force_release_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn force_release(
        &self,
        lease_id: LeaseId,
        also_requeue: bool,
        now: OffsetDateTime,
    ) -> Result<ForceReleaseOutcome, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self
            .force_release_in_tx(&mut tx, lease_id, also_requeue, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Return a held lease only when it belongs to the supplied worker.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the lease does not exist,
    /// [`VoomError::Conflict`] when another worker holds it or it is not held,
    /// and [`VoomError::Database`] if the row cannot be queried or decoded.
    pub async fn get_held_for_worker_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        lease_id: LeaseId,
        worker_id: WorkerId,
    ) -> Result<Lease, VoomError> {
        let lease = get_lease_in_tx(tx, lease_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("lease {lease_id}")))?;
        if lease.worker_id != worker_id {
            return Err(VoomError::Conflict(format!(
                "lease {lease_id} is held by worker {}, not worker {worker_id}",
                lease.worker_id
            )));
        }
        if lease.state != LeaseState::Held {
            return Err(VoomError::Conflict(format!(
                "lease {lease_id} is {}, not held",
                lease.state.as_str()
            )));
        }
        Ok(lease)
    }

    /// Return and commit a held-lease ownership check.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::get_held_for_worker_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn get_held_for_worker(
        &self,
        lease_id: LeaseId,
        worker_id: WorkerId,
    ) -> Result<Lease, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self
            .get_held_for_worker_in_tx(&mut tx, lease_id, worker_id)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Return whether a ticket currently has a held lease.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the lease state cannot be queried.
    pub async fn has_held_for_ticket_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        ticket_id: TicketId,
    ) -> Result<bool, VoomError> {
        let held: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM leases WHERE ticket_id = ? AND state = 'held' LIMIT 1",
        )
        .bind(i64_from_u64(
            ticket_id.0,
            concat!(module_path!(), ": ", stringify!(ticket_id.0)),
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("held lease for ticket", e))?;
        Ok(held.is_some())
    }

    /// Count held leases whose workers belong to one node.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the count cannot be queried or decoded.
    pub async fn active_count_for_node_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        node_id: NodeId,
    ) -> Result<u32, VoomError> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM leases \
             JOIN workers ON workers.id = leases.worker_id \
             WHERE leases.state = 'held' AND workers.node_id = ?",
        )
        .bind(i64_from_u64(
            node_id.0,
            concat!(module_path!(), ": ", stringify!(node_id.0)),
        )?)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("node active lease count", e))?;
        u32_from_i64(count)
    }

    /// Look up a lease by id.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn get(&self, id: LeaseId) -> Result<Option<Lease>, VoomError> {
        let row = sqlx::query(SELECT_LEASE_COLS)
            .bind(i64_from_u64(
                id.0,
                concat!(module_path!(), ": ", stringify!(id.0)),
            )?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("leases get", e))?;
        row.as_ref().map(row_to_lease).transpose()
    }

    /// Look up a lease by id inside the caller's transaction, in any state.
    ///
    /// Replay validation must prove identity against the same snapshot the
    /// reservation transaction already holds; lease state is deliberately
    /// not filtered (ADR 0073).
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn get_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: LeaseId,
    ) -> Result<Option<Lease>, VoomError> {
        let row = sqlx::query(SELECT_LEASE_COLS)
            .bind(i64_from_u64(
                id.0,
                concat!(module_path!(), ": ", stringify!(id.0)),
            )?)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("leases get_in_tx", e))?;
        row.as_ref().map(row_to_lease).transpose()
    }

    /// Return the worker context for a currently held lease.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the row or timestamp cannot be decoded.
    pub async fn dispatch_context(
        &self,
        lease_id: LeaseId,
    ) -> Result<Option<LeaseDispatchContext>, VoomError> {
        let row: Option<(i64, i64, String)> = sqlx::query_as(
            "SELECT workers.id, workers.epoch, leases.expires_at \
             FROM leases JOIN workers ON workers.id = leases.worker_id \
             WHERE leases.id = ? AND leases.state = 'held'",
        )
        .bind(i64::try_from(lease_id.0).map_err(|error| {
            VoomError::Config(format!("lease id exceeds SQLite integer: {error}"))
        })?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("lease dispatch context", error))?;
        row.map(|(worker_id, worker_epoch, expires_at)| {
            Ok(LeaseDispatchContext {
                worker_id: WorkerId(u64::try_from(worker_id).map_err(|error| {
                    VoomError::database_context("lease dispatch worker id negative", error)
                })?),
                worker_epoch: u64::try_from(worker_epoch).map_err(|error| {
                    VoomError::database_context("lease dispatch worker epoch negative", error)
                })?,
                expires_at: parse_iso8601(&expires_at)?,
            })
        })
        .transpose()
    }

    /// Return every lease interval for tickets in one job in deterministic order.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or typed timestamp decoding fails.
    pub async fn timeline_for_job(&self, job_id: JobId) -> Result<Vec<LeaseInterval>, VoomError> {
        let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(
            "SELECT leases.worker_id, leases.acquired_at, leases.released_at \
             FROM leases \
             JOIN tickets ON tickets.id = leases.ticket_id \
             WHERE tickets.job_id = ? \
             ORDER BY leases.acquired_at ASC, leases.worker_id ASC, leases.id ASC",
        )
        .bind(i64_from_u64(
            job_id.0,
            concat!(module_path!(), ": ", stringify!(job_id.0)),
        )?)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("lease timeline for job", e))?;
        rows.into_iter()
            .map(|(worker_id, acquired_at, released_at)| {
                Ok(LeaseInterval {
                    worker_id: WorkerId(u64_from_i64(
                        worker_id,
                        concat!(module_path!(), ": ", stringify!(worker_id)),
                    )?),
                    acquired_at: parse_iso8601(&acquired_at)?,
                    released_at: released_at.as_deref().map(parse_iso8601).transpose()?,
                })
            })
            .collect()
    }

    /// Keyset-paginated inspection read for `voom scheduler leases list`
    /// (ADR 0031). Orders strictly by `id` descending (newest first);
    /// `after_id` is an exclusive continuation token returning rows with
    /// `id < after_id`.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn list(
        &self,
        filter: LeaseFilter,
        after_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<Lease>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {LEASE_RETURNING_COLS} FROM leases \
             WHERE (?1 IS NULL OR state = ?1) \
               AND (?2 IS NULL OR id < ?2) \
             ORDER BY id DESC LIMIT ?3"
        ))
        .bind(filter.state.map(LeaseState::as_str))
        .bind(
            after_id
                .map(|value| {
                    i64_from_u64(value, concat!(module_path!(), ": ", stringify!(after_id)))
                })
                .transpose()?,
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("leases list", e))?;
        rows.iter().map(row_to_lease).collect()
    }
}

async fn release_lease_for_failure_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    lease_id: LeaseId,
    release_reason: ReleaseReason,
    now_str: &str,
) -> Result<Lease, VoomError> {
    let lease_row = sqlx::query(&format!(
        "UPDATE leases \
          SET state = 'released', release_reason = ?, released_at = ?, \
              epoch = epoch + 1 \
          WHERE id = ? AND state = 'held' \
        RETURNING {LEASE_RETURNING_COLS}"
    ))
    .bind(release_reason.as_str())
    .bind(now_str)
    .bind(i64_from_u64(
        lease_id.0,
        concat!(module_path!(), ": ", stringify!(lease_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("leases release on fail", e))?;
    let Some(lease) = lease_row.as_ref().map(row_to_lease).transpose()? else {
        tracing::warn!(lease_id = lease_id.0, "fail aborting: lease no longer held");
        return Err(VoomError::Conflict(format!(
            "fail rejected: lease {lease_id} no longer held"
        )));
    };
    Ok(lease)
}

async fn force_release_lease_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    lease_id: LeaseId,
    now_str: &str,
) -> Result<Lease, VoomError> {
    let lease_row = sqlx::query(&format!(
        "UPDATE leases \
          SET state = 'force_released', release_reason = ?, \
              released_at = ?, epoch = epoch + 1 \
          WHERE id = ? AND state = 'held' \
        RETURNING {LEASE_RETURNING_COLS}"
    ))
    .bind(ReleaseReason::ForceReleased.as_str())
    .bind(now_str)
    .bind(i64_from_u64(
        lease_id.0,
        concat!(module_path!(), ": ", stringify!(lease_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("lease force_release", e))?;
    let Some(lease) = lease_row.as_ref().map(row_to_lease).transpose()? else {
        tracing::warn!(
            lease_id = lease_id.0,
            "force_release aborting: lease no longer held"
        );
        return Err(VoomError::Conflict(format!(
            "force_release rejected: lease {lease_id} no longer held"
        )));
    };
    Ok(lease)
}

const SELECT_LEASE_COLS: &str = "SELECT id, ticket_id, worker_id, state, acquired_at, expires_at, \
            last_heartbeat_at, ttl_seconds, release_reason, released_at, epoch \
     FROM leases WHERE id = ?";

/// Column list for `UPDATE leases ... RETURNING <cols>` in the lease
/// lifecycle methods. Mirrors `SELECT_LEASE_COLS` so `row_to_lease`
/// can decode the row uniformly.
const LEASE_RETURNING_COLS: &str = "id, ticket_id, worker_id, state, acquired_at, expires_at, \
     last_heartbeat_at, ttl_seconds, release_reason, released_at, epoch";

fn extract_ticket_id_i(row: &sqlx::sqlite::SqliteRow) -> Result<i64, VoomError> {
    row.try_get("ticket_id")
        .map_err(|e| map_row_err("leases", e))
}

/// Chunk size for the `IN (?, …, ?)` clause built by
/// `fetch_ticket_attempts`. Sits well below `SQLite`'s historical
/// 999-variable floor and the bundled 32,766 default, so the prefetch
/// never exceeds `SQLITE_MAX_VARIABLE_NUMBER` regardless of which
/// `SQLite` the binary is linked against. Internal — not a tuning knob.
const TICKET_ATTEMPT_CHUNK: usize = 500;

/// Maximum rows touched by a single `expire_due_in_tx` call. Bounds
/// transaction size, memory allocation, and lock-hold time on restart
/// backlogs. The Sprint 6+ daemon loops until the report is empty;
/// under steady state each tick stays well under the cap. M1 ticket
/// events emit two rows per pair (`LeaseExpired` plus
/// `TicketRequeuedAfterLeaseExpiry` or `TicketFailedTerminal`), so
/// per-batch lock-hold is roughly twice the M3 `USE_LEASE_BATCH_LIMIT`
/// case — still conservative; if production data warrants tuning, the
/// Sprint 6+ daemon spec can promote it to a policy-driven knob.
pub const LEASE_BATCH_LIMIT: i64 = 1000;

/// Pre-fetch every distinct ticket's (`attempt`, `max_attempts`) in
/// chunked SELECTs. Used by `expire_due_in_tx` to replace what was a
/// per-row `SELECT ... FROM tickets WHERE id = ?` (N+1 over the
/// scanned lease batch) with one bulk query per `TICKET_ATTEMPT_CHUNK`
/// rows. Chunking keeps the bind count safely below the `SQLite`
/// variable limit even on restart backlogs that exceed the historical
/// 999-variable floor.
async fn fetch_ticket_attempts<I>(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ticket_ids: I,
) -> Result<std::collections::HashMap<i64, (i64, i64)>, VoomError>
where
    I: IntoIterator<Item = Result<i64, VoomError>>,
{
    let ids: Vec<i64> = ticket_ids.into_iter().collect::<Result<_, _>>()?;
    let mut out = std::collections::HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(TICKET_ATTEMPT_CHUNK) {
        if chunk.is_empty() {
            continue;
        }
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql =
            format!("SELECT id, attempt, max_attempts FROM tickets WHERE id IN ({placeholders})");
        let mut q = sqlx::query(&sql);
        for id in chunk {
            q = q.bind(id);
        }
        let rows = q
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("ticket attempts batch", e))?;
        for row in &rows {
            let id: i64 = row.try_get("id").map_err(|e| map_row_err("tickets", e))?;
            let attempt: i64 = row
                .try_get("attempt")
                .map_err(|e| map_row_err("tickets", e))?;
            let max_attempts: i64 = row
                .try_get("max_attempts")
                .map_err(|e| map_row_err("tickets", e))?;
            out.insert(id, (attempt, max_attempts));
        }
    }
    Ok(out)
}

/// Process one expired-lease row from `expire_due_in_tx`: mark the
/// lease `expired`, transition the matching ticket to `ready` or
/// `failed`, and push the per-row outcome onto `report`. The
/// `ticket_attempts` map must already contain an entry for the
/// row's `ticket_id` (populated by `fetch_ticket_attempts`).
async fn process_expired_lease(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    row: &sqlx::sqlite::SqliteRow,
    ticket_attempts: &std::collections::HashMap<i64, (i64, i64)>,
    now_str: &str,
    report: &mut ExpireReport,
) -> Result<(), VoomError> {
    let (lease_id_i, ticket_id_i, lease_id, ticket_id) = decode_expired_lease_row(row)?;
    let lease_res = sqlx::query(
        "UPDATE leases SET state = 'expired', release_reason = ?, \
         released_at = ?, epoch = epoch + 1 \
         WHERE id = ? AND state = 'held'",
    )
    .bind(ReleaseReason::IssuerLost.as_str())
    .bind(now_str)
    .bind(lease_id_i)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("lease expire", e))?;
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
    let &(attempt, max_attempts) = ticket_attempts.get(&ticket_id_i).ok_or_else(|| {
        VoomError::Internal(format!(
            "expire_due: ticket {ticket_id} missing from pre-fetch"
        ))
    })?;
    if attempt < max_attempts {
        // Reset next_eligible_at to now so the requeued ticket is immediately
        // eligible, matching force_release and fail_retriable. Omitting it
        // leaves a stale backoff timestamp from a prior fail_retriable.
        let ticket_res = sqlx::query(
            "UPDATE tickets SET state = 'ready', state_changed_at = ?, \
             next_eligible_at = ?, epoch = epoch + 1 WHERE id = ? AND state = 'leased'",
        )
        .bind(now_str)
        .bind(now_str)
        .bind(ticket_id_i)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("ticket requeue", e))?;
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
        .bind(now_str)
        .bind(ticket_id_i)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("ticket fail", e))?;
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
        report.failed_expiries.push(FailedExpiry {
            lease_id,
            ticket_id,
            attempt: u32_from_i64(attempt)?,
            max_attempts: u32_from_i64(max_attempts)?,
        });
    }
    report.expired_leases.push(lease_id);
    report.pairs.push((lease_id, ticket_id));
    Ok(())
}

fn decode_expired_lease_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<(i64, i64, LeaseId, TicketId), VoomError> {
    let lease_id_raw = row.try_get("id").map_err(|e| map_row_err("leases", e))?;
    let ticket_id_raw = row
        .try_get("ticket_id")
        .map_err(|e| map_row_err("leases", e))?;
    let lease_id = LeaseId(u64_from_i64(lease_id_raw, "leases.id")?);
    let ticket_id = TicketId(u64_from_i64(ticket_id_raw, "leases.ticket_id")?);
    Ok((lease_id_raw, ticket_id_raw, lease_id, ticket_id))
}

/// Classify an operation-eligibility observation into the closed
/// [`LeaseIneligibilityReason`] set, or `None` when the worker is eligible.
///
/// The order mirrors how the facts rule each other out: lifecycle first, then
/// denial, then capability, then grant.
fn ineligibility_reason(
    eligibility: &WorkerOperationEligibility,
) -> Option<LeaseIneligibilityReason> {
    if eligibility.is_eligible() {
        return None;
    }
    match eligibility.worker_status {
        None => return Some(LeaseIneligibilityReason::WorkerMissing),
        Some(WorkerStatus::Stale) => return Some(LeaseIneligibilityReason::WorkerStale),
        Some(WorkerStatus::Retired) => return Some(LeaseIneligibilityReason::WorkerRetired),
        Some(WorkerStatus::Registered | WorkerStatus::Active) => {}
    }
    if eligibility.is_denied {
        return Some(LeaseIneligibilityReason::OperationDenied);
    }
    if !eligibility.has_capability {
        return Some(LeaseIneligibilityReason::MissingCapability);
    }
    if !eligibility.has_grant {
        return Some(LeaseIneligibilityReason::MissingGrant);
    }
    None
}

async fn get_lease_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: LeaseId,
) -> Result<Option<Lease>, VoomError> {
    let row = sqlx::query(SELECT_LEASE_COLS)
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("leases get_in_tx", e))?;
    row.as_ref().map(row_to_lease).transpose()
}

fn row_to_lease(row: &sqlx::sqlite::SqliteRow) -> Result<Lease, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("leases", e))?;
    let ticket_id: i64 = row
        .try_get("ticket_id")
        .map_err(|e| map_row_err("leases", e))?;
    let worker_id: i64 = row
        .try_get("worker_id")
        .map_err(|e| map_row_err("leases", e))?;
    let state: String = row.try_get("state").map_err(|e| map_row_err("leases", e))?;
    let acquired: String = row
        .try_get("acquired_at")
        .map_err(|e| map_row_err("leases", e))?;
    let expires: String = row
        .try_get("expires_at")
        .map_err(|e| map_row_err("leases", e))?;
    let last_hb: String = row
        .try_get("last_heartbeat_at")
        .map_err(|e| map_row_err("leases", e))?;
    let ttl: i64 = row
        .try_get("ttl_seconds")
        .map_err(|e| map_row_err("leases", e))?;
    let reason: Option<String> = row
        .try_get("release_reason")
        .map_err(|e| map_row_err("leases", e))?;
    let released: Option<String> = row
        .try_get("released_at")
        .map_err(|e| map_row_err("leases", e))?;
    let epoch: i64 = row.try_get("epoch").map_err(|e| map_row_err("leases", e))?;
    Ok(Lease {
        id: LeaseId(u64_from_i64(
            id,
            concat!(module_path!(), ": ", stringify!(id)),
        )?),
        ticket_id: TicketId(u64_from_i64(
            ticket_id,
            concat!(module_path!(), ": ", stringify!(ticket_id)),
        )?),
        worker_id: WorkerId(u64_from_i64(
            worker_id,
            concat!(module_path!(), ": ", stringify!(worker_id)),
        )?),
        state: LeaseState::parse(&state)?,
        acquired_at: parse_iso8601(&acquired)?,
        expires_at: parse_iso8601(&expires)?,
        last_heartbeat_at: parse_iso8601(&last_hb)?,
        ttl_seconds: ttl,
        release_reason: reason.map(|s| ReleaseReason::parse(&s)).transpose()?,
        released_at: released.map(|s| parse_iso8601(&s)).transpose()?,
        epoch: u64_from_i64(epoch, concat!(module_path!(), ": ", stringify!(epoch)))?,
    })
}

#[cfg(test)]
#[path = "leases_test.rs"]
mod tests;
