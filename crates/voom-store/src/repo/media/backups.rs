//! `SqliteBackupRepo` — durable backup records (Sprint 17, T9).
//!
//! A backup is written `pending` before the copy and transitioned to
//! `verified` or `failed` after, so a crash mid-backup leaves a recoverable
//! `pending` row (`finished_at IS NULL`). The `backups_verified_key`
//! partial-unique index enforces at most one verified backup per
//! `(ticket, source version)` so a retried mutating operation reuses the
//! existing copy instead of writing a duplicate. Shape and rationale:
//! `docs/adr/0025-backup-worker-and-backup-before-mutation-gate.md`.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::{BackupId, FileVersionId, JobId, TicketId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, iso8601, map_row_err, parse_iso8601, u64_from_i64};

/// Lifecycle of a backup record. Mirrors the `backups.status` CHECK exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStatus {
    Pending,
    Verified,
    Failed,
}

impl BackupStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Verified => "verified",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "pending" => Ok(Self::Pending),
            "verified" => Ok(Self::Verified),
            "failed" => Ok(Self::Failed),
            other => Err(VoomError::database(format!(
                "backups.status {other:?} not in vocab"
            ))),
        }
    }
}

/// Terminal-failure detail written when a backup fails. All three fields live
/// or die together (enforced by the DB CHECK).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFailureDetail {
    pub failure_class: String,
    pub error_code: String,
    pub message: String,
}

/// Input for a new `pending` backup record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBackup {
    pub source_file_version_id: FileVersionId,
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub provider: String,
    pub destination_path: String,
}

/// A backup record row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    pub id: BackupId,
    pub source_file_version_id: FileVersionId,
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub provider: String,
    pub destination_path: String,
    pub size_bytes: Option<u64>,
    pub checksum: Option<String>,
    pub status: BackupStatus,
    pub failure_class: Option<String>,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub started_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SqliteBackupRepo {
    pool: SqlitePool,
}

impl SqliteBackupRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteBackupRepo {}

const BACKUP_COLS: &str = "id, source_file_version_id, job_id, ticket_id, provider, \
     destination_path, size_bytes, checksum, status, failure_class, error_code, message, \
     started_at, finished_at, created_at";

impl SqliteBackupRepo {
    pub async fn insert_pending_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewBackup,
        now: OffsetDateTime,
    ) -> Result<Backup, VoomError> {
        let started = iso8601(now)?;
        let res = sqlx::query(
            "INSERT INTO backups \
             (source_file_version_id, job_id, ticket_id, provider, destination_path, status, \
              started_at, created_at) \
             VALUES (?, ?, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(i64_from_u64(input.source_file_version_id.0))
        .bind(i64_from_u64(input.job_id.0))
        .bind(i64_from_u64(input.ticket_id.0))
        .bind(&input.provider)
        .bind(&input.destination_path)
        .bind(&started)
        .bind(&started)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("backups insert_pending", e))?;
        Ok(Backup {
            id: BackupId(u64_from_i64(res.last_insert_rowid())),
            source_file_version_id: input.source_file_version_id,
            job_id: input.job_id,
            ticket_id: input.ticket_id,
            provider: input.provider,
            destination_path: input.destination_path,
            size_bytes: None,
            checksum: None,
            status: BackupStatus::Pending,
            failure_class: None,
            error_code: None,
            message: None,
            started_at: now,
            finished_at: None,
            created_at: now,
        })
    }

    pub async fn insert_pending(
        &self,
        input: NewBackup,
        now: OffsetDateTime,
    ) -> Result<Backup, VoomError> {
        let mut tx = begin(&self.pool).await?;
        let out = self.insert_pending_in_tx(&mut tx, input, now).await?;
        commit(tx).await?;
        Ok(out)
    }

    pub async fn mark_verified_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: BackupId,
        size_bytes: u64,
        checksum: &str,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let finished = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE backups \
             SET status = 'verified', size_bytes = ?, checksum = ?, finished_at = ? \
             WHERE id = ? AND status = 'pending'",
        )
        .bind(i64_from_u64(size_bytes))
        .bind(checksum)
        .bind(&finished)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("backups mark_verified", e))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "backups mark_verified: id={id} not in pending state"
            )));
        }
        Ok(())
    }

    pub async fn mark_verified(
        &self,
        id: BackupId,
        size_bytes: u64,
        checksum: &str,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let mut tx = begin(&self.pool).await?;
        self.mark_verified_in_tx(&mut tx, id, size_bytes, checksum, now)
            .await?;
        commit(tx).await
    }

    pub async fn mark_failed_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: BackupId,
        detail: &BackupFailureDetail,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let finished = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE backups \
             SET status = 'failed', failure_class = ?, error_code = ?, message = ?, \
                 finished_at = ? \
             WHERE id = ? AND status = 'pending'",
        )
        .bind(&detail.failure_class)
        .bind(&detail.error_code)
        .bind(&detail.message)
        .bind(&finished)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("backups mark_failed", e))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "backups mark_failed: id={id} not in pending state"
            )));
        }
        Ok(())
    }

    pub async fn mark_failed(
        &self,
        id: BackupId,
        detail: &BackupFailureDetail,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let mut tx = begin(&self.pool).await?;
        self.mark_failed_in_tx(&mut tx, id, detail, now).await?;
        commit(tx).await
    }

    pub async fn get(&self, id: BackupId) -> Result<Option<Backup>, VoomError> {
        let row = sqlx::query(&format!("SELECT {BACKUP_COLS} FROM backups WHERE id = ?"))
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("backups get", e))?;
        row.as_ref().map(row_to_backup).transpose()
    }

    /// Keyset-paginated inspection read for `voom backup list` (ADR 0031),
    /// optionally filtered by status. Orders strictly by `id` descending
    /// (newest first); `after_id` is an exclusive continuation token returning
    /// rows with `id < after_id`.
    pub async fn list(
        &self,
        status: Option<BackupStatus>,
        after_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<Backup>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {BACKUP_COLS} FROM backups \
             WHERE (?1 IS NULL OR status = ?1) \
               AND (?2 IS NULL OR id < ?2) \
             ORDER BY id DESC LIMIT ?3"
        ))
        .bind(status.map(BackupStatus::as_str))
        .bind(after_id.map(i64_from_u64))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("backups list", e))?;
        rows.iter().map(row_to_backup).collect()
    }

    pub async fn list_by_status(
        &self,
        status: BackupStatus,
        limit: u32,
    ) -> Result<Vec<Backup>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {BACKUP_COLS} FROM backups WHERE status = ? \
             ORDER BY created_at ASC, id ASC LIMIT ?"
        ))
        .bind(status.as_str())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("backups list_by_status", e))?;
        rows.iter().map(row_to_backup).collect()
    }

    pub async fn list_by_file_version(
        &self,
        source_file_version_id: FileVersionId,
        limit: u32,
    ) -> Result<Vec<Backup>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {BACKUP_COLS} FROM backups WHERE source_file_version_id = ? \
             ORDER BY created_at ASC, id ASC LIMIT ?"
        ))
        .bind(i64_from_u64(source_file_version_id.0))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("backups list_by_file_version", e))?;
        rows.iter().map(row_to_backup).collect()
    }

    /// Pending backups with no terminal transition — a crash-recovery signal.
    pub async fn list_pending(&self, limit: u32) -> Result<Vec<Backup>, VoomError> {
        self.list_by_status(BackupStatus::Pending, limit).await
    }

    /// The most recent backup for a source file version by `created_at` then
    /// `id`, or `None`. The safety gate consults this for its latest-record
    /// semantics (ADR 0028): a later verified backup supersedes an earlier
    /// failed one, so a retried operation clears a prior-failure block.
    pub async fn latest_by_file_version(
        &self,
        source_file_version_id: FileVersionId,
    ) -> Result<Option<Backup>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {BACKUP_COLS} FROM backups WHERE source_file_version_id = ? \
             ORDER BY created_at DESC, id DESC LIMIT 1"
        ))
        .bind(i64_from_u64(source_file_version_id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("backups latest_by_file_version", e))?;
        row.as_ref().map(row_to_backup).transpose()
    }

    /// The verified backup for `(ticket, source version)`, if any. The
    /// idempotency short-circuit for the execute-path gate: when this returns
    /// `Some`, a retried operation reuses the existing copy.
    pub async fn verified_for_ticket_and_version(
        &self,
        ticket_id: TicketId,
        source_file_version_id: FileVersionId,
    ) -> Result<Option<Backup>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {BACKUP_COLS} FROM backups \
             WHERE ticket_id = ? AND source_file_version_id = ? AND status = 'verified'"
        ))
        .bind(i64_from_u64(ticket_id.0))
        .bind(i64_from_u64(source_file_version_id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("backups verified_for_ticket_and_version", e))?;
        row.as_ref().map(row_to_backup).transpose()
    }
}

async fn begin(pool: &SqlitePool) -> Result<Transaction<'static, Sqlite>, VoomError> {
    pool.begin()
        .await
        .map_err(|e| VoomError::database_context("begin", e))
}

async fn commit(tx: Transaction<'_, Sqlite>) -> Result<(), VoomError> {
    tx.commit()
        .await
        .map_err(|e| VoomError::database_context("commit", e))
}

fn row_to_backup(row: &SqliteRow) -> Result<Backup, VoomError> {
    let t = "backups";
    let id: i64 = row.try_get("id").map_err(|e| map_row_err(t, &e))?;
    let source_file_version_id: i64 = row
        .try_get("source_file_version_id")
        .map_err(|e| map_row_err(t, &e))?;
    let job_id: i64 = row.try_get("job_id").map_err(|e| map_row_err(t, &e))?;
    let ticket_id: i64 = row.try_get("ticket_id").map_err(|e| map_row_err(t, &e))?;
    let provider: String = row.try_get("provider").map_err(|e| map_row_err(t, &e))?;
    let destination_path: String = row
        .try_get("destination_path")
        .map_err(|e| map_row_err(t, &e))?;
    let size_bytes: Option<i64> = row.try_get("size_bytes").map_err(|e| map_row_err(t, &e))?;
    let checksum: Option<String> = row.try_get("checksum").map_err(|e| map_row_err(t, &e))?;
    let status: String = row.try_get("status").map_err(|e| map_row_err(t, &e))?;
    let failure_class: Option<String> = row
        .try_get("failure_class")
        .map_err(|e| map_row_err(t, &e))?;
    let error_code: Option<String> = row.try_get("error_code").map_err(|e| map_row_err(t, &e))?;
    let message: Option<String> = row.try_get("message").map_err(|e| map_row_err(t, &e))?;
    let started_at: String = row.try_get("started_at").map_err(|e| map_row_err(t, &e))?;
    let finished_at: Option<String> = row.try_get("finished_at").map_err(|e| map_row_err(t, &e))?;
    let created_at: String = row.try_get("created_at").map_err(|e| map_row_err(t, &e))?;
    Ok(Backup {
        id: BackupId(u64_from_i64(id)),
        source_file_version_id: FileVersionId(u64_from_i64(source_file_version_id)),
        job_id: JobId(u64_from_i64(job_id)),
        ticket_id: TicketId(u64_from_i64(ticket_id)),
        provider,
        destination_path,
        size_bytes: size_bytes.map(u64_from_i64),
        checksum,
        status: BackupStatus::parse(&status)?,
        failure_class,
        error_code,
        message,
        started_at: parse_iso8601(&started_at)?,
        finished_at: finished_at.as_deref().map(parse_iso8601).transpose()?,
        created_at: parse_iso8601(&created_at)?,
    })
}

#[cfg(test)]
#[path = "backups_test.rs"]
mod tests;
