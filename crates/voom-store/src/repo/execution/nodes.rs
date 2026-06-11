//! `SqliteNodeRepo` — owns durable node identity rows.

use std::fmt;

use serde_json::Value as JsonValue;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use time::{Duration, OffsetDateTime};
use voom_core::{NodeId, VoomError};
pub use voom_core::{NodeKind, NodeStatus};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};

#[derive(Clone)]
pub struct NewNode {
    pub name: String,
    pub kind: NodeKind,
    pub registered_at: OffsetDateTime,
    pub heartbeat_ttl_seconds: u32,
    pub auth_token_hash: String,
    pub auth_token_hint: String,
    pub metadata: JsonValue,
}

impl fmt::Debug for NewNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewNode")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("registered_at", &self.registered_at)
            .field("heartbeat_ttl_seconds", &self.heartbeat_ttl_seconds)
            .field("auth_token_hash", &"<secret>")
            .field("auth_token_hint", &self.auth_token_hint)
            .field("metadata", &self.metadata)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub name: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub registered_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
    pub heartbeat_ttl_seconds: u32,
    pub auth_token_hint: String,
    pub metadata: JsonValue,
    pub epoch: u64,
}

#[derive(Clone)]
pub struct NodeAuthRecord {
    pub id: NodeId,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub last_seen_at: OffsetDateTime,
    pub heartbeat_ttl_seconds: u32,
    pub auth_token_hash: String,
}

impl fmt::Debug for NodeAuthRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeAuthRecord")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field("last_seen_at", &self.last_seen_at)
            .field("heartbeat_ttl_seconds", &self.heartbeat_ttl_seconds)
            .field("auth_token_hash", &"<secret>")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct SqliteNodeRepo {
    pool: SqlitePool,
}

impl SqliteNodeRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteNodeRepo {}

impl SqliteNodeRepo {
    pub async fn register_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewNode,
    ) -> Result<Node, VoomError> {
        let ts = iso8601(input.registered_at)?;
        let metadata = serialize_json(&input.metadata, "nodes.metadata")?;
        let res = sqlx::query(
            "INSERT INTO nodes \
             (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
              auth_token_hash, auth_token_hint, metadata) \
             VALUES (?, ?, 'registered', ?, ?, ?, ?, ?, ?)",
        )
        .bind(&input.name)
        .bind(input.kind.as_str())
        .bind(&ts)
        .bind(&ts)
        .bind(i64::from(input.heartbeat_ttl_seconds))
        .bind(&input.auth_token_hash)
        .bind(&input.auth_token_hint)
        .bind(metadata)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("nodes insert", e))?;
        Ok(Node {
            id: NodeId(u64_from_i64(res.last_insert_rowid())),
            name: input.name,
            kind: input.kind,
            status: NodeStatus::Registered,
            registered_at: input.registered_at,
            last_seen_at: input.registered_at,
            retired_at: None,
            heartbeat_ttl_seconds: input.heartbeat_ttl_seconds,
            auth_token_hint: input.auth_token_hint,
            metadata: input.metadata,
            epoch: 0,
        })
    }

    pub async fn get(&self, id: NodeId) -> Result<Option<Node>, VoomError> {
        let row = sqlx::query(
            "SELECT id, name, kind, status, registered_at, last_seen_at, retired_at, \
             heartbeat_ttl_seconds, auth_token_hint, metadata, epoch \
             FROM nodes WHERE id = ?",
        )
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("nodes get", e))?;
        row.as_ref().map(row_to_node).transpose()
    }

    pub async fn list(
        &self,
        status: Option<NodeStatus>,
        limit: u32,
    ) -> Result<Vec<Node>, VoomError> {
        let status = status.map(NodeStatus::as_str);
        let rows = sqlx::query(
            "SELECT id, name, kind, status, registered_at, last_seen_at, retired_at, \
             heartbeat_ttl_seconds, auth_token_hint, metadata, epoch \
             FROM nodes WHERE (? IS NULL OR status = ?) \
             ORDER BY registered_at ASC, id ASC LIMIT ?",
        )
        .bind(status)
        .bind(status)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("nodes list", e))?;
        rows.iter().map(row_to_node).collect()
    }

    pub async fn auth_record_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: NodeId,
    ) -> Result<Option<NodeAuthRecord>, VoomError> {
        let row = sqlx::query(
            "SELECT id, kind, status, last_seen_at, heartbeat_ttl_seconds, auth_token_hash \
             FROM nodes WHERE id = ?",
        )
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("nodes auth record", e))?;
        row.as_ref().map(row_to_auth_record).transpose()
    }

    pub async fn heartbeat_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: NodeId,
        now: OffsetDateTime,
    ) -> Result<Node, VoomError> {
        let current = get_in_tx(tx, id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("nodes heartbeat: id={id} not found")))?;
        if current.status == NodeStatus::Retired {
            return Err(VoomError::Conflict(format!(
                "nodes heartbeat rejected: id={id} is retired"
            )));
        }
        let ts = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE nodes \
             SET status = 'active', last_seen_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND status IN ('registered','active','stale')",
        )
        .bind(&ts)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("nodes heartbeat", e))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "nodes heartbeat rejected: id={id} status changed during update"
            )));
        }
        get_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("nodes heartbeat: row vanished post-update id={id}"))
        })
    }

    pub async fn mark_stale_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        now: OffsetDateTime,
    ) -> Result<Vec<Node>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, name, kind, status, registered_at, last_seen_at, retired_at, \
             heartbeat_ttl_seconds, auth_token_hint, metadata, epoch \
             FROM nodes WHERE status IN ('registered','active') \
             ORDER BY last_seen_at ASC, id ASC",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("nodes stale candidates", e))?;
        let mut changed = Vec::new();
        for row in &rows {
            let node = row_to_node(row)?;
            if let Some(node) = mark_stale_candidate_in_tx(tx, &node, now).await? {
                changed.push(node);
            }
        }
        Ok(changed)
    }

    pub async fn retire_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: NodeId,
        expected_epoch: u64,
        now: OffsetDateTime,
    ) -> Result<Node, VoomError> {
        let current = get_in_tx(tx, id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("nodes retire: id={id} not found")))?;
        if current.status == NodeStatus::Retired {
            return Err(VoomError::Conflict(format!(
                "nodes retire rejected: id={id} already retired"
            )));
        }
        if current.epoch != expected_epoch {
            return Err(VoomError::Conflict(format!(
                "nodes retire rejected: id={id} expected_epoch={expected_epoch} actual_epoch={}",
                current.epoch
            )));
        }
        let ts = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE nodes \
             SET status = 'retired', retired_at = ?, last_seen_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ? AND status != 'retired'",
        )
        .bind(&ts)
        .bind(&ts)
        .bind(i64_from_u64(id.0))
        .bind(i64_from_u64(expected_epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("nodes retire", e))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "nodes retire rejected: id={id} expected_epoch={expected_epoch} \
                 changed during update"
            )));
        }
        get_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("nodes retire: row vanished post-update id={id}"))
        })
    }
}

async fn mark_stale_candidate_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    node: &Node,
    now: OffsetDateTime,
) -> Result<Option<Node>, VoomError> {
    let expires_at = node.last_seen_at + Duration::seconds(i64::from(node.heartbeat_ttl_seconds));
    if node.status == NodeStatus::Stale || expires_at > now {
        return Ok(None);
    }

    let last_seen_at = iso8601(node.last_seen_at)?;
    let res = sqlx::query(
        "UPDATE nodes \
         SET status = 'stale', epoch = epoch + 1 \
         WHERE id = ? \
           AND status IN ('registered','active') \
           AND last_seen_at = ? \
           AND heartbeat_ttl_seconds = ? \
           AND epoch = ?",
    )
    .bind(i64_from_u64(node.id.0))
    .bind(last_seen_at)
    .bind(i64::from(node.heartbeat_ttl_seconds))
    .bind(i64_from_u64(node.epoch))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("nodes mark stale", e))?;
    if res.rows_affected() == 0 {
        return Ok(None);
    }

    get_in_tx(tx, node.id).await?.map(Some).ok_or_else(|| {
        VoomError::Internal(format!(
            "nodes mark stale: row vanished post-update id={}",
            node.id
        ))
    })
}

async fn get_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: NodeId,
) -> Result<Option<Node>, VoomError> {
    let row = sqlx::query(
        "SELECT id, name, kind, status, registered_at, last_seen_at, retired_at, \
         heartbeat_ttl_seconds, auth_token_hint, metadata, epoch \
         FROM nodes WHERE id = ?",
    )
    .bind(i64_from_u64(id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("nodes reload", e))?;
    row.as_ref().map(row_to_node).transpose()
}

fn row_to_node(row: &sqlx::sqlite::SqliteRow) -> Result<Node, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("nodes", &e))?;
    let name: String = row.try_get("name").map_err(|e| map_row_err("nodes", &e))?;
    let kind: String = row.try_get("kind").map_err(|e| map_row_err("nodes", &e))?;
    let status: String = row
        .try_get("status")
        .map_err(|e| map_row_err("nodes", &e))?;
    let registered: String = row
        .try_get("registered_at")
        .map_err(|e| map_row_err("nodes", &e))?;
    let last_seen: String = row
        .try_get("last_seen_at")
        .map_err(|e| map_row_err("nodes", &e))?;
    let retired: Option<String> = row
        .try_get("retired_at")
        .map_err(|e| map_row_err("nodes", &e))?;
    let heartbeat_ttl_seconds: i64 = row
        .try_get("heartbeat_ttl_seconds")
        .map_err(|e| map_row_err("nodes", &e))?;
    let auth_token_hint: String = row
        .try_get("auth_token_hint")
        .map_err(|e| map_row_err("nodes", &e))?;
    let metadata: String = row
        .try_get("metadata")
        .map_err(|e| map_row_err("nodes", &e))?;
    let epoch: i64 = row.try_get("epoch").map_err(|e| map_row_err("nodes", &e))?;
    Ok(Node {
        id: NodeId(u64_from_i64(id)),
        name,
        kind: NodeKind::parse_database("nodes.kind", &kind)?,
        status: NodeStatus::parse_database("nodes.status", &status)?,
        registered_at: parse_iso8601(&registered)?,
        last_seen_at: parse_iso8601(&last_seen)?,
        retired_at: retired.map(|s| parse_iso8601(&s)).transpose()?,
        heartbeat_ttl_seconds: u32_from_i64(heartbeat_ttl_seconds)?,
        auth_token_hint,
        metadata: serde_json::from_str(&metadata)
            .map_err(|e| VoomError::database_context("nodes.metadata decode", e))?,
        epoch: u64_from_i64(epoch),
    })
}

fn row_to_auth_record(row: &sqlx::sqlite::SqliteRow) -> Result<NodeAuthRecord, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("nodes", &e))?;
    let kind: String = row.try_get("kind").map_err(|e| map_row_err("nodes", &e))?;
    let status: String = row
        .try_get("status")
        .map_err(|e| map_row_err("nodes", &e))?;
    let last_seen: String = row
        .try_get("last_seen_at")
        .map_err(|e| map_row_err("nodes", &e))?;
    let heartbeat_ttl_seconds: i64 = row
        .try_get("heartbeat_ttl_seconds")
        .map_err(|e| map_row_err("nodes", &e))?;
    let auth_token_hash: String = row
        .try_get("auth_token_hash")
        .map_err(|e| map_row_err("nodes", &e))?;
    Ok(NodeAuthRecord {
        id: NodeId(u64_from_i64(id)),
        kind: NodeKind::parse_database("nodes.kind", &kind)?,
        status: NodeStatus::parse_database("nodes.status", &status)?,
        last_seen_at: parse_iso8601(&last_seen)?,
        heartbeat_ttl_seconds: u32_from_i64(heartbeat_ttl_seconds)?,
        auth_token_hash,
    })
}

#[cfg(test)]
#[path = "nodes_test.rs"]
mod tests;
