//! `WorkerRepo` — owns workers + `worker_capabilities` + `worker_grants`.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{VoomError, WorkerId};

use super::Repository;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerKind {
    Synthetic,
    Local,
    Remote,
}

impl WorkerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "synthetic" => Ok(Self::Synthetic),
            "local" => Ok(Self::Local),
            "remote" => Ok(Self::Remote),
            other => Err(VoomError::Database(format!(
                "workers.kind {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStatus {
    Registered,
    Active,
    Stale,
    Retired,
}

impl WorkerStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Retired => "retired",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "registered" => Ok(Self::Registered),
            "active" => Ok(Self::Active),
            "stale" => Ok(Self::Stale),
            "retired" => Ok(Self::Retired),
            other => Err(VoomError::Database(format!(
                "workers.status {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewWorker {
    pub name: String,
    pub kind: WorkerKind,
    pub registered_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct Worker {
    pub id: WorkerId,
    pub name: String,
    pub kind: WorkerKind,
    pub status: WorkerStatus,
    pub registered_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
    pub epoch: u64,
}

#[derive(Debug, Clone)]
pub struct NewCapability {
    pub worker_id: WorkerId,
    pub operation: String,
    pub codecs: Vec<String>,
    pub hardware: Vec<String>,
    pub artifact_access: Vec<String>,
    pub extra: JsonValue,
}

#[derive(Debug, Clone)]
pub struct Capability {
    pub id: u64,
    pub worker_id: WorkerId,
    pub operation: String,
}

#[derive(Debug, Clone)]
pub struct NewGrant {
    pub worker_id: WorkerId,
    pub can_execute: Vec<String>,
    pub can_access_read: Vec<String>,
    pub can_access_write: Vec<String>,
    pub denies: Vec<String>,
    pub max_parallel: JsonValue,
}

#[derive(Debug, Clone)]
pub struct Grant {
    pub id: u64,
    pub worker_id: WorkerId,
}

#[async_trait]
pub trait WorkerRepo: Repository {
    async fn register_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewWorker,
    ) -> Result<Worker, VoomError>;
    async fn register(&self, input: NewWorker) -> Result<Worker, VoomError>;

    async fn record_capability_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewCapability,
    ) -> Result<Capability, VoomError>;
    async fn record_capability(&self, input: NewCapability) -> Result<Capability, VoomError>;

    async fn record_grant_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewGrant,
    ) -> Result<Grant, VoomError>;
    async fn record_grant(&self, input: NewGrant) -> Result<Grant, VoomError>;

    async fn retire_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: WorkerId,
        expected_epoch: u64,
        now: OffsetDateTime,
    ) -> Result<Worker, VoomError>;
    async fn retire(
        &self,
        id: WorkerId,
        expected_epoch: u64,
        now: OffsetDateTime,
    ) -> Result<Worker, VoomError>;

    async fn get(&self, id: WorkerId) -> Result<Option<Worker>, VoomError>;
    async fn list_by_status(
        &self,
        status: WorkerStatus,
        limit: u32,
    ) -> Result<Vec<Worker>, VoomError>;
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

#[async_trait]
impl WorkerRepo for SqliteWorkerRepo {
    async fn register_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewWorker,
    ) -> Result<Worker, VoomError> {
        let ts = iso8601(input.registered_at)?;
        let res = sqlx::query(
            "INSERT INTO workers (name, kind, status, registered_at, last_seen_at) \
             VALUES (?, ?, 'registered', ?, ?)",
        )
        .bind(&input.name)
        .bind(input.kind.as_str())
        .bind(&ts)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("workers insert: {e}")))?;
        Ok(Worker {
            id: WorkerId(u64_from_i64(res.last_insert_rowid())),
            name: input.name,
            kind: input.kind,
            status: WorkerStatus::Registered,
            registered_at: input.registered_at,
            last_seen_at: input.registered_at,
            retired_at: None,
            epoch: 0,
        })
    }

    async fn register(&self, input: NewWorker) -> Result<Worker, VoomError> {
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

    async fn record_capability_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewCapability,
    ) -> Result<Capability, VoomError> {
        let codecs = serialize_string_vec(&input.codecs, "codecs")?;
        let hw = serialize_string_vec(&input.hardware, "hardware")?;
        let access = serialize_string_vec(&input.artifact_access, "artifact_access")?;
        let extra = serialize_json(&input.extra, "extra")?;
        let res = sqlx::query(
            "INSERT INTO worker_capabilities \
             (worker_id, operation, codecs, hardware, artifact_access, extra) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.worker_id.0))
        .bind(&input.operation)
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

    async fn record_capability(&self, input: NewCapability) -> Result<Capability, VoomError> {
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

    async fn record_grant_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewGrant,
    ) -> Result<Grant, VoomError> {
        let ce = serialize_string_vec(&input.can_execute, "can_execute")?;
        let cr = serialize_string_vec(&input.can_access_read, "can_access_read")?;
        let cw = serialize_string_vec(&input.can_access_write, "can_access_write")?;
        let d = serialize_string_vec(&input.denies, "denies")?;
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

    async fn record_grant(&self, input: NewGrant) -> Result<Grant, VoomError> {
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

    async fn retire_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: WorkerId,
        expected_epoch: u64,
        now: OffsetDateTime,
    ) -> Result<Worker, VoomError> {
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
                 (row missing, wrong epoch, or already retired)"
            )));
        }
        // Re-read inside the same transaction so the caller sees the updated
        // row. A pool-side `get` would query a different connection and miss
        // the uncommitted write.
        get_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("workers retire: row vanished post-update id={id}"))
        })
    }

    async fn retire(
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

    async fn get(&self, id: WorkerId) -> Result<Option<Worker>, VoomError> {
        let row = sqlx::query(
            "SELECT id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
             FROM workers WHERE id = ?",
        )
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("workers get: {e}")))?;
        row.as_ref().map(row_to_worker).transpose()
    }

    async fn list_by_status(
        &self,
        status: WorkerStatus,
        limit: u32,
    ) -> Result<Vec<Worker>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
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
}

async fn get_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: WorkerId,
) -> Result<Option<Worker>, VoomError> {
    let row = sqlx::query(
        "SELECT id, name, kind, status, registered_at, last_seen_at, retired_at, epoch \
         FROM workers WHERE id = ?",
    )
    .bind(i64_from_u64(id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("workers reload: {e}")))?;
    row.as_ref().map(row_to_worker).transpose()
}

fn row_to_worker(row: &sqlx::sqlite::SqliteRow) -> Result<Worker, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err(&e))?;
    let name: String = row.try_get("name").map_err(|e| map_row_err(&e))?;
    let kind: String = row.try_get("kind").map_err(|e| map_row_err(&e))?;
    let status: String = row.try_get("status").map_err(|e| map_row_err(&e))?;
    let registered: String = row.try_get("registered_at").map_err(|e| map_row_err(&e))?;
    let last_seen: String = row.try_get("last_seen_at").map_err(|e| map_row_err(&e))?;
    let retired: Option<String> = row.try_get("retired_at").map_err(|e| map_row_err(&e))?;
    let epoch: i64 = row.try_get("epoch").map_err(|e| map_row_err(&e))?;
    Ok(Worker {
        id: WorkerId(u64_from_i64(id)),
        name,
        kind: WorkerKind::parse(&kind)?,
        status: WorkerStatus::parse(&status)?,
        registered_at: parse_iso8601(&registered)?,
        last_seen_at: parse_iso8601(&last_seen)?,
        retired_at: retired.map(|s| parse_iso8601(&s)).transpose()?,
        epoch: u64_from_i64(epoch),
    })
}

fn serialize_string_vec(v: &[String], field: &str) -> Result<String, VoomError> {
    serde_json::to_string(v).map_err(|e| VoomError::Internal(format!("serialize {field}: {e}")))
}

fn serialize_json(v: &JsonValue, field: &str) -> Result<String, VoomError> {
    serde_json::to_string(v).map_err(|e| VoomError::Internal(format!("serialize {field}: {e}")))
}

fn map_row_err(e: &sqlx::Error) -> VoomError {
    VoomError::Database(format!("workers row decode: {e}"))
}

fn iso8601(t: OffsetDateTime) -> Result<String, VoomError> {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::Internal(format!("format iso8601: {e}")))
}

fn parse_iso8601(s: &str) -> Result<OffsetDateTime, VoomError> {
    OffsetDateTime::parse(s, &time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::Database(format!("parse iso8601 {s:?}: {e}")))
}

#[expect(clippy::cast_possible_wrap, reason = "rowid fits i64")]
const fn i64_from_u64(v: u64) -> i64 {
    v as i64
}
#[expect(clippy::cast_sign_loss, reason = "rowid is non-negative")]
const fn u64_from_i64(v: i64) -> u64 {
    v as u64
}

#[cfg(test)]
#[path = "workers_test.rs"]
mod tests;
