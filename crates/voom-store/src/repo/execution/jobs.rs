//! `SqliteJobRepo` — durable journal of operator-initiated work.

use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{JobId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, iso8601, map_row_err, parse_iso8601, u64_from_i64};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Open,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "open" => Ok(Self::Open),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(VoomError::database(format!(
                "jobs.state {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: String,
    pub priority: i64,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: JobId,
    pub kind: String,
    pub state: JobState,
    pub priority: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub epoch: u64,
}

#[derive(Debug, Clone)]
pub struct SqliteJobRepo {
    pool: SqlitePool,
}

impl SqliteJobRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteJobRepo {}

impl SqliteJobRepo {
    pub async fn create_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewJob,
    ) -> Result<Job, VoomError> {
        let created = iso8601(input.created_at)?;
        let res = sqlx::query(
            "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
             VALUES (?, 'open', ?, ?, ?)",
        )
        .bind(&input.kind)
        .bind(input.priority)
        .bind(&created)
        .bind(&created)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("jobs insert", e))?;
        Ok(Job {
            id: JobId(u64_from_i64(res.last_insert_rowid())),
            kind: input.kind,
            state: JobState::Open,
            priority: input.priority,
            created_at: input.created_at,
            updated_at: input.created_at,
            epoch: 0,
        })
    }

    pub async fn create(&self, input: NewJob) -> Result<Job, VoomError> {
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

    pub async fn get(&self, id: JobId) -> Result<Option<Job>, VoomError> {
        let row = sqlx::query(
            "SELECT id, kind, state, priority, created_at, updated_at, epoch \
             FROM jobs WHERE id = ?",
        )
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("jobs get", e))?;
        row.as_ref().map(row_to_job).transpose()
    }

    pub async fn list_by_state(&self, state: JobState, limit: u32) -> Result<Vec<Job>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, kind, state, priority, created_at, updated_at, epoch \
             FROM jobs WHERE state = ? \
             ORDER BY priority DESC, id ASC LIMIT ?",
        )
        .bind(state.as_str())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("jobs list", e))?;
        rows.iter().map(row_to_job).collect()
    }

    pub async fn succeed_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: JobId,
        now: OffsetDateTime,
    ) -> Result<Job, VoomError> {
        transition_open_to(tx, id, JobState::Succeeded, now).await
    }

    pub async fn succeed(&self, id: JobId, now: OffsetDateTime) -> Result<Job, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.succeed_in_tx(&mut tx, id, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    pub async fn fail_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: JobId,
        now: OffsetDateTime,
    ) -> Result<Job, VoomError> {
        transition_open_to(tx, id, JobState::Failed, now).await
    }

    pub async fn fail(&self, id: JobId, now: OffsetDateTime) -> Result<Job, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.fail_in_tx(&mut tx, id, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    pub async fn cancel_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: JobId,
        now: OffsetDateTime,
    ) -> Result<Job, VoomError> {
        transition_open_to(tx, id, JobState::Cancelled, now).await
    }

    pub async fn cancel(&self, id: JobId, now: OffsetDateTime) -> Result<Job, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.cancel_in_tx(&mut tx, id, now).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }
}

async fn transition_open_to(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: JobId,
    next: JobState,
    now: OffsetDateTime,
) -> Result<Job, VoomError> {
    let updated = iso8601(now)?;
    let res = sqlx::query(
        "UPDATE jobs SET state = ?, updated_at = ?, epoch = epoch + 1 \
         WHERE id = ? AND state = 'open'",
    )
    .bind(next.as_str())
    .bind(&updated)
    .bind(i64_from_u64(id.0))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("jobs update", e))?;
    if res.rows_affected() == 0 {
        return Err(VoomError::Conflict(format!(
            "jobs transition rejected: id={id} next={next:?} \
             (row missing or non-open state)"
        )));
    }
    // Re-read inside the same transaction so the caller sees the updated
    // row. A pool-side `get` would query a different connection and miss
    // the uncommitted write.
    let row = sqlx::query(
        "SELECT id, kind, state, priority, created_at, updated_at, epoch \
         FROM jobs WHERE id = ?",
    )
    .bind(i64_from_u64(id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("jobs reload", e))?;
    row.as_ref().map(row_to_job).transpose()?.ok_or_else(|| {
        VoomError::Internal(format!("jobs transition: row vanished post-update id={id}"))
    })
}

fn row_to_job(row: &sqlx::sqlite::SqliteRow) -> Result<Job, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("jobs", &e))?;
    let kind: String = row.try_get("kind").map_err(|e| map_row_err("jobs", &e))?;
    let state_str: String = row.try_get("state").map_err(|e| map_row_err("jobs", &e))?;
    let priority: i64 = row
        .try_get("priority")
        .map_err(|e| map_row_err("jobs", &e))?;
    let created: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("jobs", &e))?;
    let updated: String = row
        .try_get("updated_at")
        .map_err(|e| map_row_err("jobs", &e))?;
    let epoch: i64 = row.try_get("epoch").map_err(|e| map_row_err("jobs", &e))?;
    Ok(Job {
        id: JobId(u64_from_i64(id)),
        kind,
        state: JobState::parse(&state_str)?,
        priority,
        created_at: parse_iso8601(&created)?,
        updated_at: parse_iso8601(&updated)?,
        epoch: u64_from_i64(epoch),
    })
}

#[cfg(test)]
#[path = "jobs_test.rs"]
mod tests;
