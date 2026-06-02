//! `SqliteWorkerRepo` — owns workers + `worker_capabilities` + `worker_grants`.

use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{NodeId, TicketOperation, VoomError, WorkerId};
pub use voom_core::{WorkerKind, WorkerStatus};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
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
    pub has_capability: bool,
    pub has_grant: bool,
    pub is_denied: bool,
    pub artifact_access: Vec<String>,
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
}

impl Repository for SqliteWorkerRepo {}

impl SqliteWorkerRepo {
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
        .bind(input.node_id.map(|id| i64_from_u64(id.0)))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("workers insert: {e}")))?;
        Ok(Worker {
            id: WorkerId(u64_from_i64(res.last_insert_rowid())),
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

    pub async fn register(&self, input: NewWorker) -> Result<Worker, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.register_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

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
        .bind(i64_from_u64(input.worker_id.0))
        .bind(input.operation.as_str())
        .bind(codecs)
        .bind(hw)
        .bind(access)
        .bind(extra)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("worker_capabilities insert: {e}")))?;
        Ok(Capability {
            id: u64_from_i64(res.last_insert_rowid()),
            worker_id: input.worker_id,
            operation: input.operation,
        })
    }

    pub async fn record_capability(&self, input: NewCapability) -> Result<Capability, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.record_capability_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

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
        .bind(i64_from_u64(input.worker_id.0))
        .bind(ce)
        .bind(cr)
        .bind(cw)
        .bind(d)
        .bind(mp)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("worker_grants insert: {e}")))?;
        Ok(Grant {
            id: u64_from_i64(res.last_insert_rowid()),
            worker_id: input.worker_id,
        })
    }

    pub async fn record_grant(&self, input: NewGrant) -> Result<Grant, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.record_grant_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

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
        let res = sqlx::query(
            "UPDATE workers \
             SET status = 'retired', retired_at = ?, last_seen_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ? AND status != 'retired'",
        )
        .bind(&ts)
        .bind(&ts)
        .bind(i64_from_u64(id.0))
        .bind(i64_from_u64(expected_epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("workers update: {e}")))?;
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
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.retire_in_tx(&mut tx, id, expected_epoch, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    pub async fn get(&self, id: WorkerId) -> Result<Option<Worker>, VoomError> {
        let row = sqlx::query(
            "SELECT id, node_id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
             FROM workers WHERE id = ?",
        )
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("workers get: {e}")))?;
        row.as_ref().map(row_to_worker).transpose()
    }

    pub async fn get_by_name_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        name: &str,
    ) -> Result<Option<Worker>, VoomError> {
        get_by_name_in_tx(tx, name).await
    }

    pub async fn get_by_name(&self, name: &str) -> Result<Option<Worker>, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.get_by_name_in_tx(&mut tx, name).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

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
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("workers inspection get: {e}")))?;
        row.as_ref().map(row_to_inspection).transpose()
    }

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
        .map_err(|e| VoomError::Database(format!("workers list: {e}")))?;
        rows.iter().map(row_to_worker).collect()
    }

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
        .map_err(|e| VoomError::Database(format!("workers inspection list: {e}")))?;
        rows.iter().map(row_to_inspection).collect()
    }

    pub async fn operation_eligibility_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        worker_id: WorkerId,
        operation: &TicketOperation,
    ) -> Result<WorkerOperationEligibility, VoomError> {
        let capability_rows = sqlx::query(
            "SELECT artifact_access FROM worker_capabilities \
             WHERE worker_id = ? AND operation = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(worker_id.0))
        .bind(operation.as_str())
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("worker_capabilities eligibility: {e}")))?;

        let mut artifact_access = Vec::new();
        for row in &capability_rows {
            let access: String = row
                .try_get("artifact_access")
                .map_err(|e| map_row_err("worker_capabilities eligibility", &e))?;
            artifact_access.extend(parse_string_array_json(&access, "artifact_access")?);
        }

        let grant_rows = sqlx::query(
            "SELECT can_execute, denies FROM worker_grants WHERE worker_id = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(worker_id.0))
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("worker_grants eligibility: {e}")))?;

        let mut has_grant = false;
        let mut is_denied = false;
        for row in &grant_rows {
            let can_execute: String = row
                .try_get("can_execute")
                .map_err(|e| map_row_err("worker_grants eligibility", &e))?;
            let denies: String = row
                .try_get("denies")
                .map_err(|e| map_row_err("worker_grants eligibility", &e))?;
            has_grant |= parse_operation_array_json(&can_execute, "can_execute")?
                .iter()
                .any(|item| item == operation);
            is_denied |= parse_operation_array_json(&denies, "denies")?
                .iter()
                .any(|item| item == operation);
        }

        Ok(WorkerOperationEligibility {
            has_capability: !capability_rows.is_empty(),
            has_grant,
            is_denied,
            artifact_access,
        })
    }

    pub async fn operation_eligibility(
        &self,
        worker_id: WorkerId,
        operation: &TicketOperation,
    ) -> Result<WorkerOperationEligibility, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self
            .operation_eligibility_in_tx(&mut tx, worker_id, operation)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

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

async fn get_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: WorkerId,
) -> Result<Option<Worker>, VoomError> {
    let row = sqlx::query(
        "SELECT id, node_id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
         FROM workers WHERE id = ?",
    )
    .bind(i64_from_u64(id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("workers reload: {e}")))?;
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
    .map_err(|e| VoomError::Database(format!("workers get by name: {e}")))?;
    row.as_ref().map(row_to_worker).transpose()
}

fn row_to_worker(row: &sqlx::sqlite::SqliteRow) -> Result<Worker, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("workers", &e))?;
    let node_id: Option<i64> = row
        .try_get("node_id")
        .map_err(|e| map_row_err("workers", &e))?;
    let name: String = row
        .try_get("name")
        .map_err(|e| map_row_err("workers", &e))?;
    let kind: String = row
        .try_get("kind")
        .map_err(|e| map_row_err("workers", &e))?;
    let status: String = row
        .try_get("status")
        .map_err(|e| map_row_err("workers", &e))?;
    let registered: String = row
        .try_get("registered_at")
        .map_err(|e| map_row_err("workers", &e))?;
    let last_seen: String = row
        .try_get("last_seen_at")
        .map_err(|e| map_row_err("workers", &e))?;
    let retired: Option<String> = row
        .try_get("retired_at")
        .map_err(|e| map_row_err("workers", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("workers", &e))?;
    Ok(Worker {
        id: WorkerId(u64_from_i64(id)),
        node_id: node_id.map(|id| NodeId(u64_from_i64(id))),
        name,
        kind: WorkerKind::parse_database("workers.kind", &kind)?,
        status: WorkerStatus::parse_database("workers.status", &status)?,
        registered_at: parse_iso8601(&registered)?,
        last_seen_at: parse_iso8601(&last_seen)?,
        retired_at: retired.map(|s| parse_iso8601(&s)).transpose()?,
        epoch: u64_from_i64(epoch),
    })
}

fn row_to_inspection(row: &sqlx::sqlite::SqliteRow) -> Result<WorkerInspection, VoomError> {
    let worker = row_to_worker(row)?;
    let node_id: Option<i64> = row
        .try_get("node_context_id")
        .map_err(|e| map_row_err("workers inspection", &e))?;
    if let (Some(worker_node_id), None) = (worker.node_id, node_id) {
        return Err(VoomError::Database(format!(
            "workers inspection missing node context: worker_id={} node_id={}",
            worker.id, worker_node_id
        )));
    }
    let node = node_id
        .map(|id| {
            let name: String = row
                .try_get("node_context_name")
                .map_err(|e| map_row_err("workers inspection", &e))?;
            let kind: String = row
                .try_get("node_context_kind")
                .map_err(|e| map_row_err("workers inspection", &e))?;
            let status: String = row
                .try_get("node_context_status")
                .map_err(|e| map_row_err("workers inspection", &e))?;
            let last_seen: String = row
                .try_get("node_context_last_seen_at")
                .map_err(|e| map_row_err("workers inspection", &e))?;
            Ok(WorkerNodeContext {
                id: NodeId(u64_from_i64(id)),
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
        .map_err(|e| VoomError::Database(format!("parse worker {field}: {e}")))
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
