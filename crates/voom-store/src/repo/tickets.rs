//! `TicketRepo` — owns tickets + `ticket_dependencies`.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{JobId, TicketId, VoomError};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};

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
    pub kind: String,
    pub priority: i64,
    pub payload: JsonValue,
    pub max_attempts: u32,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct Ticket {
    pub id: TicketId,
    pub job_id: Option<JobId>,
    pub kind: String,
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
        .bind(&input.kind)
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
        // Only pending tickets are candidates.
        let current = get_in_tx_inner(tx, ticket_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("ticket {ticket_id}")))?;
        if current.state != TicketState::Pending {
            return Ok(Vec::new());
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
        let res = sqlx::query(
            "UPDATE tickets SET state = 'ready', state_changed_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND state = 'pending' AND epoch = ?",
        )
        .bind(&ts)
        .bind(i64_from_u64(ticket_id.0))
        .bind(i64_from_u64(current.epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("tickets update: {e}")))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "tickets mark_ready_if_unblocked: id={ticket_id} epoch raced"
            )));
        }
        let promoted = get_in_tx_inner(tx, ticket_id).await?.ok_or_else(|| {
            VoomError::Internal(format!("ticket {ticket_id} vanished post-promote"))
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
        kind,
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
