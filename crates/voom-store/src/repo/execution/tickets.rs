//! `SqliteTicketRepo` — owns tickets + `ticket_dependencies`.

use rand::RngCore;
use serde_json::Value as JsonValue;
use sqlx::{QueryBuilder, Row, SqlitePool};
use time::{Duration, OffsetDateTime};
use voom_core::{FileVersionId, JobId, TicketId, TicketOperation, VoomError};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};

/// Default backoff window: capped exponential with full jitter.
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
            other => Err(VoomError::database(format!(
                "tickets.state {other:?} not in vocab"
            ))),
        }
    }
}

/// Filter for the keyset-paginated `SqliteTicketRepo::list` inspection read.
#[derive(Debug, Clone, Default)]
pub struct TicketFilter {
    pub state: Option<TicketState>,
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

#[derive(Debug, Clone, PartialEq)]
pub struct SucceededTicketResult {
    pub ticket_id: TicketId,
    pub result: JsonValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTicketFacts {
    pub unfinished: u32,
    pub ready: u32,
    pub leased: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowTicketIdentity<'a> {
    pub job_id: JobId,
    pub workflow_id: &'a str,
    pub branch_id: &'a str,
    pub node_id: &'a str,
    pub source_file_version_id: Option<FileVersionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreLeaseFailureTransition {
    Terminal,
    RetryAt(OffsetDateTime),
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

    /// Default backoff window after a retriable failure: capped
    /// exponential with full jitter.
    ///
    /// The current value is `random_between(0, min(cap, base * 2^attempt))`
    /// with `base = 5s` and `cap = 300s`.
    pub fn default_backoff(attempt: u32, rng: &mut (dyn RngCore + Send)) -> Duration {
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

impl Repository for SqliteTicketRepo {}

impl SqliteTicketRepo {
    /// Create a pending ticket in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the row cannot be inserted or decoded,
    /// and [`VoomError::Internal`] if the payload or timestamp cannot be encoded
    /// or the inserted row cannot be re-read.
    pub async fn create_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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
        .map_err(|e| VoomError::database_context("tickets insert", e))?;
        let id = TicketId(u64_from_i64(res.last_insert_rowid()));
        // Re-read to return the canonical row.
        get_in_tx_inner(tx, id)
            .await?
            .ok_or_else(|| VoomError::Internal(format!("tickets create: row vanished id={id}")))
    }

    /// Create and commit a pending ticket.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::create_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn create(&self, input: NewTicket) -> Result<Ticket, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.create_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Return succeeded, result-bearing tickets for one job and operation.
    ///
    /// Results are ordered by ticket id so consumers preserve durable ticket
    /// order when a result contains multiple projected records.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if rows or persisted JSON cannot be
    /// read or decoded.
    pub async fn succeeded_results_for_job_and_operation(
        &self,
        job_id: JobId,
        operation: TicketOperation,
    ) -> Result<Vec<SucceededTicketResult>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, result FROM tickets \
             WHERE job_id = ? AND kind = ? AND state = 'succeeded' AND result IS NOT NULL \
             ORDER BY id ASC",
        )
        .bind(i64_from_u64(job_id.0))
        .bind(operation.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("succeeded ticket results", error))?;

        let mut results = Vec::with_capacity(rows.len());
        for row in rows {
            let ticket_id: i64 = row
                .try_get("id")
                .map_err(|error| map_row_err("succeeded ticket result ticket id", &error))?;
            let result: String = row
                .try_get("result")
                .map_err(|error| map_row_err("succeeded ticket result", &error))?;
            results.push(SucceededTicketResult {
                ticket_id: TicketId(u64_from_i64(ticket_id)),
                result: serde_json::from_str(&result).map_err(|error| {
                    VoomError::database_context("parse succeeded ticket result", error)
                })?,
            });
        }
        Ok(results)
    }

    /// Add a dependency to a pending ticket in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the dependent ticket is missing,
    /// [`VoomError::Conflict`] when it is no longer pending,
    /// [`VoomError::DependencyCycle`] for a self-edge or transitive cycle, and
    /// [`VoomError::Database`] for query or constraint failures.
    pub async fn add_dependency_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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
            .map_err(|e| VoomError::database_context("ticket state probe", e))?;
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
        .map_err(|e| VoomError::database_context("cycle check", e))?;
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
        .map_err(|e| VoomError::database_context("ticket_dependencies insert", e))?;
        Ok(())
    }

    /// Add and commit a dependency to a pending ticket.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::add_dependency_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn add_dependency(
        &self,
        ticket_id: TicketId,
        depends_on: TicketId,
    ) -> Result<(), VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        self.add_dependency_in_tx(&mut tx, ticket_id, depends_on)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(())
    }

    /// Report whether an exact dependency edge exists in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the existence query fails.
    pub async fn dependency_exists_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        ticket_id: TicketId,
        depends_on: TicketId,
    ) -> Result<bool, VoomError> {
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM ticket_dependencies \
             WHERE ticket_id = ? AND depends_on_ticket_id = ? \
             LIMIT 1",
        )
        .bind(i64_from_u64(ticket_id.0))
        .bind(i64_from_u64(depends_on.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("workflow dependency lookup", e))?;
        Ok(exists.is_some())
    }

    /// Promote a pending ticket when all dependencies have succeeded.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the ticket does not exist,
    /// [`VoomError::Conflict`] when it changes during promotion,
    /// [`VoomError::Database`] for query or row-decoding failures, and
    /// [`VoomError::Internal`] if the timestamp cannot be encoded.
    pub async fn mark_ready_if_unblocked_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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
            .map_err(|e| VoomError::database_context("tickets state probe", e))?;
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
        .map_err(|e| VoomError::database_context("dependency count", e))?;
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
        .map_err(|e| VoomError::database_context("tickets update", e))?;
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

    /// Promote and commit a pending ticket when all dependencies have succeeded.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::mark_ready_if_unblocked_in_tx`],
    /// or [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn mark_ready_if_unblocked(
        &self,
        ticket_id: TicketId,
        now: OffsetDateTime,
    ) -> Result<Vec<Ticket>, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self
            .mark_ready_if_unblocked_in_tx(&mut tx, ticket_id, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Look up a ticket by id.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn get(&self, id: TicketId) -> Result<Option<Ticket>, VoomError> {
        let row = sqlx::query(SELECT_TICKET_BY_ID)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("tickets get", e))?;
        row.as_ref().map(row_to_ticket).transpose()
    }

    /// Look up a ticket by id in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn get_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: TicketId,
    ) -> Result<Option<Ticket>, VoomError> {
        get_in_tx_inner(tx, id).await
    }

    /// List the distinct succeeded node ids for one workflow in lexical order.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the projection cannot be queried or decoded.
    pub async fn succeeded_workflow_node_ids(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<Vec<String>, VoomError> {
        sqlx::query_scalar(
            "SELECT DISTINCT json_extract(payload, '$.node_id') FROM tickets \
             WHERE job_id = ? \
               AND state = 'succeeded' \
               AND json_extract(payload, '$.workflow_id') = ? \
               AND json_type(payload, '$.node_id') = 'text' \
             ORDER BY json_extract(payload, '$.node_id') ASC",
        )
        .bind(i64_from_u64(job_id.0))
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workflow succeeded node ids", e))
    }

    /// Report whether one workflow node already has a ticket in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the existence query fails.
    pub async fn workflow_ticket_exists_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        job_id: JobId,
        workflow_id: &str,
        branch_id: &str,
        node_id: &str,
    ) -> Result<bool, VoomError> {
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM tickets \
             WHERE job_id = ? \
               AND json_extract(payload, '$.workflow_id') = ? \
               AND json_extract(payload, '$.branch_id') = ? \
               AND json_extract(payload, '$.node_id') = ? \
             LIMIT 1",
        )
        .bind(i64_from_u64(job_id.0))
        .bind(workflow_id)
        .bind(branch_id)
        .bind(node_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("workflow ticket existence", e))?;
        Ok(exists.is_some())
    }

    /// List eligible ready tickets for one workflow in scheduling order.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or typed row decoding fails, and
    /// [`VoomError::Internal`] if the eligibility timestamp cannot be encoded.
    pub async fn ready_workflow_tickets(
        &self,
        job_id: JobId,
        workflow_id: &str,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Ticket>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {TICKET_RETURNING_COLS} FROM tickets \
             WHERE job_id = ? \
               AND state = 'ready' \
               AND next_eligible_at <= ? \
               AND json_extract(payload, '$.workflow_id') = ? \
             ORDER BY priority DESC, next_eligible_at ASC, id ASC \
             LIMIT ?"
        ))
        .bind(i64_from_u64(job_id.0))
        .bind(iso8601(now)?)
        .bind(workflow_id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workflow ready tickets", e))?;
        rows.iter().map(row_to_ticket).collect()
    }

    /// Return execution-state counts for one workflow.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the projection cannot be queried or decoded.
    pub async fn workflow_ticket_facts(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<WorkflowTicketFacts, VoomError> {
        let (unfinished, ready, leased, failed): (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT \
               COALESCE(SUM(state IN ('pending', 'ready', 'leased')), 0), \
               COALESCE(SUM(state = 'ready'), 0), \
               COALESCE(SUM(state = 'leased'), 0), \
               COALESCE(SUM(state = 'failed'), 0) \
             FROM tickets \
             WHERE job_id = ? AND json_extract(payload, '$.workflow_id') = ?",
        )
        .bind(i64_from_u64(job_id.0))
        .bind(workflow_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workflow ticket facts", e))?;
        Ok(WorkflowTicketFacts {
            unfinished: u32_from_i64(unfinished)?,
            ready: u32_from_i64(ready)?,
            leased: u32_from_i64(leased)?,
            failed: u32_from_i64(failed)?,
        })
    }

    /// Return the earliest-created failed ticket for one workflow.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or typed row decoding fails.
    pub async fn first_failed_workflow_ticket(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<Option<Ticket>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {TICKET_RETURNING_COLS} FROM tickets \
             WHERE job_id = ? \
               AND state = 'failed' \
               AND json_extract(payload, '$.workflow_id') = ? \
             ORDER BY id ASC LIMIT 1"
        ))
        .bind(i64_from_u64(job_id.0))
        .bind(workflow_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("first failed workflow ticket", e))?;
        row.as_ref().map(row_to_ticket).transpose()
    }

    /// Return the earliest future eligibility timestamp among ready workflow tickets.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the projection cannot be queried or decoded, and
    /// [`VoomError::Internal`] if the comparison timestamp cannot be encoded.
    pub async fn retry_eligible_at(
        &self,
        job_id: JobId,
        workflow_id: &str,
        now: OffsetDateTime,
    ) -> Result<Option<OffsetDateTime>, VoomError> {
        let eligible_at: Option<String> = sqlx::query_scalar(
            "SELECT MIN(next_eligible_at) FROM tickets \
             WHERE job_id = ? \
               AND state = 'ready' \
               AND next_eligible_at > ? \
               AND json_extract(payload, '$.workflow_id') = ?",
        )
        .bind(i64_from_u64(job_id.0))
        .bind(iso8601(now)?)
        .bind(workflow_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workflow retry eligibility", e))?;
        eligible_at.as_deref().map(parse_iso8601).transpose()
    }

    /// Find the unique ticket for one durable workflow phase identity.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] if multiple tickets share the identity and
    /// [`VoomError::Database`] if the projection cannot be queried or decoded.
    pub async fn find_workflow_ticket_id_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        identity: WorkflowTicketIdentity<'_>,
    ) -> Result<Option<TicketId>, VoomError> {
        let source_file_version_id = identity
            .source_file_version_id
            .map(|file_version_id| i64_from_u64(file_version_id.0));
        let rows: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM tickets \
             WHERE job_id = ? \
               AND json_extract(payload, '$.workflow_id') = ? \
               AND json_extract(payload, '$.branch_id') = ? \
               AND json_extract(payload, '$.node_id') = ? \
               AND ((? IS NULL \
                     AND json_extract( \
                         payload, '$.rendered_payload.source_file_version_id' \
                     ) IS NULL) \
                    OR json_extract( \
                         payload, '$.rendered_payload.source_file_version_id' \
                       ) = ?) \
             ORDER BY id ASC LIMIT 2",
        )
        .bind(i64_from_u64(identity.job_id.0))
        .bind(identity.workflow_id)
        .bind(identity.branch_id)
        .bind(identity.node_id)
        .bind(source_file_version_id)
        .bind(source_file_version_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("workflow ticket identity lookup", e))?;
        match rows.as_slice() {
            [] => Ok(None),
            [id] => Ok(Some(TicketId(u64_from_i64(*id)))),
            [first, second, ..] => Err(VoomError::Conflict(format!(
                "duplicate workflow tickets for job {} workflow `{}` branch `{}` node `{}`: \
                 ids {first}, {second}",
                identity.job_id, identity.workflow_id, identity.branch_id, identity.node_id
            ))),
        }
    }

    /// Transition a ready ticket after failure before any lease exists.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when the ready-state, prior-attempt, or
    /// eligibility predicate no longer holds, and [`VoomError::Database`] or
    /// [`VoomError::Internal`] when the mutation cannot be completed or decoded.
    pub async fn transition_ready_before_lease_failure_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        ticket_id: TicketId,
        previous_attempt: u32,
        next_attempt: u32,
        transition: PreLeaseFailureTransition,
        now: OffsetDateTime,
    ) -> Result<Ticket, VoomError> {
        let now = iso8601(now)?;
        let (state, next_eligible_at) = match transition {
            PreLeaseFailureTransition::Terminal => (TicketState::Failed.as_str(), None),
            PreLeaseFailureTransition::RetryAt(at) => {
                (TicketState::Ready.as_str(), Some(iso8601(at)?))
            }
        };
        let row = sqlx::query(&format!(
            "UPDATE tickets SET state = ?, state_changed_at = ?, attempt = ?, \
             next_eligible_at = COALESCE(?, next_eligible_at), epoch = epoch + 1 \
             WHERE id = ? AND state = 'ready' AND attempt = ? AND next_eligible_at <= ? \
             RETURNING {TICKET_RETURNING_COLS}"
        ))
        .bind(state)
        .bind(&now)
        .bind(i64::from(next_attempt))
        .bind(next_eligible_at)
        .bind(i64_from_u64(ticket_id.0))
        .bind(i64::from(previous_attempt))
        .bind(&now)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("pre-lease ticket transition", e))?;
        row.as_ref().map(row_to_ticket).transpose()?.ok_or_else(|| {
            VoomError::Conflict(format!(
                "pre-lease failure rejected: ticket {ticket_id} changed concurrently"
            ))
        })
    }

    /// Keyset-paginated inspection read for `voom ticket list` (ADR 0031).
    /// Orders strictly by `id` descending (newest first); `after_id` is an
    /// exclusive continuation token returning rows with `id < after_id`.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn list(
        &self,
        filter: TicketFilter,
        after_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<Ticket>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {TICKET_RETURNING_COLS} FROM tickets \
             WHERE (?1 IS NULL OR state = ?1) \
               AND (?2 IS NULL OR id < ?2) \
             ORDER BY id DESC LIMIT ?3"
        ))
        .bind(filter.state.map(TicketState::as_str))
        .bind(after_id.map(i64_from_u64))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("tickets keyset list", e))?;
        rows.iter().map(row_to_ticket).collect()
    }

    /// List tickets in one state using scheduling order.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn list_by_state(
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
        .map_err(|e| VoomError::database_context("tickets list", e))?;
        rows.iter().map(row_to_ticket).collect()
    }

    /// List every ticket for one job in deterministic id order.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or typed row decoding fails.
    pub async fn list_for_job(&self, job_id: JobId) -> Result<Vec<Ticket>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {TICKET_RETURNING_COLS} FROM tickets \
             WHERE job_id = ? ORDER BY id ASC"
        ))
        .bind(i64_from_u64(job_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("tickets list for job", e))?;
        rows.iter().map(row_to_ticket).collect()
    }

    /// Return the highest-priority eligible ticket for the requested operations.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails, and
    /// [`VoomError::Internal`] if the supplied timestamp cannot be encoded.
    pub async fn next_ready_for_operations_in_tx(
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

    /// List eligible tickets for the requested operations in scheduling order.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails, and
    /// [`VoomError::Internal`] if the supplied timestamp cannot be encoded.
    pub async fn ready_for_operations_in_tx(
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
        query.push(
            " AND attempt < max_attempts \
               AND (job_id IS NULL OR EXISTS ( \
                   SELECT 1 FROM jobs \
                   WHERE jobs.id = tickets.job_id AND jobs.state = 'open' \
               )) \
               AND kind IN (",
        );
        let mut separated = query.separated(", ");
        for operation in operations {
            separated.push_bind(operation.as_str());
        }
        separated.push_unseparated(") ");
        query.push("ORDER BY priority DESC, next_eligible_at ASC, id ASC");

        let rows = query
            .build()
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("tickets next_ready_for_operations", e))?;
        rows.iter().map(row_to_ticket).collect()
    }

    /// Return the highest-priority eligible ticket for the requested operations.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::next_ready_for_operations_in_tx`],
    /// or [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn next_ready_for_operations(
        &self,
        operations: &[TicketOperation],
        now: OffsetDateTime,
    ) -> Result<Option<Ticket>, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self
            .next_ready_for_operations_in_tx(&mut tx, operations, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// List tickets that directly depend on the supplied ticket.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn list_dependents(&self, depends_on: TicketId) -> Result<Vec<Ticket>, VoomError> {
        let rows = sqlx::query(SELECT_DEPENDENTS_OF)
            .bind(i64_from_u64(depends_on.0))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("tickets list_dependents", e))?;
        rows.iter().map(row_to_ticket).collect()
    }

    /// List direct dependents in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn list_dependents_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        depends_on: TicketId,
    ) -> Result<Vec<Ticket>, VoomError> {
        let rows = sqlx::query(SELECT_DEPENDENTS_OF)
            .bind(i64_from_u64(depends_on.0))
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("tickets list_dependents_in_tx", e))?;
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
        .map_err(|e| VoomError::database_context("tickets get_in_tx", e))?;
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
        .map_err(|e| VoomError::database_context("parse payload", e))?;
    let result_v = result
        .map(|s| serde_json::from_str::<JsonValue>(&s))
        .transpose()
        .map_err(|e| VoomError::database_context("parse result", e))?;
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
