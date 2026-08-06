//! Durable node-process incarnations and their worker ownership boundary.

use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::{
    NodeId, NodeIncarnationEndReason, NodeIncarnationId, NodeIncarnationStatus, VoomError,
};

use super::Repository;
use super::common::{i64_from_u64, iso8601, map_row_err, parse_iso8601, u64_from_i64};

#[derive(Debug, Clone, Copy)]
pub struct NewNodeIncarnation {
    pub id: NodeIncarnationId,
    pub node_id: NodeId,
    pub started_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeIncarnation {
    pub id: NodeIncarnationId,
    pub node_id: NodeId,
    pub status: NodeIncarnationStatus,
    pub started_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub ended_at: Option<OffsetDateTime>,
    pub end_reason: Option<NodeIncarnationEndReason>,
    pub worker_count: u32,
}

#[derive(Debug, Clone)]
pub struct SqliteNodeIncarnationRepo {
    pool: SqlitePool,
}

impl SqliteNodeIncarnationRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert one active incarnation in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] for constraint or storage failures and
    /// [`VoomError::Internal`] when the timestamp cannot be encoded.
    pub async fn insert_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewNodeIncarnation,
    ) -> Result<NodeIncarnation, VoomError> {
        let started_at = iso8601(input.started_at)?;
        sqlx::query(
            "INSERT INTO node_incarnations \
             (incarnation_id, node_id, status, started_at, last_seen_at) \
             VALUES (?, ?, 'active', ?, ?)",
        )
        .bind(input.id.to_string())
        .bind(i64_from_u64(input.node_id.0, "node_incarnations.node_id")?)
        .bind(&started_at)
        .bind(&started_at)
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("node incarnation insert", error))?;
        self.get_in_tx(tx, input.id).await?.ok_or_else(|| {
            VoomError::Internal(format!(
                "node incarnation {} missing after insert",
                input.id
            ))
        })
    }

    /// Read one incarnation inside the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when persisted fields are corrupt.
    pub async fn get_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: NodeIncarnationId,
    ) -> Result<Option<NodeIncarnation>, VoomError> {
        let row = sqlx::query(
            "SELECT ni.incarnation_id, ni.node_id, ni.status, ni.started_at, \
             ni.last_seen_at, ni.ended_at, ni.end_reason, COUNT(w.id) AS worker_count \
             FROM node_incarnations ni \
             LEFT JOIN workers w ON w.node_incarnation_id = ni.incarnation_id \
             WHERE ni.incarnation_id = ? \
             GROUP BY ni.incarnation_id",
        )
        .bind(id.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("node incarnation get", error))?;
        row.as_ref().map(row_to_incarnation).transpose()
    }

    /// List a node's incarnation history newest first.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when the query or any persisted field is corrupt.
    pub async fn list_for_node(
        &self,
        node_id: NodeId,
        limit: u32,
    ) -> Result<Vec<NodeIncarnation>, VoomError> {
        let rows = sqlx::query(
            "SELECT ni.incarnation_id, ni.node_id, ni.status, ni.started_at, \
             ni.last_seen_at, ni.ended_at, ni.end_reason, COUNT(w.id) AS worker_count \
             FROM node_incarnations ni \
             LEFT JOIN workers w ON w.node_incarnation_id = ni.incarnation_id \
             WHERE ni.node_id = ? \
             GROUP BY ni.incarnation_id \
             ORDER BY ni.started_at DESC, ni.incarnation_id DESC LIMIT ?",
        )
        .bind(i64_from_u64(node_id.0, "node_incarnations.node_id")?)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("node incarnation list", error))?;
        rows.iter().map(row_to_incarnation).collect()
    }

    /// Update the last-seen timestamp for an active incarnation.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the incarnation is absent,
    /// [`VoomError::Conflict`] when it is terminal, and database/internal errors
    /// for corrupt storage or timestamp encoding.
    pub async fn heartbeat_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: NodeIncarnationId,
        now: OffsetDateTime,
    ) -> Result<NodeIncarnation, VoomError> {
        let current = self.get_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::NotFound(format!("node incarnation heartbeat: id={id} not found"))
        })?;
        if current.status != NodeIncarnationStatus::Active {
            return Err(VoomError::Conflict(format!(
                "node incarnation heartbeat rejected: id={id} is {}",
                current.status.as_str()
            )));
        }
        let now = iso8601(now)?;
        let result = sqlx::query(
            "UPDATE node_incarnations SET last_seen_at = ? \
             WHERE incarnation_id = ? AND status = 'active'",
        )
        .bind(now)
        .bind(id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("node incarnation heartbeat", error))?;
        if result.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "node incarnation heartbeat rejected: id={id} changed during update"
            )));
        }
        self.get_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("node incarnation {id} missing after heartbeat"))
        })
    }

    /// End an active incarnation with one valid terminal status/reason pair.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the incarnation is absent,
    /// [`VoomError::Conflict`] when it is already terminal, and database/internal
    /// errors for invalid pairs, corrupt storage, or timestamp encoding.
    pub async fn end_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: NodeIncarnationId,
        status: NodeIncarnationStatus,
        reason: NodeIncarnationEndReason,
        ended_at: OffsetDateTime,
    ) -> Result<NodeIncarnation, VoomError> {
        validate_terminal_pair(status, reason)?;
        let current = self.get_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::NotFound(format!("node incarnation end: id={id} not found"))
        })?;
        if current.status != NodeIncarnationStatus::Active {
            return Err(VoomError::Conflict(format!(
                "node incarnation end rejected: id={id} is {}",
                current.status.as_str()
            )));
        }
        let ended_at = iso8601(ended_at)?;
        let result = sqlx::query(
            "UPDATE node_incarnations \
             SET status = ?, ended_at = ?, end_reason = ? \
             WHERE incarnation_id = ? AND status = 'active'",
        )
        .bind(status.as_str())
        .bind(&ended_at)
        .bind(reason.as_str())
        .bind(id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("node incarnation end", error))?;
        if result.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "node incarnation end rejected: id={id} changed during update"
            )));
        }
        self.get_in_tx(tx, id)
            .await?
            .ok_or_else(|| VoomError::Internal(format!("node incarnation {id} missing after end")))
    }
}

impl Repository for SqliteNodeIncarnationRepo {}

fn row_to_incarnation(row: &sqlx::sqlite::SqliteRow) -> Result<NodeIncarnation, VoomError> {
    let id: String = row
        .try_get("incarnation_id")
        .map_err(|error| map_row_err("node incarnation id", error))?;
    let node_id: i64 = row
        .try_get("node_id")
        .map_err(|error| map_row_err("node incarnation node id", error))?;
    let status: String = row
        .try_get("status")
        .map_err(|error| map_row_err("node incarnation status", error))?;
    let started_at: String = row
        .try_get("started_at")
        .map_err(|error| map_row_err("node incarnation started at", error))?;
    let last_seen_at: String = row
        .try_get("last_seen_at")
        .map_err(|error| map_row_err("node incarnation last seen at", error))?;
    let ended_at: Option<String> = row
        .try_get("ended_at")
        .map_err(|error| map_row_err("node incarnation ended at", error))?;
    let end_reason: Option<String> = row
        .try_get("end_reason")
        .map_err(|error| map_row_err("node incarnation end reason", error))?;
    let worker_count: i64 = row
        .try_get("worker_count")
        .map_err(|error| map_row_err("node incarnation worker count", error))?;

    let id = NodeIncarnationId::parse_database("node incarnation id", &id)?;
    let node_id = NodeId(u64_from_i64(node_id, "node incarnation node id")?);
    let status = NodeIncarnationStatus::parse_database("node incarnation status", &status)?;
    let started_at = parse_iso8601(&started_at)?;
    let last_seen_at = parse_iso8601(&last_seen_at)?;
    let ended_at = ended_at.map(|value| parse_iso8601(&value)).transpose()?;
    let end_reason = end_reason
        .map(|value| {
            NodeIncarnationEndReason::parse_database("node incarnation end reason", &value)
        })
        .transpose()?;
    validate_persisted_pair(status, ended_at, end_reason)?;

    Ok(NodeIncarnation {
        id,
        node_id,
        status,
        started_at,
        last_seen_at,
        ended_at,
        end_reason,
        worker_count: worker_count_from_i64(worker_count)?,
    })
}

fn validate_persisted_pair(
    status: NodeIncarnationStatus,
    ended_at: Option<OffsetDateTime>,
    reason: Option<NodeIncarnationEndReason>,
) -> Result<(), VoomError> {
    let valid = matches!(
        (status, ended_at, reason),
        (NodeIncarnationStatus::Active, None, None)
            | (
                NodeIncarnationStatus::Superseded,
                Some(_),
                Some(NodeIncarnationEndReason::Superseded),
            )
            | (
                NodeIncarnationStatus::Retired,
                Some(_),
                Some(
                    NodeIncarnationEndReason::GracefulShutdown
                        | NodeIncarnationEndReason::LogicalNodeRetired,
                ),
            )
            | (
                NodeIncarnationStatus::Failed,
                Some(_),
                Some(
                    NodeIncarnationEndReason::ChildStartupFailed
                        | NodeIncarnationEndReason::ChildRestartExhausted
                        | NodeIncarnationEndReason::HeartbeatExpired,
                ),
            )
    );
    if valid {
        Ok(())
    } else {
        Err(VoomError::database(format!(
            "node incarnation status/end reason pairing is corrupt: status={} ended_at={} reason={}",
            status.as_str(),
            ended_at.is_some(),
            reason.map_or("null", NodeIncarnationEndReason::as_str)
        )))
    }
}

fn validate_terminal_pair(
    status: NodeIncarnationStatus,
    reason: NodeIncarnationEndReason,
) -> Result<(), VoomError> {
    validate_persisted_pair(status, Some(OffsetDateTime::UNIX_EPOCH), Some(reason)).map_err(|_| {
        VoomError::Internal(format!(
            "invalid node incarnation terminal pair: status={} reason={}",
            status.as_str(),
            reason.as_str()
        ))
    })
}

fn worker_count_from_i64(value: i64) -> Result<u32, VoomError> {
    u32::try_from(value).map_err(|_| {
        VoomError::database(format!(
            "node incarnation worker count does not fit u32: {value}"
        ))
    })
}

#[cfg(test)]
#[path = "node_incarnations_test.rs"]
mod tests;
