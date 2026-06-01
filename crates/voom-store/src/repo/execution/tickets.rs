//! `TicketRepo` — owns tickets + `ticket_dependencies`.

use async_trait::async_trait;
use rand::RngCore;
use serde_json::Value as JsonValue;
use sqlx::{QueryBuilder, Row, SqlitePool};
use time::{Duration, OffsetDateTime};
use voom_core::{Clock, JobId, TicketId, TicketOperation, VoomError};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};

/// Sprint 1 default backoff window — capped exponential with full
/// jitter, matching the architectural spec's Error Handling And
/// Recovery → Retry policy. Sprint 4+'s scheduling policy will replace
/// these constants with policy-driven values; the seam stays in
/// `TicketRepo::default_backoff` so the call sites don't change.
const DEFAULT_BACKOFF_BASE_SECS: u64 = 5;
const DEFAULT_BACKOFF_CAP_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketState {
    Pending,
    Ready,
    Leased,
    Succeeded,
    Failed,
}

impl TicketState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Leased => "leased",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "leased" => Ok(Self::Leased),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            other => Err(VoomError::Database(format!(
                "tickets.state {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewTicket {
    pub job_id: Option<JobId>,
    pub kind: TicketOperation,
    pub priority: i64,
    pub payload: JsonValue,
    pub max_attempts: u32,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct Ticket {
    pub id: TicketId,
    pub job_id: Option<JobId>,
    pub kind: TicketOperation,
    pub state: TicketState,
    pub priority: i64,
    pub payload: JsonValue,
    pub result: Option<JsonValue>,
    pub attempt: u32,
    pub max_attempts: u32,
    pub next_eligible_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub state_changed_at: OffsetDateTime,
    pub epoch: u64,
}

#[async_trait]
pub trait TicketRepo: Repository {
    async fn create_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewTicket,
    ) -> Result<Ticket, VoomError>;
    async fn create(&self, input: NewTicket) -> Result<Ticket, VoomError>;

    async fn add_dependency_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        ticket_id: TicketId,
        depends_on: TicketId,
    ) -> Result<(), VoomError>;
    async fn add_dependency(
        &self,
        ticket_id: TicketId,
        depends_on: TicketId,
    ) -> Result<(), VoomError>;

    /// Returns the vector of newly-promoted tickets — empty if the target
    /// was not eligible for promotion. For the M1 surface this returns at
    /// most one element (the target); cascade to dependents is the
    /// `ControlPlane`'s responsibility (Task 14 walks dependents on success).
    /// Repo writes only the ticket row — callers emit the `ticket.ready` event.
    async fn mark_ready_if_unblocked_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        ticket_id: TicketId,
        now: OffsetDateTime,
    ) -> Result<Vec<Ticket>, VoomError>;
    async fn mark_ready_if_unblocked(
        &self,
        ticket_id: TicketId,
        now: OffsetDateTime,
    ) -> Result<Vec<Ticket>, VoomError>;

    async fn get(&self, id: TicketId) -> Result<Option<Ticket>, VoomError>;
    async fn get_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: TicketId,
    ) -> Result<Option<Ticket>, VoomError>;
    async fn list_by_state(&self, state: TicketState, limit: u32)
    -> Result<Vec<Ticket>, VoomError>;
    async fn next_ready_for_operations_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        operations: &[TicketOperation],
        now: OffsetDateTime,
    ) -> Result<Option<Ticket>, VoomError>;
    async fn ready_for_operations_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        operations: &[TicketOperation],
        now: OffsetDateTime,
    ) -> Result<Vec<Ticket>, VoomError>;
    async fn next_ready_for_operations(
        &self,
        operations: &[TicketOperation],
        now: OffsetDateTime,
    ) -> Result<Option<Ticket>, VoomError>;
    async fn list_dependents(&self, depends_on: TicketId) -> Result<Vec<Ticket>, VoomError>;
    /// Same as `list_dependents` but reads through the supplied transaction.
    /// Required for the release-lease cascade so newly-succeeded parent state
    /// is visible to the lookup (sqlx-on-SQLite isolates pool reads from an
    /// open transaction).
    async fn list_dependents_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        depends_on: TicketId,
    ) -> Result<Vec<Ticket>, VoomError>;

    /// Default backoff window after a retriable failure: capped
    /// exponential with full jitter, per the architectural spec's
    /// Error Handling And Recovery → Retry policy.
    ///
    /// The current value is `random_between(0, min(cap, base * 2^attempt))`
    /// with `base = 5s` and `cap = 300s`. Sprint 4+ replaces the
    /// constants with scheduling-policy values; the signature stays
    /// stable so call sites don't move.
    ///
    /// `clock` is currently unused — it stays in the signature so
    /// Sprint 4 can introduce time-of-day-aware backoff windows
    /// without forcing every caller to change.
    #[expect(
        unused_variables,
        reason = "clock reserved for Sprint 4 time-of-day-aware backoff windows"
    )]
    fn default_backoff(
        attempt: u32,
        clock: &dyn Clock,
        rng: &mut (dyn RngCore + Send),
    ) -> Duration {
        let exp_secs =
            DEFAULT_BACKOFF_BASE_SECS.saturating_mul(1u64.checked_shl(attempt).unwrap_or(u64::MAX));
        let cap_secs = exp_secs.min(DEFAULT_BACKOFF_CAP_SECS);
        // Full jitter: uniform pick in [0, cap_secs]. Scale the u32 RNG
        // value across the (cap_secs + 1) buckets via 96-bit multiply
        // so `FrozenRng::new(0)` lands at 0 (floor) and
        // `FrozenRng::new(u32::MAX)` lands at `cap_secs` (ceiling).
        // The post-shift value fits in 64 bits whenever cap_secs does
        // (`(u32::MAX as u128 * (cap_secs as u128 + 1)) >> 32 < 2 * cap_secs`),
        // so `try_from` only fails for absurdly large caps — fall back
        // to the cap itself in that case rather than panicking.
        let buckets = u128::from(cap_secs).saturating_add(1);
        let raw = u128::from(rng.next_u32()).saturating_mul(buckets);
        let jitter_secs = u64::try_from(raw >> 32).unwrap_or(cap_secs);
        Duration::seconds(i64::try_from(jitter_secs).unwrap_or(i64::MAX))
    }
}

#[derive(Debug, Clone)]
pub struct SqliteTicketRepo {
    pool: SqlitePool,
}

impl SqliteTicketRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteTicketRepo {}

#[async_trait]
impl TicketRepo for SqliteTicketRepo {
    async fn create_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewTicket,
    ) -> Result<Ticket, VoomError> {
        let ts = iso8601(input.created_at)?;
        let payload_json = serialize_json(&input.payload, "payload")?;
        let res = sqlx::query(
            "INSERT INTO tickets \
             (job_id, kind, state, priority, payload, max_attempts, \
              next_eligible_at, created_at, state_changed_at) \
             VALUES (?, ?, 'pending', ?, ?, ?, ?, ?, ?)",
        )
        .bind(input.job_id.map(|j| i64_from_u64(j.0)))
        .bind(input.kind.as_str())
        .bind(input.priority)
        .bind(payload_json)
        .bind(i64::from(input.max_attempts))
        .bind(&ts)
        .bind(&ts)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("tickets insert: {e}")))?;
        let id = TicketId(u64_from_i64(res.last_insert_rowid()));
        // Re-read to return the canonical row.
        get_in_tx_inner(tx, id)
            .await?
            .ok_or_else(|| VoomError::Internal(format!("tickets create: row vanished id={id}")))
    }

    async fn create(&self, input: NewTicket) -> Result<Ticket, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.create_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn add_dependency_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        ticket_id: TicketId,
        depends_on: TicketId,
    ) -> Result<(), VoomError> {
        if ticket_id == depends_on {
            return Err(VoomError::DependencyCycle(format!(
                "ticket {ticket_id} cannot depend on itself"
            )));
        }
        // Dependencies may only be added while the dependent is still
        // pending. Once a ticket has crossed the readiness gate
        // (ready/leased/succeeded/failed), adding a new edge would not
        // demote it back to pending — and `acquire_in_tx` only checks
        // `state = 'ready'`, so a late edge would let the ticket lease and
        // run before the new blocker succeeds.
        let row: Option<(String,)> = sqlx::query_as("SELECT state FROM tickets WHERE id = ?")
            .bind(i64_from_u64(ticket_id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("ticket state probe: {e}")))?;
        let Some((state,)) = row else {
            return Err(VoomError::NotFound(format!("ticket {ticket_id}")));
        };
        if state != TicketState::Pending.as_str() {
            return Err(VoomError::Conflict(format!(
                "add_dependency rejected: ticket {ticket_id} is {state}, not pending"
            )));
        }
        // Cycle detection: walk dependencies of `depends_on` transitively.
        // If `ticket_id` appears, adding `ticket_id -> depends_on` would
        // close a cycle.
        let cyclic: Option<(i64,)> = sqlx::query_as(
            "WITH RECURSIVE reach(id) AS ( \
                 SELECT depends_on_ticket_id FROM ticket_dependencies WHERE ticket_id = ? \
                 UNION \
                 SELECT td.depends_on_ticket_id \
                   FROM ticket_dependencies td JOIN reach r ON td.ticket_id = r.id \
             ) \
             SELECT id FROM reach WHERE id = ? LIMIT 1",
        )
        .bind(i64_from_u64(depends_on.0))
        .bind(i64_from_u64(ticket_id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("cycle check: {e}")))?;
        if cyclic.is_some() {
            return Err(VoomError::DependencyCycle(format!(
                "adding {ticket_id} -> {depends_on} would create a cycle"
            )));
        }
        sqlx::query(
            "INSERT INTO ticket_dependencies (ticket_id, depends_on_ticket_id, kind) \
             VALUES (?, ?, 'phase')",
        )
        .bind(i64_from_u64(ticket_id.0))
        .bind(i64_from_u64(depends_on.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("ticket_dependencies insert: {e}")))?;
        Ok(())
    }

    async fn add_dependency(
        &self,
        ticket_id: TicketId,
        depends_on: TicketId,
    ) -> Result<(), VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        self.add_dependency_in_tx(&mut tx, ticket_id, depends_on)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(())
    }

    async fn mark_ready_if_unblocked_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        ticket_id: TicketId,
        now: OffsetDateTime,
    ) -> Result<Vec<Ticket>, VoomError> {
        // Lean state probe (one column, by PK). Replaces the previous
        // wide `get_in_tx_inner` pre-read whose only consumer was the
        // pending-state gate below. The post-read after the UPDATE is
        // gone — we use `RETURNING` instead.
        let state: Option<String> = sqlx::query_scalar("SELECT state FROM tickets WHERE id = ?")
            .bind(i64_from_u64(ticket_id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("tickets state probe: {e}")))?;
        match state.as_deref() {
            None => return Err(VoomError::NotFound(format!("ticket {ticket_id}"))),
            Some("pending") => {}
            Some(_) => return Ok(Vec::new()),
        }
        // Count unsucceeded dependencies.
        let unsucceeded: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM ticket_dependencies td \
               JOIN tickets t ON t.id = td.depends_on_ticket_id \
              WHERE td.ticket_id = ? AND t.state != 'succeeded'",
        )
        .bind(i64_from_u64(ticket_id.0))
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("dependency count: {e}")))?;
        if unsucceeded.0 > 0 {
            return Ok(Vec::new());
        }
        let ts = iso8601(now)?;
        let row = sqlx::query(&format!(
            "UPDATE tickets SET state = 'ready', state_changed_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND state = 'pending' \
             RETURNING {TICKET_RETURNING_COLS}"
        ))
        .bind(&ts)
        .bind(i64_from_u64(ticket_id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("tickets update: {e}")))?;
        let promoted = row
            .as_ref()
            .map(row_to_ticket)
            .transpose()?
            .ok_or_else(|| {
                VoomError::Conflict(format!(
                    "tickets mark_ready_if_unblocked: id={ticket_id} no longer pending"
                ))
            })?;
        Ok(vec![promoted])
    }

    async fn mark_ready_if_unblocked(
        &self,
        ticket_id: TicketId,
        now: OffsetDateTime,
    ) -> Result<Vec<Ticket>, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self
            .mark_ready_if_unblocked_in_tx(&mut tx, ticket_id, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn get(&self, id: TicketId) -> Result<Option<Ticket>, VoomError> {
        let row = sqlx::query(SELECT_TICKET_BY_ID)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("tickets get: {e}")))?;
        row.as_ref().map(row_to_ticket).transpose()
    }

    async fn get_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: TicketId,
    ) -> Result<Option<Ticket>, VoomError> {
        get_in_tx_inner(tx, id).await
    }

    async fn list_by_state(
        &self,
        state: TicketState,
        limit: u32,
    ) -> Result<Vec<Ticket>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, job_id, kind, state, priority, payload, result, attempt, \
                    max_attempts, next_eligible_at, created_at, state_changed_at, epoch \
             FROM tickets WHERE state = ? \
             ORDER BY priority DESC, next_eligible_at ASC, id ASC LIMIT ?",
        )
        .bind(state.as_str())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("tickets list: {e}")))?;
        rows.iter().map(row_to_ticket).collect()
    }

    async fn next_ready_for_operations_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        operations: &[TicketOperation],
        now: OffsetDateTime,
    ) -> Result<Option<Ticket>, VoomError> {
        Ok(self
            .ready_for_operations_in_tx(tx, operations, now)
            .await?
            .into_iter()
            .next())
    }

    async fn ready_for_operations_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        operations: &[TicketOperation],
        now: OffsetDateTime,
    ) -> Result<Vec<Ticket>, VoomError> {
        if operations.is_empty() {
            return Ok(Vec::new());
        }

        let ts = iso8601(now)?;
        let mut query = QueryBuilder::new(
            "SELECT id, job_id, kind, state, priority, payload, result, attempt, \
                    max_attempts, next_eligible_at, created_at, state_changed_at, epoch \
             FROM tickets \
             WHERE state = 'ready' \
               AND next_eligible_at <= ",
        );
        query.push_bind(ts);
        query.push(" AND attempt < max_attempts AND kind IN (");
        let mut separated = query.separated(", ");
        for operation in operations {
            separated.push_bind(operation.as_str());
        }
        separated.push_unseparated(") ");
        query.push("ORDER BY priority DESC, next_eligible_at ASC, id ASC");

        let rows =
            query.build().fetch_all(&mut **tx).await.map_err(|e| {
                VoomError::Database(format!("tickets next_ready_for_operations: {e}"))
            })?;
        rows.iter().map(row_to_ticket).collect()
    }

    async fn next_ready_for_operations(
        &self,
        operations: &[TicketOperation],
        now: OffsetDateTime,
    ) -> Result<Option<Ticket>, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self
            .next_ready_for_operations_in_tx(&mut tx, operations, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn list_dependents(&self, depends_on: TicketId) -> Result<Vec<Ticket>, VoomError> {
        let rows = sqlx::query(SELECT_DEPENDENTS_OF)
            .bind(i64_from_u64(depends_on.0))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("tickets list_dependents: {e}")))?;
        rows.iter().map(row_to_ticket).collect()
    }

    async fn list_dependents_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        depends_on: TicketId,
    ) -> Result<Vec<Ticket>, VoomError> {
        let rows = sqlx::query(SELECT_DEPENDENTS_OF)
            .bind(i64_from_u64(depends_on.0))
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("tickets list_dependents_in_tx: {e}")))?;
        rows.iter().map(row_to_ticket).collect()
    }
}

const SELECT_TICKET_BY_ID: &str = "SELECT id, job_id, kind, state, priority, payload, result, attempt, \
            max_attempts, next_eligible_at, created_at, state_changed_at, epoch \
     FROM tickets WHERE id = ?";

/// Column list for `UPDATE tickets ... RETURNING <cols>`. Mirrors the
/// projection in `SELECT_TICKET_BY_ID` so `row_to_ticket` can decode
/// the returned row uniformly.
const TICKET_RETURNING_COLS: &str = "id, job_id, kind, state, priority, payload, result, attempt, \
     max_attempts, next_eligible_at, created_at, state_changed_at, epoch";

const SELECT_DEPENDENTS_OF: &str = concat!(
    "SELECT t.id, t.job_id, t.kind, t.state, t.priority, t.payload, t.result, ",
    "t.attempt, t.max_attempts, t.next_eligible_at, t.created_at, ",
    "t.state_changed_at, t.epoch ",
    "FROM tickets t ",
    "JOIN ticket_dependencies td ON td.ticket_id = t.id ",
    "WHERE td.depends_on_ticket_id = ? ",
    "ORDER BY t.id ASC",
);

async fn get_in_tx_inner(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: TicketId,
) -> Result<Option<Ticket>, VoomError> {
    let row = sqlx::query(SELECT_TICKET_BY_ID)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("tickets get_in_tx: {e}")))?;
    row.as_ref().map(row_to_ticket).transpose()
}

fn row_to_ticket(row: &sqlx::sqlite::SqliteRow) -> Result<Ticket, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("tickets", &e))?;
    let job_id: Option<i64> = row
        .try_get("job_id")
        .map_err(|e| map_row_err("tickets", &e))?;
    let kind: String = row
        .try_get("kind")
        .map_err(|e| map_row_err("tickets", &e))?;
    let state: String = row
        .try_get("state")
        .map_err(|e| map_row_err("tickets", &e))?;
    let priority: i64 = row
        .try_get("priority")
        .map_err(|e| map_row_err("tickets", &e))?;
    let payload: String = row
        .try_get("payload")
        .map_err(|e| map_row_err("tickets", &e))?;
    let result: Option<String> = row
        .try_get("result")
        .map_err(|e| map_row_err("tickets", &e))?;
    let attempt: i64 = row
        .try_get("attempt")
        .map_err(|e| map_row_err("tickets", &e))?;
    let max_attempts: i64 = row
        .try_get("max_attempts")
        .map_err(|e| map_row_err("tickets", &e))?;
    let next_eligible: String = row
        .try_get("next_eligible_at")
        .map_err(|e| map_row_err("tickets", &e))?;
    let created: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("tickets", &e))?;
    let state_changed: String = row
        .try_get("state_changed_at")
        .map_err(|e| map_row_err("tickets", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("tickets", &e))?;
    let payload_v: JsonValue = serde_json::from_str(&payload)
        .map_err(|e| VoomError::Database(format!("parse payload: {e}")))?;
    let result_v = result
        .map(|s| serde_json::from_str::<JsonValue>(&s))
        .transpose()
        .map_err(|e| VoomError::Database(format!("parse result: {e}")))?;
    Ok(Ticket {
        id: TicketId(u64_from_i64(id)),
        job_id: job_id.map(|j| JobId(u64_from_i64(j))),
        kind: TicketOperation::from_stored(kind, "tickets.kind")?,
        state: TicketState::parse(&state)?,
        priority,
        payload: payload_v,
        result: result_v,
        attempt: u32_from_i64(attempt)?,
        max_attempts: u32_from_i64(max_attempts)?,
        next_eligible_at: parse_iso8601(&next_eligible)?,
        created_at: parse_iso8601(&created)?,
        state_changed_at: parse_iso8601(&state_changed)?,
        epoch: u64_from_i64(epoch),
    })
}

#[cfg(test)]
#[path = "tickets_test.rs"]
mod tests;
