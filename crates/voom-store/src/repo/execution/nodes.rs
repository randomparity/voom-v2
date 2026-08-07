//! `SqliteNodeRepo` — owns durable node identity rows.

use std::fmt;

use serde_json::Value as JsonValue;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use time::{Duration, OffsetDateTime};
use voom_core::{NodeId, NodeIncarnationId, VoomError};
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
    pub active_incarnation_id: Option<NodeIncarnationId>,
}

#[derive(Clone)]
pub struct NodeAuthRecord {
    pub id: NodeId,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub last_seen_at: OffsetDateTime,
    pub heartbeat_ttl_seconds: u32,
    pub auth_token_hash: String,
    pub active_incarnation_id: Option<NodeIncarnationId>,
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
            .field("active_incarnation_id", &self.active_incarnation_id)
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
    /// Register a node in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the row cannot be inserted and
    /// [`VoomError::Internal`] if the timestamp or metadata cannot be encoded.
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
            id: NodeId(u64_from_i64(
                res.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res.last_insert_rowid())),
            )?),
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
            active_incarnation_id: None,
        })
    }

    /// Look up a node by id.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn get(&self, id: NodeId) -> Result<Option<Node>, VoomError> {
        let row = sqlx::query(
            "SELECT n.id, n.name, n.kind, n.status, n.registered_at, n.last_seen_at, \
             n.retired_at, n.heartbeat_ttl_seconds, n.auth_token_hint, n.metadata, n.epoch, \
             n.active_incarnation_id, \
             ai.node_id AS active_incarnation_node_id, \
             ai.status AS active_incarnation_status, \
             (SELECT COUNT(*) FROM node_incarnations active \
              WHERE active.node_id = n.id AND active.status = 'active') \
                 AS active_incarnation_count \
             FROM nodes n \
             LEFT JOIN node_incarnations ai ON ai.incarnation_id = n.active_incarnation_id \
             WHERE n.id = ?",
        )
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("nodes get", e))?;
        row.as_ref().map(row_to_node).transpose()
    }

    /// List nodes, optionally filtered by status.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn list(
        &self,
        status: Option<NodeStatus>,
        limit: u32,
    ) -> Result<Vec<Node>, VoomError> {
        let status = status.map(NodeStatus::as_str);
        let rows = sqlx::query(
            "SELECT n.id, n.name, n.kind, n.status, n.registered_at, n.last_seen_at, \
             n.retired_at, n.heartbeat_ttl_seconds, n.auth_token_hint, n.metadata, n.epoch, \
             n.active_incarnation_id, \
             ai.node_id AS active_incarnation_node_id, \
             ai.status AS active_incarnation_status, \
             (SELECT COUNT(*) FROM node_incarnations active \
              WHERE active.node_id = n.id AND active.status = 'active') \
                 AS active_incarnation_count \
             FROM nodes n \
             LEFT JOIN node_incarnations ai ON ai.incarnation_id = n.active_incarnation_id \
             WHERE (? IS NULL OR n.status = ?) \
             ORDER BY n.registered_at ASC, n.id ASC LIMIT ?",
        )
        .bind(status)
        .bind(status)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("nodes list", e))?;
        rows.iter().map(row_to_node).collect()
    }

    /// Read the authentication fields for a node in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn auth_record_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: NodeId,
    ) -> Result<Option<NodeAuthRecord>, VoomError> {
        // An unrepresentable ID cannot identify a SQLite row, so preserve this
        // lookup's absent-record semantics without sending a wrapped value to SQLite.
        let Ok(id) = i64_from_u64(id.0, "nodes.id") else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT n.id, n.kind, n.status, n.last_seen_at, n.heartbeat_ttl_seconds, \
             n.auth_token_hash, n.active_incarnation_id, \
             ai.node_id AS active_incarnation_node_id, \
             ai.status AS active_incarnation_status, \
             (SELECT COUNT(*) FROM node_incarnations active \
              WHERE active.node_id = n.id AND active.status = 'active') \
                 AS active_incarnation_count \
             FROM nodes n \
             LEFT JOIN node_incarnations ai ON ai.incarnation_id = n.active_incarnation_id \
             WHERE n.id = ?",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("nodes auth record", e))?;
        row.as_ref().map(row_to_auth_record).transpose()
    }

    /// Read a node and its validated active-incarnation context in this transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when the node or its incarnation pointer is corrupt.
    pub async fn active_context_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: NodeId,
    ) -> Result<Option<Node>, VoomError> {
        get_in_tx(tx, id).await
    }

    /// Require one node's validated active-incarnation pointer to match a request fence.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the node is absent,
    /// [`VoomError::Conflict`] for an inactive or mismatched incarnation, and
    /// [`VoomError::Database`] when the persisted pointer is corrupt.
    pub async fn require_active_incarnation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        incarnation_id: NodeIncarnationId,
    ) -> Result<Node, VoomError> {
        let node = get_in_tx(tx, node_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("node {node_id}")))?;
        if node.active_incarnation_id != Some(incarnation_id) {
            return Err(VoomError::Conflict(format!(
                "node {node_id} active incarnation does not match {incarnation_id}"
            )));
        }
        Ok(node)
    }

    /// Point a non-retired logical node at a newly inserted active incarnation.
    ///
    /// `expected_current` is checked without decoding the temporarily incoherent
    /// pointer created while the caller ends the predecessor and inserts the
    /// replacement in the same transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when the pointer or lifecycle changed and
    /// database/internal errors for storage failures or an unreadable result.
    pub async fn activate_incarnation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        expected_current: Option<NodeIncarnationId>,
        incarnation_id: NodeIncarnationId,
        now: OffsetDateTime,
    ) -> Result<Node, VoomError> {
        let now = iso8601(now)?;
        let result = sqlx::query(
            "UPDATE nodes SET status = 'active', last_seen_at = ?, retired_at = NULL, \
             active_incarnation_id = ?, epoch = epoch + 1 \
             WHERE id = ? AND active_incarnation_id IS ? AND status != 'retired'",
        )
        .bind(now)
        .bind(incarnation_id.to_string())
        .bind(i64_from_u64(node_id.0, "nodes.id")?)
        .bind(expected_current.map(|id| id.to_string()))
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("nodes activate incarnation", error))?;
        if result.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "node {node_id} active incarnation changed during activation"
            )));
        }
        get_in_tx(tx, node_id).await?.ok_or_else(|| {
            VoomError::Internal(format!(
                "node {node_id} missing after incarnation activation"
            ))
        })
    }

    /// Clear the exact current incarnation and return a non-retired node to registered.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when the pointer or lifecycle changed and
    /// database/internal errors for storage failures or an unreadable result.
    pub async fn clear_incarnation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        incarnation_id: NodeIncarnationId,
    ) -> Result<Node, VoomError> {
        let result = sqlx::query(
            "UPDATE nodes SET status = 'registered', active_incarnation_id = NULL, \
             epoch = epoch + 1 \
             WHERE id = ? AND active_incarnation_id = ? AND status != 'retired'",
        )
        .bind(i64_from_u64(node_id.0, "nodes.id")?)
        .bind(incarnation_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("nodes clear incarnation", error))?;
        if result.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "node {node_id} active incarnation changed during deactivation"
            )));
        }
        get_in_tx(tx, node_id).await?.ok_or_else(|| {
            VoomError::Internal(format!("node {node_id} missing after incarnation clear"))
        })
    }

    /// Record a heartbeat and activate the node in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the node does not exist,
    /// [`VoomError::Conflict`] when it is retired or changes concurrently,
    /// [`VoomError::Database`] for storage failures, and [`VoomError::Internal`]
    /// if the updated row cannot be encoded or re-read.
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
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
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

    /// Read validated non-terminal node snapshots in deterministic stale-check order.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when the query or a persisted node/incarnation
    /// projection is corrupt.
    pub async fn stale_candidates_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<Vec<Node>, VoomError> {
        let rows = sqlx::query(
            "SELECT n.id, n.name, n.kind, n.status, n.registered_at, n.last_seen_at, \
             n.retired_at, n.heartbeat_ttl_seconds, n.auth_token_hint, n.metadata, n.epoch, \
             n.active_incarnation_id, \
             ai.node_id AS active_incarnation_node_id, \
             ai.status AS active_incarnation_status, \
             (SELECT COUNT(*) FROM node_incarnations active \
              WHERE active.node_id = n.id AND active.status = 'active') \
                 AS active_incarnation_count \
             FROM nodes n \
             LEFT JOIN node_incarnations ai ON ai.incarnation_id = n.active_incarnation_id \
             WHERE n.status IN ('registered','active') \
             ORDER BY n.last_seen_at ASC, n.id ASC",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("nodes stale candidates", e))?;
        rows.iter().map(row_to_node).collect()
    }

    /// Mark one validated candidate stale when its snapshot is still current.
    ///
    /// # Errors
    ///
    /// Returns database/internal errors from the guarded transition and reload.
    pub async fn mark_stale_candidate_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node: &Node,
        now: OffsetDateTime,
    ) -> Result<Option<Node>, VoomError> {
        mark_stale_candidate_in_tx(tx, node, now).await
    }

    /// Mark an expired snapshot stale after its active incarnation was ended in this transaction.
    ///
    /// This typed transition clears the now-terminal pointer without decoding the deliberately
    /// short-lived intermediate state between incarnation end and node transition.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when the snapshot changed, plus database/internal errors.
    pub async fn mark_stale_after_incarnation_end_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node: &Node,
        incarnation_id: NodeIncarnationId,
        now: OffsetDateTime,
    ) -> Result<Option<Node>, VoomError> {
        let expires_at =
            node.last_seen_at + Duration::seconds(i64::from(node.heartbeat_ttl_seconds));
        if expires_at > now {
            return Ok(None);
        }
        if node.active_incarnation_id != Some(incarnation_id) {
            return Err(VoomError::Conflict(format!(
                "node {} active incarnation changed before stale transition",
                node.id
            )));
        }
        let result = sqlx::query(
            "UPDATE nodes SET status = 'stale', active_incarnation_id = NULL, epoch = epoch + 1 \
             WHERE id = ? AND status IN ('registered','active') AND epoch = ? \
               AND active_incarnation_id = ?",
        )
        .bind(i64_from_u64(node.id.0, "nodes.id")?)
        .bind(i64_from_u64(node.epoch, "nodes.epoch")?)
        .bind(incarnation_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("nodes stale ended incarnation", error))?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        get_in_tx(tx, node.id).await?.map(Some).ok_or_else(|| {
            VoomError::Internal(format!("node {} missing after stale transition", node.id))
        })
    }

    /// Retire a node when its epoch still matches the caller's observation.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the node does not exist,
    /// [`VoomError::Conflict`] when it is already retired or its epoch changes,
    /// [`VoomError::Database`] for storage failures, and [`VoomError::Internal`]
    /// if the updated row cannot be encoded or re-read.
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
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .bind(i64_from_u64(
            expected_epoch,
            concat!(module_path!(), ": ", stringify!(expected_epoch)),
        )?)
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

    /// Retire a node and clear the exact incarnation already ended by the caller.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Conflict`] when the epoch, pointer, or lifecycle changed,
    /// plus database/internal errors for the guarded update and reload.
    pub async fn retire_after_incarnation_end_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: NodeId,
        expected_epoch: u64,
        incarnation_id: NodeIncarnationId,
        now: OffsetDateTime,
    ) -> Result<Node, VoomError> {
        let now = iso8601(now)?;
        let result = sqlx::query(
            "UPDATE nodes SET status = 'retired', retired_at = ?, last_seen_at = ?, \
             active_incarnation_id = NULL, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ? AND active_incarnation_id = ? AND status != 'retired'",
        )
        .bind(&now)
        .bind(&now)
        .bind(i64_from_u64(id.0, "nodes.id")?)
        .bind(i64_from_u64(expected_epoch, "nodes.epoch")?)
        .bind(incarnation_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("nodes retire ended incarnation", error))?;
        if result.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "nodes retire rejected: id={id} expected epoch/pointer changed"
            )));
        }
        get_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!(
                "node {id} missing after incarnation-aware retirement"
            ))
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
    // `active_incarnation_id IS NULL` keeps this transition off any node that still owns an
    // incarnation: those must go through end_incarnation_in_tx first, or the node would read
    // stale while its incarnation stayed active and its workers stayed live.
    let res = sqlx::query(
        "UPDATE nodes \
         SET status = 'stale', epoch = epoch + 1 \
         WHERE id = ? \
           AND status IN ('registered','active') \
           AND active_incarnation_id IS NULL \
           AND last_seen_at = ? \
           AND heartbeat_ttl_seconds = ? \
           AND epoch = ?",
    )
    .bind(i64_from_u64(
        node.id.0,
        concat!(module_path!(), ": ", stringify!(node.id.0)),
    )?)
    .bind(last_seen_at)
    .bind(i64::from(node.heartbeat_ttl_seconds))
    .bind(i64_from_u64(
        node.epoch,
        concat!(module_path!(), ": ", stringify!(node.epoch)),
    )?)
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
        "SELECT n.id, n.name, n.kind, n.status, n.registered_at, n.last_seen_at, \
         n.retired_at, n.heartbeat_ttl_seconds, n.auth_token_hint, n.metadata, n.epoch, \
         n.active_incarnation_id, \
         ai.node_id AS active_incarnation_node_id, \
         ai.status AS active_incarnation_status, \
         (SELECT COUNT(*) FROM node_incarnations active \
          WHERE active.node_id = n.id AND active.status = 'active') \
             AS active_incarnation_count \
         FROM nodes n \
         LEFT JOIN node_incarnations ai ON ai.incarnation_id = n.active_incarnation_id \
         WHERE n.id = ?",
    )
    .bind(i64_from_u64(
        id.0,
        concat!(module_path!(), ": ", stringify!(id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("nodes reload", e))?;
    row.as_ref().map(row_to_node).transpose()
}

fn row_to_node(row: &sqlx::sqlite::SqliteRow) -> Result<Node, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("nodes", e))?;
    let name: String = row.try_get("name").map_err(|e| map_row_err("nodes", e))?;
    let kind: String = row.try_get("kind").map_err(|e| map_row_err("nodes", e))?;
    let status: String = row.try_get("status").map_err(|e| map_row_err("nodes", e))?;
    let registered: String = row
        .try_get("registered_at")
        .map_err(|e| map_row_err("nodes", e))?;
    let last_seen: String = row
        .try_get("last_seen_at")
        .map_err(|e| map_row_err("nodes", e))?;
    let retired: Option<String> = row
        .try_get("retired_at")
        .map_err(|e| map_row_err("nodes", e))?;
    let heartbeat_ttl_seconds: i64 = row
        .try_get("heartbeat_ttl_seconds")
        .map_err(|e| map_row_err("nodes", e))?;
    let auth_token_hint: String = row
        .try_get("auth_token_hint")
        .map_err(|e| map_row_err("nodes", e))?;
    let metadata: String = row
        .try_get("metadata")
        .map_err(|e| map_row_err("nodes", e))?;
    let epoch: i64 = row.try_get("epoch").map_err(|e| map_row_err("nodes", e))?;
    let id = NodeId(u64_from_i64(
        id,
        concat!(module_path!(), ": ", stringify!(id)),
    )?);
    let active_incarnation_id = active_incarnation_projection(row, id)?;
    Ok(Node {
        id,
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
        epoch: u64_from_i64(epoch, concat!(module_path!(), ": ", stringify!(epoch)))?,
        active_incarnation_id,
    })
}

fn row_to_auth_record(row: &sqlx::sqlite::SqliteRow) -> Result<NodeAuthRecord, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("nodes", e))?;
    let kind: String = row.try_get("kind").map_err(|e| map_row_err("nodes", e))?;
    let status: String = row.try_get("status").map_err(|e| map_row_err("nodes", e))?;
    let last_seen: String = row
        .try_get("last_seen_at")
        .map_err(|e| map_row_err("nodes", e))?;
    let heartbeat_ttl_seconds: i64 = row
        .try_get("heartbeat_ttl_seconds")
        .map_err(|e| map_row_err("nodes", e))?;
    let auth_token_hash: String = row
        .try_get("auth_token_hash")
        .map_err(|e| map_row_err("nodes", e))?;
    let id = NodeId(u64_from_i64(
        id,
        concat!(module_path!(), ": ", stringify!(id)),
    )?);
    let active_incarnation_id = active_incarnation_projection(row, id)?;
    Ok(NodeAuthRecord {
        id,
        kind: NodeKind::parse_database("nodes.kind", &kind)?,
        status: NodeStatus::parse_database("nodes.status", &status)?,
        last_seen_at: parse_iso8601(&last_seen)?,
        heartbeat_ttl_seconds: u32_from_i64(heartbeat_ttl_seconds)?,
        auth_token_hash,
        active_incarnation_id,
    })
}

fn active_incarnation_projection(
    row: &sqlx::sqlite::SqliteRow,
    node_id: NodeId,
) -> Result<Option<NodeIncarnationId>, VoomError> {
    let id: Option<String> = row
        .try_get("active_incarnation_id")
        .map_err(|error| map_row_err("nodes active incarnation id", error))?;
    let owner_id: Option<i64> = row
        .try_get("active_incarnation_node_id")
        .map_err(|error| map_row_err("nodes active incarnation owner", error))?;
    let status: Option<String> = row
        .try_get("active_incarnation_status")
        .map_err(|error| map_row_err("nodes active incarnation status", error))?;
    let active_count: i64 = row
        .try_get("active_incarnation_count")
        .map_err(|error| map_row_err("nodes active incarnation count", error))?;
    let active_count = u32_from_i64(active_count)?;

    let Some(id) = id else {
        if active_count != 0 || owner_id.is_some() || status.is_some() {
            return Err(VoomError::database(format!(
                "nodes active incarnation pointer is null but node {node_id} has \
                 {active_count} active incarnation rows"
            )));
        }
        return Ok(None);
    };
    let id = NodeIncarnationId::parse_database("nodes active incarnation id", &id)?;
    let owner_id = owner_id
        .ok_or_else(|| {
            VoomError::database(format!(
                "nodes active incarnation {id} does not resolve for node {node_id}"
            ))
        })
        .and_then(|value| u64_from_i64(value, "nodes active incarnation owner"))
        .map(NodeId)?;
    if owner_id != node_id {
        return Err(VoomError::database(format!(
            "nodes active incarnation {id} is owned by node {owner_id}, not node {node_id}"
        )));
    }
    let status = status.ok_or_else(|| {
        VoomError::database(format!(
            "nodes active incarnation {id} has no readable status"
        ))
    })?;
    if status != "active" {
        return Err(VoomError::database(format!(
            "nodes active incarnation {id} for node {node_id} is not active: {status:?}"
        )));
    }
    if active_count != 1 {
        return Err(VoomError::database(format!(
            "nodes active incarnation pointer {id} for node {node_id} has \
             active row count {active_count}, expected 1"
        )));
    }
    Ok(Some(id))
}

#[cfg(test)]
#[path = "nodes_test.rs"]
mod tests;
