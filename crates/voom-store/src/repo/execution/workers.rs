//! `SqliteWorkerRepo` — owns workers + `worker_capabilities` + `worker_grants`.

use serde_json::Value as JsonValue;
use sqlx::{QueryBuilder, Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{NodeId, TicketOperation, VoomError, WorkerId};
pub use voom_core::{WorkerKind, WorkerStatus};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};
use super::nodes::{NodeKind, NodeStatus};

#[derive(Debug, Clone)]
pub struct NewWorker {
    pub name: String,
    pub kind: WorkerKind,
    pub registered_at: OffsetDateTime,
    pub node_id: Option<NodeId>,
}

#[derive(Debug, Clone)]
pub struct Worker {
    pub id: WorkerId,
    pub node_id: Option<NodeId>,
    pub name: String,
    pub kind: WorkerKind,
    pub status: WorkerStatus,
    pub registered_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
    pub epoch: u64,
}

#[derive(Debug, Clone)]
pub struct WorkerNodeContext {
    pub id: NodeId,
    pub name: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub last_seen_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct WorkerInspection {
    pub worker: Worker,
    pub node: Option<WorkerNodeContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOperationEligibility {
    pub worker_status: Option<WorkerStatus>,
    pub has_capability: bool,
    pub has_grant: bool,
    pub is_denied: bool,
    pub artifact_access: Vec<String>,
}

impl WorkerOperationEligibility {
    /// Return whether the worker may execute the operation now.
    #[must_use]
    pub const fn is_eligible(&self) -> bool {
        let is_live = match self.worker_status {
            Some(WorkerStatus::Registered | WorkerStatus::Active) => true,
            Some(WorkerStatus::Stale | WorkerStatus::Retired) | None => false,
        };
        is_live && self.has_capability && self.has_grant && !self.is_denied
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOperationCandidate {
    pub worker_id: WorkerId,
    pub active_leases: u32,
    pub max_parallel: u32,
    pub hardware: Vec<String>,
    pub capability_extra: Vec<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOperationCapability {
    pub worker_id: WorkerId,
    pub hardware: Vec<String>,
    pub extra: JsonValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeWorkerCapability {
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
    pub operation: TicketOperation,
    pub extra: JsonValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerOperationCapacity {
    pub active_leases: u32,
    pub max_parallel: u32,
}

impl WorkerOperationCapacity {
    /// Return whether another lease may be acquired for this worker operation.
    #[must_use]
    pub const fn has_capacity(self) -> bool {
        self.active_leases < self.max_parallel
    }
}

#[derive(Debug, Clone)]
pub struct NewCapability {
    pub worker_id: WorkerId,
    pub operation: TicketOperation,
    pub codecs: Vec<String>,
    pub hardware: Vec<String>,
    pub artifact_access: Vec<String>,
    pub extra: JsonValue,
}

#[derive(Debug, Clone)]
pub struct Capability {
    pub id: u64,
    pub worker_id: WorkerId,
    pub operation: TicketOperation,
}

#[derive(Debug, Clone)]
pub struct NewGrant {
    pub worker_id: WorkerId,
    pub can_execute: Vec<TicketOperation>,
    pub can_access_read: Vec<String>,
    pub can_access_write: Vec<String>,
    pub denies: Vec<TicketOperation>,
    pub max_parallel: JsonValue,
}

#[derive(Debug, Clone)]
pub struct Grant {
    pub id: u64,
    pub worker_id: WorkerId,
}

#[derive(Debug, Clone)]
pub struct SqliteWorkerRepo {
    pool: SqlitePool,
}

impl SqliteWorkerRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Return runtime metadata for live workers declaring one of the operations.
    ///
    /// A worker is live when its durable status is `registered` or `active`.
    /// Results are ordered by worker id and then capability id.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the rows, stored operation, or JSON
    /// metadata cannot be read or decoded.
    pub async fn runtime_capabilities_for_operations(
        &self,
        operations: &[TicketOperation],
    ) -> Result<Vec<RuntimeWorkerCapability>, VoomError> {
        if operations.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::new(
            "SELECT w.id AS worker_id, w.epoch AS worker_epoch, wc.operation, wc.extra \
             FROM workers w \
             JOIN worker_capabilities wc ON wc.worker_id = w.id \
             WHERE w.status IN ('registered', 'active') AND wc.operation IN (",
        );
        let mut separated = query.separated(", ");
        for operation in operations {
            separated.push_bind(operation.as_str());
        }
        separated.push_unseparated(") ORDER BY w.id ASC, wc.id ASC");
        let rows =
            query.build().fetch_all(&self.pool).await.map_err(|error| {
                VoomError::database_context("runtime worker capabilities", error)
            })?;

        let mut capabilities = Vec::with_capacity(rows.len());
        for row in rows {
            let worker_id: i64 = row
                .try_get("worker_id")
                .map_err(|error| map_row_err("runtime worker capability worker id", error))?;
            let worker_epoch: i64 = row
                .try_get("worker_epoch")
                .map_err(|error| map_row_err("runtime worker capability worker epoch", error))?;
            let operation: String = row
                .try_get("operation")
                .map_err(|error| map_row_err("runtime worker capability operation", error))?;
            let extra: String = row
                .try_get("extra")
                .map_err(|error| map_row_err("runtime worker capability extra", error))?;
            let worker_id = u64::try_from(worker_id).map_err(|_| {
                VoomError::database(format!(
                    "runtime worker capability worker id was negative: {worker_id}"
                ))
            })?;
            let worker_epoch = u64::try_from(worker_epoch).map_err(|_| {
                VoomError::database(format!(
                    "runtime worker capability worker epoch was negative: {worker_epoch}"
                ))
            })?;
            capabilities.push(RuntimeWorkerCapability {
                worker_id: WorkerId(worker_id),
                worker_epoch,
                operation: TicketOperation::from_stored(
                    operation,
                    "runtime worker capability operation",
                )?,
                extra: serde_json::from_str(&extra).map_err(|error| {
                    VoomError::database_context("parse runtime worker capability extra", error)
                })?,
            });
        }
        Ok(capabilities)
    }
}

impl Repository for SqliteWorkerRepo {}

impl SqliteWorkerRepo {
    /// Register a worker in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the row cannot be inserted, and
    /// [`VoomError::Internal`] if the registration timestamp cannot be encoded.
    pub async fn register_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewWorker,
    ) -> Result<Worker, VoomError> {
        let ts = iso8601(input.registered_at)?;
        let res = sqlx::query(
            "INSERT INTO workers (name, kind, status, registered_at, last_seen_at, node_id) \
             VALUES (?, ?, 'registered', ?, ?, ?)",
        )
        .bind(&input.name)
        .bind(input.kind.as_str())
        .bind(&ts)
        .bind(&ts)
        .bind(
            input
                .node_id
                .map(|id| i64_from_u64(id.0, concat!(module_path!(), ": ", stringify!(id.0))))
                .transpose()?,
        )
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("workers insert", e))?;
        Ok(Worker {
            id: WorkerId(u64_from_i64(
                res.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res.last_insert_rowid())),
            )?),
            node_id: input.node_id,
            name: input.name,
            kind: input.kind,
            status: WorkerStatus::Registered,
            registered_at: input.registered_at,
            last_seen_at: input.registered_at,
            retired_at: None,
            epoch: 0,
        })
    }

    /// Register a built-in worker without changing an existing identity.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the worker cannot be inserted or read,
    /// and [`VoomError::Internal`] if the timestamp cannot be encoded.
    pub async fn register_builtin_if_missing_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewWorker,
    ) -> Result<Worker, VoomError> {
        let registered_at = iso8601(input.registered_at)?;
        sqlx::query(
            "INSERT OR IGNORE INTO workers \
             (name, kind, status, registered_at, last_seen_at, node_id) \
             VALUES (?, ?, 'registered', ?, ?, ?)",
        )
        .bind(&input.name)
        .bind(input.kind.as_str())
        .bind(&registered_at)
        .bind(&registered_at)
        .bind(
            input
                .node_id
                .map(|id| i64_from_u64(id.0, concat!(module_path!(), ": ", stringify!(id.0))))
                .transpose()?,
        )
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("workers insert built-in", error))?;
        get_by_name_in_tx(tx, &input.name).await?.ok_or_else(|| {
            VoomError::Internal(format!(
                "built-in worker {} missing after insert",
                input.name
            ))
        })
    }

    /// Register and commit a worker.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::register_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn register(&self, input: NewWorker) -> Result<Worker, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.register_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Record a worker capability in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the row cannot be inserted, and
    /// [`VoomError::Internal`] if capability fields cannot be serialized.
    pub async fn record_capability_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewCapability,
    ) -> Result<Capability, VoomError> {
        let codecs = serialize_json(&input.codecs, "codecs")?;
        let hw = serialize_json(&input.hardware, "hardware")?;
        let access = serialize_json(&input.artifact_access, "artifact_access")?;
        let extra = serialize_json(&input.extra, "extra")?;
        let res = sqlx::query(
            "INSERT INTO worker_capabilities \
             (worker_id, operation, codecs, hardware, artifact_access, extra) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(
            input.worker_id.0,
            concat!(module_path!(), ": ", stringify!(input.worker_id.0)),
        )?)
        .bind(input.operation.as_str())
        .bind(codecs)
        .bind(hw)
        .bind(access)
        .bind(extra)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("worker_capabilities insert", e))?;
        Ok(Capability {
            id: u64_from_i64(
                res.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res.last_insert_rowid())),
            )?,
            worker_id: input.worker_id,
            operation: input.operation,
        })
    }

    /// Record and commit a worker capability.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::record_capability_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn record_capability(&self, input: NewCapability) -> Result<Capability, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.record_capability_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Record worker grants in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the row cannot be inserted, and
    /// [`VoomError::Internal`] if grant fields cannot be serialized.
    pub async fn record_grant_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewGrant,
    ) -> Result<Grant, VoomError> {
        let ce = serialize_json(&input.can_execute, "can_execute")?;
        let cr = serialize_json(&input.can_access_read, "can_access_read")?;
        let cw = serialize_json(&input.can_access_write, "can_access_write")?;
        let d = serialize_json(&input.denies, "denies")?;
        let mp = serialize_json(&input.max_parallel, "max_parallel")?;
        let res = sqlx::query(
            "INSERT INTO worker_grants \
             (worker_id, can_execute, can_access_read, can_access_write, denies, max_parallel) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(
            input.worker_id.0,
            concat!(module_path!(), ": ", stringify!(input.worker_id.0)),
        )?)
        .bind(ce)
        .bind(cr)
        .bind(cw)
        .bind(d)
        .bind(mp)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("worker_grants insert", e))?;
        Ok(Grant {
            id: u64_from_i64(
                res.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res.last_insert_rowid())),
            )?,
            worker_id: input.worker_id,
        })
    }

    /// Record and commit worker grants.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::record_grant_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn record_grant(&self, input: NewGrant) -> Result<Grant, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.record_grant_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Retire a worker when its epoch still matches the caller's observation.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the worker does not exist,
    /// [`VoomError::Conflict`] when it is retired or its epoch changes,
    /// [`VoomError::Database`] for storage failures, and [`VoomError::Internal`]
    /// if the updated row cannot be encoded or re-read.
    pub async fn retire_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: WorkerId,
        expected_epoch: u64,
        now: OffsetDateTime,
    ) -> Result<Worker, VoomError> {
        let current = get_in_tx(tx, id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("workers retire: id={id} not found")))?;
        if current.status == WorkerStatus::Retired {
            return Err(VoomError::Conflict(format!(
                "workers retire rejected: id={id} already retired"
            )));
        }
        if current.epoch != expected_epoch {
            return Err(VoomError::Conflict(format!(
                "workers retire rejected: id={id} expected_epoch={expected_epoch} \
                 actual_epoch={}",
                current.epoch
            )));
        }
        let ts = iso8601(now)?;
        sqlx::query("DELETE FROM accelerator_claims WHERE worker_id = ?")
            .bind(i64_from_u64(
                id.0,
                concat!(module_path!(), ": ", stringify!(id.0)),
            )?)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("accelerator_claims release", e))?;
        let res = sqlx::query(
            "UPDATE workers \
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
        .map_err(|e| VoomError::database_context("workers update", e))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "workers retire rejected: id={id} expected_epoch={expected_epoch} \
                 changed during update"
            )));
        }
        // Re-read inside the same transaction so the caller sees the updated
        // row. A pool-side `get` would query a different connection and miss
        // the uncommitted write.
        get_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("workers retire: row vanished post-update id={id}"))
        })
    }

    /// Retire and commit a worker at an expected epoch.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::retire_in_tx`], or
    /// [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn retire(
        &self,
        id: WorkerId,
        expected_epoch: u64,
        now: OffsetDateTime,
    ) -> Result<Worker, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.retire_in_tx(&mut tx, id, expected_epoch, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Look up a worker by id.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn get(&self, id: WorkerId) -> Result<Option<Worker>, VoomError> {
        let row = sqlx::query(
            "SELECT id, node_id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
             FROM workers WHERE id = ?",
        )
        .bind(i64_from_u64(id.0, concat!(module_path!(), ": ", stringify!(id.0)))?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workers get", e))?;
        row.as_ref().map(row_to_worker).transpose()
    }

    /// Look up a worker by name in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn get_by_name_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        name: &str,
    ) -> Result<Option<Worker>, VoomError> {
        get_by_name_in_tx(tx, name).await
    }

    /// Look up and commit a worker-by-name read.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the transaction, query, or row
    /// decoding fails.
    pub async fn get_by_name(&self, name: &str) -> Result<Option<Worker>, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.get_by_name_in_tx(&mut tx, name).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// List live workers in an exact legacy-name or incarnation namespace.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn list_live_by_name_namespace_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        legacy_name: &str,
        incarnation_prefix: &str,
    ) -> Result<Vec<Worker>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, node_id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
             FROM workers \
             WHERE (name = ? OR substr(name, 1, length(?)) = ?) \
               AND status IN ('registered', 'active') \
             ORDER BY registered_at ASC, id ASC",
        )
        .bind(legacy_name)
        .bind(incarnation_prefix)
        .bind(incarnation_prefix)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("workers list live name namespace", e))?;
        rows.iter().map(row_to_worker).collect()
    }

    /// List every worker in an exact legacy-name or incarnation-prefix namespace.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when the query or row decoding fails.
    pub async fn list_by_name_namespace(
        &self,
        legacy_name: &str,
        incarnation_prefix: &str,
    ) -> Result<Vec<Worker>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, node_id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
             FROM workers \
             WHERE name = ? OR substr(name, 1, length(?)) = ? \
             ORDER BY registered_at ASC, id ASC",
        )
        .bind(legacy_name)
        .bind(incarnation_prefix)
        .bind(incarnation_prefix)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workers list name namespace", e))?;
        rows.iter().map(row_to_worker).collect()
    }

    /// Read a worker together with its optional node context.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails,
    /// including when a worker references a missing node.
    pub async fn get_inspection(
        &self,
        id: WorkerId,
    ) -> Result<Option<WorkerInspection>, VoomError> {
        let row = sqlx::query(
            "SELECT w.id, w.node_id, w.name, w.kind, w.status, w.registered_at, \
             w.last_seen_at, w.retired_at, w.epoch, \
             n.id AS node_context_id, n.name AS node_context_name, \
             n.kind AS node_context_kind, n.status AS node_context_status, \
             n.last_seen_at AS node_context_last_seen_at \
             FROM workers w LEFT JOIN nodes n ON n.id = w.node_id \
             WHERE w.id = ?",
        )
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workers inspection get", e))?;
        row.as_ref().map(row_to_inspection).transpose()
    }

    /// List workers in one status.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails.
    pub async fn list_by_status(
        &self,
        status: WorkerStatus,
        limit: u32,
    ) -> Result<Vec<Worker>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, node_id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
             FROM workers WHERE status = ? \
             ORDER BY registered_at ASC, id ASC LIMIT ?",
        )
        .bind(status.as_str())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workers list", e))?;
        rows.iter().map(row_to_worker).collect()
    }

    /// List workers and their optional node context.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if the query or row decoding fails,
    /// including when a worker references a missing node.
    pub async fn list_inspections(
        &self,
        status: Option<WorkerStatus>,
        limit: u32,
    ) -> Result<Vec<WorkerInspection>, VoomError> {
        let status = status.map(WorkerStatus::as_str);
        let rows = sqlx::query(
            "SELECT w.id, w.node_id, w.name, w.kind, w.status, w.registered_at, \
             w.last_seen_at, w.retired_at, w.epoch, \
             n.id AS node_context_id, n.name AS node_context_name, \
             n.kind AS node_context_kind, n.status AS node_context_status, \
             n.last_seen_at AS node_context_last_seen_at \
             FROM workers w LEFT JOIN nodes n ON n.id = w.node_id \
             WHERE (? IS NULL OR w.status = ?) \
             ORDER BY w.registered_at ASC, w.id ASC LIMIT ?",
        )
        .bind(status)
        .bind(status)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workers inspection list", e))?;
        rows.iter().map(row_to_inspection).collect()
    }

    /// Evaluate whether a worker may execute an operation in this transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if worker, capability, or grant rows
    /// cannot be queried or decoded.
    pub async fn operation_eligibility_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        worker_id: WorkerId,
        operation: &TicketOperation,
    ) -> Result<WorkerOperationEligibility, VoomError> {
        let worker_status = get_in_tx(tx, worker_id).await?.map(|worker| worker.status);
        let capability_rows = sqlx::query(
            "SELECT artifact_access FROM worker_capabilities \
             WHERE worker_id = ? AND operation = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(
            worker_id.0,
            concat!(module_path!(), ": ", stringify!(worker_id.0)),
        )?)
        .bind(operation.as_str())
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("worker_capabilities eligibility", e))?;

        let mut artifact_access = Vec::new();
        for row in &capability_rows {
            let access: String = row
                .try_get("artifact_access")
                .map_err(|e| map_row_err("worker_capabilities eligibility", e))?;
            artifact_access.extend(parse_string_array_json(&access, "artifact_access")?);
        }

        let grant_rows = sqlx::query(
            "SELECT can_execute, denies FROM worker_grants WHERE worker_id = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(
            worker_id.0,
            concat!(module_path!(), ": ", stringify!(worker_id.0)),
        )?)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("worker_grants eligibility", e))?;

        let mut has_grant = false;
        let mut is_denied = false;
        for row in &grant_rows {
            let can_execute: String = row
                .try_get("can_execute")
                .map_err(|e| map_row_err("worker_grants eligibility", e))?;
            let denies: String = row
                .try_get("denies")
                .map_err(|e| map_row_err("worker_grants eligibility", e))?;
            has_grant |= parse_operation_array_json(&can_execute, "can_execute")?
                .iter()
                .any(|item| item == operation);
            is_denied |= parse_operation_array_json(&denies, "denies")?
                .iter()
                .any(|item| item == operation);
        }

        Ok(WorkerOperationEligibility {
            worker_status,
            has_capability: !capability_rows.is_empty(),
            has_grant,
            is_denied,
            artifact_access,
        })
    }

    /// Evaluate whether a worker may execute an operation.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::operation_eligibility_in_tx`],
    /// or [`VoomError::Database`] if the transaction cannot begin or commit.
    pub async fn operation_eligibility(
        &self,
        worker_id: WorkerId,
        operation: &TicketOperation,
    ) -> Result<WorkerOperationEligibility, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self
            .operation_eligibility_in_tx(&mut tx, worker_id, operation)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Return this worker's candidate operations from capabilities and grants.
    ///
    /// Denials override either source. The decoded result is sorted and unique.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if durable rows cannot be queried and
    /// [`VoomError::Database`] when an operation is outside the stored vocabulary.
    pub async fn candidate_operations_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        worker_id: WorkerId,
    ) -> Result<Vec<TicketOperation>, VoomError> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT operation FROM worker_capabilities WHERE worker_id = ? \
             UNION \
             SELECT grants.value AS operation \
             FROM worker_grants, json_each(worker_grants.can_execute) AS grants \
             WHERE worker_grants.worker_id = ? \
             EXCEPT \
             SELECT denied.value AS operation \
             FROM worker_grants, json_each(worker_grants.denies) AS denied \
             WHERE worker_grants.worker_id = ?",
        )
        .bind(i64_from_u64(
            worker_id.0,
            concat!(module_path!(), ": ", stringify!(worker_id.0)),
        )?)
        .bind(i64_from_u64(
            worker_id.0,
            concat!(module_path!(), ": ", stringify!(worker_id.0)),
        )?)
        .bind(i64_from_u64(
            worker_id.0,
            concat!(module_path!(), ": ", stringify!(worker_id.0)),
        )?)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("worker candidate operations", e))?;
        let mut operations = rows
            .into_iter()
            .map(|operation| {
                TicketOperation::from_stored(operation, "worker candidate operations.operation")
            })
            .collect::<Result<Vec<_>, _>>()?;
        operations.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        operations.dedup();
        Ok(operations)
    }

    /// Return a worker only if its lifecycle state permits new work.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] for a missing worker, [`VoomError::Conflict`]
    /// for stale or retired workers, and [`VoomError::Database`] if it cannot be read.
    pub async fn require_live_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        worker_id: WorkerId,
    ) -> Result<Worker, VoomError> {
        let worker = get_in_tx(tx, worker_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("worker {worker_id}")))?;
        match worker.status {
            WorkerStatus::Registered | WorkerStatus::Active => Ok(worker),
            WorkerStatus::Stale | WorkerStatus::Retired => Err(VoomError::Conflict(format!(
                "worker {worker_id} is {:?}, not live",
                worker.status
            ))),
        }
    }

    /// Read the effective held-lease count and grant limit for one worker
    /// operation in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if operation, capability, grant, or lease
    /// state cannot be queried or decoded, and [`VoomError::Config`] when an
    /// accelerator capability omits its required hardware token.
    pub async fn operation_capacity_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        worker_id: WorkerId,
        operation: &TicketOperation,
    ) -> Result<WorkerOperationCapacity, VoomError> {
        let operation = normalized_worker_operation(operation)?;
        if operation.as_str() == "transcode_video"
            && let Some(capacity) = accelerator_operation_capacity(tx, worker_id).await?
        {
            return Ok(capacity);
        }
        let workflow_operation = format!("{WORKFLOW_OPERATION_PREFIX}{}", operation.as_str());
        let active_leases = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) \
             FROM leases \
             JOIN tickets ON tickets.id = leases.ticket_id \
             WHERE leases.state = 'held' AND leases.worker_id = ? \
               AND (tickets.kind = ? OR tickets.kind = ?)",
        )
        .bind(i64_from_u64(
            worker_id.0,
            concat!(module_path!(), ": ", stringify!(worker_id.0)),
        )?)
        .bind(operation.as_str())
        .bind(workflow_operation)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("worker operation active lease count", e))?;
        Ok(WorkerOperationCapacity {
            active_leases: u32_from_i64(active_leases)?,
            max_parallel: max_parallel_in_tx(tx, worker_id, &operation).await?,
        })
    }

    /// List workers whose effective capability and grants allow an operation.
    ///
    /// Each worker appears at most once. The result is observational; lease
    /// acquisition must recheck eligibility in its write transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when worker, lease, capability, or grant
    /// rows cannot be read or decoded, and [`VoomError::Config`] when an
    /// accelerator capability omits its required hardware token.
    pub async fn operation_candidates(
        &self,
        operation: &TicketOperation,
    ) -> Result<Vec<WorkerOperationCandidate>, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.operation_candidates_in_tx(&mut tx, operation).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    /// Lists every durable capability declaration for an operation, including
    /// declarations owned by stale or retired workers.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] if capability rows cannot be queried or
    /// decoded.
    pub async fn operation_capability_history(
        &self,
        operation: &TicketOperation,
    ) -> Result<Vec<WorkerOperationCapability>, VoomError> {
        let operation = normalized_worker_operation(operation)?;
        let rows = sqlx::query(
            "SELECT worker_id, hardware, extra FROM worker_capabilities \
             WHERE operation = ? ORDER BY id ASC",
        )
        .bind(operation.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("worker capability history", error))?;
        let mut capabilities = Vec::with_capacity(rows.len());
        for row in rows {
            let worker_id: i64 = row
                .try_get("worker_id")
                .map_err(|error| map_row_err("worker capability history worker", error))?;
            let hardware: String = row
                .try_get("hardware")
                .map_err(|error| map_row_err("worker capability history hardware", error))?;
            let extra: String = row
                .try_get("extra")
                .map_err(|error| map_row_err("worker capability history extra", error))?;
            capabilities.push(WorkerOperationCapability {
                worker_id: WorkerId(u64_from_i64(
                    worker_id,
                    concat!(module_path!(), ": ", stringify!(worker_id)),
                )?),
                hardware: parse_string_array_json(&hardware, "hardware")?,
                extra: serde_json::from_str(&extra).map_err(|error| {
                    VoomError::database_context("parse worker capability history extra", error)
                })?,
            });
        }
        Ok(capabilities)
    }

    async fn operation_candidates_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        operation: &TicketOperation,
    ) -> Result<Vec<WorkerOperationCandidate>, VoomError> {
        let rows = sqlx::query("SELECT id AS worker_id FROM workers ORDER BY id ASC")
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("worker operation candidates", e))?;
        let mut candidates = Vec::new();
        for row in rows {
            let worker_id = worker_candidate_id(&row)?;
            let eligibility = self
                .operation_eligibility_in_tx(tx, worker_id, operation)
                .await?;
            if !eligibility.is_eligible() {
                continue;
            }
            let capacity = self
                .operation_capacity_in_tx(tx, worker_id, operation)
                .await?;
            let (hardware, capability_extra) =
                operation_capability_details_in_tx(tx, worker_id, operation).await?;
            candidates.push(WorkerOperationCandidate {
                worker_id,
                active_leases: capacity.active_leases,
                max_parallel: capacity.max_parallel,
                hardware,
                capability_extra,
            });
        }
        Ok(candidates)
    }

    /// Return a non-retired worker owned by the supplied node.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::NotFound`] when the worker does not exist,
    /// [`VoomError::Conflict`] when another node owns it or it is retired, and
    /// [`VoomError::Database`] if the row cannot be queried or decoded.
    pub async fn node_owned_worker_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        worker_id: WorkerId,
        node_id: NodeId,
    ) -> Result<Worker, VoomError> {
        let worker = get_in_tx(tx, worker_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("worker {worker_id}")))?;
        if worker.node_id != Some(node_id) {
            return Err(VoomError::Conflict(format!(
                "worker {worker_id} is not owned by node {node_id}"
            )));
        }
        if worker.status == WorkerStatus::Retired {
            return Err(VoomError::Conflict(format!(
                "worker {worker_id} is retired"
            )));
        }
        Ok(worker)
    }
}

/// Per-device `transcode_video` capacity for an accelerator-bound worker.
///
/// The device is keyed off the capability's own `hardware` token rather than a
/// token repeated inside `extra.accelerator`. Only the NVIDIA descriptor carries a
/// `hardware_token` field; reading the key from the descriptor would silently yield
/// no capacity row for any other backend, so the bound device would never receive
/// work. `hardware` is where ADR 0049 §4 puts the stable token, and ADR 0049 §6
/// defines capacity across "every worker advertising the same stable hardware
/// token", so it is also the correct grouping key.
async fn accelerator_operation_capacity(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker_id: WorkerId,
) -> Result<Option<WorkerOperationCapacity>, VoomError> {
    let row = sqlx::query(
        "SELECT json_extract(hardware, '$[0]') AS hardware_token \
         FROM worker_capabilities \
         WHERE worker_id = ? AND operation = 'transcode_video' \
           AND json_type(extra, '$.accelerator') = 'object' \
         ORDER BY id ASC LIMIT 1",
    )
    .bind(i64_from_u64(
        worker_id.0,
        concat!(module_path!(), ": ", stringify!(worker_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("accelerator capacity descriptor", error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let hardware_token: Option<String> = row
        .try_get("hardware_token")
        .map_err(|error| map_row_err("accelerator hardware token", error))?;
    let hardware_token = hardware_token.ok_or_else(|| {
        VoomError::Config(format!(
            "worker {worker_id} advertises a transcode_video accelerator descriptor but no \
             hardware token; the capability must carry the device token so per-device capacity \
             can be counted"
        ))
    })?;
    let max_parallel = sqlx::query_scalar::<_, i64>(
        "SELECT MIN(CAST(json_extract(extra, '$.accelerator.max_sessions') AS INTEGER)) \
         FROM worker_capabilities \
         JOIN workers ON workers.id = worker_capabilities.worker_id \
         WHERE operation = 'transcode_video' AND workers.status != 'retired' \
           AND json_extract(hardware, '$[0]') = ?",
    )
    .bind(&hardware_token)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("accelerator capacity limit", error))?;
    let active_leases = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT leases.id) FROM leases \
         JOIN tickets ON tickets.id = leases.ticket_id \
         JOIN worker_capabilities ON worker_capabilities.worker_id = leases.worker_id \
         WHERE leases.state = 'held' \
           AND (tickets.kind = 'transcode_video' \
                OR tickets.kind = 'synthetic.workflow.operation.transcode_video') \
           AND worker_capabilities.operation = 'transcode_video' \
           AND json_extract(worker_capabilities.hardware, '$[0]') = ?",
    )
    .bind(hardware_token)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("accelerator active lease count", error))?;
    Ok(Some(WorkerOperationCapacity {
        active_leases: u32_from_i64(active_leases)?,
        max_parallel: u32_from_i64(max_parallel)?,
    }))
}

async fn operation_capability_details_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker_id: WorkerId,
    operation: &TicketOperation,
) -> Result<(Vec<String>, Vec<JsonValue>), VoomError> {
    let operation = normalized_worker_operation(operation)?;
    let rows = sqlx::query(
        "SELECT hardware, extra FROM worker_capabilities \
         WHERE worker_id = ? AND operation = ? ORDER BY id ASC",
    )
    .bind(i64_from_u64(
        worker_id.0,
        concat!(module_path!(), ": ", stringify!(worker_id.0)),
    )?)
    .bind(operation.as_str())
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("worker capability details", error))?;
    let mut hardware = Vec::new();
    let mut extra = Vec::with_capacity(rows.len());
    for row in rows {
        let encoded_hardware: String = row
            .try_get("hardware")
            .map_err(|error| map_row_err("worker capability hardware", error))?;
        hardware.extend(parse_string_array_json(&encoded_hardware, "hardware")?);
        let encoded_extra: String = row
            .try_get("extra")
            .map_err(|error| map_row_err("worker capability extra", error))?;
        extra.push(serde_json::from_str(&encoded_extra).map_err(|error| {
            VoomError::database_context("parse worker capability extra", error)
        })?);
    }
    Ok((hardware, extra))
}

fn worker_candidate_id(row: &sqlx::sqlite::SqliteRow) -> Result<WorkerId, VoomError> {
    let worker_id: i64 = row
        .try_get("worker_id")
        .map_err(|e| map_row_err("worker operation candidates", e))?;
    Ok(WorkerId(u64_from_i64(
        worker_id,
        concat!(module_path!(), ": ", stringify!(worker_id)),
    )?))
}

async fn max_parallel_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker_id: WorkerId,
    operation: &TicketOperation,
) -> Result<u32, VoomError> {
    let rows =
        sqlx::query("SELECT max_parallel FROM worker_grants WHERE worker_id = ? ORDER BY id")
            .bind(i64_from_u64(
                worker_id.0,
                concat!(module_path!(), ": ", stringify!(worker_id.0)),
            )?)
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("worker max_parallel read", e))?;
    let mut operation_limit = None;
    let mut wildcard_limit = None;
    for row in rows {
        let raw: String = row
            .try_get("max_parallel")
            .map_err(|e| map_row_err("worker max_parallel", e))?;
        let value: JsonValue = serde_json::from_str(&raw)
            .map_err(|e| VoomError::database_context("parse worker max_parallel", e))?;
        operation_limit = max_limit(
            operation_limit,
            positive_u32(value.get(operation.as_str()), "max_parallel operation")?,
        );
        wildcard_limit = max_limit(
            wildcard_limit,
            positive_u32(value.get("*"), "max_parallel wildcard")?,
        );
    }
    Ok(operation_limit.or(wildcard_limit).unwrap_or(1))
}

const WORKFLOW_OPERATION_PREFIX: &str = "synthetic.workflow.operation.";

pub(super) fn normalized_worker_operation(
    operation: &TicketOperation,
) -> Result<TicketOperation, VoomError> {
    let Some(operation) = operation.as_str().strip_prefix(WORKFLOW_OPERATION_PREFIX) else {
        return Ok(operation.clone());
    };
    TicketOperation::from_stored(operation, "worker capacity operation")
}

fn max_limit(current: Option<u32>, candidate: Option<u32>) -> Option<u32> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (Some(current), None) => Some(current),
        (None, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

fn positive_u32(value: Option<&JsonValue>, field: &'static str) -> Result<Option<u32>, VoomError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(limit) = value.as_u64() else {
        return Err(VoomError::database(format!("{field} must be an integer")));
    };
    if limit == 0 {
        return Err(VoomError::database(format!("{field} must be positive")));
    }
    u32::try_from(limit)
        .map(Some)
        .map_err(|_| VoomError::database(format!("{field} does not fit u32")))
}

async fn get_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: WorkerId,
) -> Result<Option<Worker>, VoomError> {
    let row = sqlx::query(
        "SELECT id, node_id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
         FROM workers WHERE id = ?",
    )
    .bind(i64_from_u64(
        id.0,
        concat!(module_path!(), ": ", stringify!(id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("workers reload", e))?;
    row.as_ref().map(row_to_worker).transpose()
}

async fn get_by_name_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    name: &str,
) -> Result<Option<Worker>, VoomError> {
    let row = sqlx::query(
        "SELECT id, node_id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
         FROM workers WHERE name = ?",
    )
    .bind(name)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("workers get by name", e))?;
    row.as_ref().map(row_to_worker).transpose()
}

fn row_to_worker(row: &sqlx::sqlite::SqliteRow) -> Result<Worker, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("workers", e))?;
    let node_id: Option<i64> = row
        .try_get("node_id")
        .map_err(|e| map_row_err("workers", e))?;
    let name: String = row.try_get("name").map_err(|e| map_row_err("workers", e))?;
    let kind: String = row.try_get("kind").map_err(|e| map_row_err("workers", e))?;
    let status: String = row
        .try_get("status")
        .map_err(|e| map_row_err("workers", e))?;
    let registered: String = row
        .try_get("registered_at")
        .map_err(|e| map_row_err("workers", e))?;
    let last_seen: String = row
        .try_get("last_seen_at")
        .map_err(|e| map_row_err("workers", e))?;
    let retired: Option<String> = row
        .try_get("retired_at")
        .map_err(|e| map_row_err("workers", e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("workers", e))?;
    Ok(Worker {
        id: WorkerId(u64_from_i64(
            id,
            concat!(module_path!(), ": ", stringify!(id)),
        )?),
        node_id: node_id
            .map(|id| u64_from_i64(id, concat!(module_path!(), ": ", stringify!(id))).map(NodeId))
            .transpose()?,
        name,
        kind: WorkerKind::parse_database("workers.kind", &kind)?,
        status: WorkerStatus::parse_database("workers.status", &status)?,
        registered_at: parse_iso8601(&registered)?,
        last_seen_at: parse_iso8601(&last_seen)?,
        retired_at: retired.map(|s| parse_iso8601(&s)).transpose()?,
        epoch: u64_from_i64(epoch, concat!(module_path!(), ": ", stringify!(epoch)))?,
    })
}

fn row_to_inspection(row: &sqlx::sqlite::SqliteRow) -> Result<WorkerInspection, VoomError> {
    let worker = row_to_worker(row)?;
    let node_id: Option<i64> = row
        .try_get("node_context_id")
        .map_err(|e| map_row_err("workers inspection", e))?;
    if let (Some(worker_node_id), None) = (worker.node_id, node_id) {
        return Err(VoomError::database(format!(
            "workers inspection missing node context: worker_id={} node_id={}",
            worker.id, worker_node_id
        )));
    }
    let node = node_id
        .map(|id| {
            let name: String = row
                .try_get("node_context_name")
                .map_err(|e| map_row_err("workers inspection", e))?;
            let kind: String = row
                .try_get("node_context_kind")
                .map_err(|e| map_row_err("workers inspection", e))?;
            let status: String = row
                .try_get("node_context_status")
                .map_err(|e| map_row_err("workers inspection", e))?;
            let last_seen: String = row
                .try_get("node_context_last_seen_at")
                .map_err(|e| map_row_err("workers inspection", e))?;
            Ok(WorkerNodeContext {
                id: NodeId(u64_from_i64(
                    id,
                    concat!(module_path!(), ": ", stringify!(id)),
                )?),
                name,
                kind: NodeKind::parse_database("nodes.kind", &kind)?,
                status: NodeStatus::parse_database("nodes.status", &status)?,
                last_seen_at: parse_iso8601(&last_seen)?,
            })
        })
        .transpose()?;
    Ok(WorkerInspection { worker, node })
}

fn parse_string_array_json(input: &str, field: &'static str) -> Result<Vec<String>, VoomError> {
    serde_json::from_str(input)
        .map_err(|e| VoomError::database_context(format!("parse worker {field}"), e))
}

fn parse_operation_array_json(
    input: &str,
    field: &'static str,
) -> Result<Vec<TicketOperation>, VoomError> {
    let raw = parse_string_array_json(input, field)?;
    raw.into_iter()
        .map(|operation| TicketOperation::from_stored(operation, field))
        .collect()
}

#[cfg(test)]
#[path = "workers_test.rs"]
mod tests;
