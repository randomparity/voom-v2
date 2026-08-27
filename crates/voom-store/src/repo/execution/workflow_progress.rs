//! Live workflow file admission, cursor advancement, and terminalization.

use sqlx::{Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::{JobId, MediaSnapshotId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, iso8601, parse_iso8601, u32_from_i64, u64_from_i64};
use super::workflow_summaries::{FilePhaseSummary, NewFilePhaseSummary, SqliteWorkflowSummaryRepo};
use crate::tx::begin_write_first;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFilePhaseEntry {
    pub job_id: JobId,
    pub phase_ordinal: u32,
    pub branch_id: String,
    pub media_snapshot_id: MediaSnapshotId,
    pub gate_admitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePhaseEntry {
    pub job_id: JobId,
    pub phase_ordinal: u32,
    pub branch_id: String,
    pub media_snapshot_id: MediaSnapshotId,
    pub gate_admitted: bool,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileProgressState {
    Pending,
    Active,
    Terminalizing,
    Terminal,
}

impl FileProgressState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Terminalizing => "terminalizing",
            Self::Terminal => "terminal",
        }
    }

    fn parse(value: &str) -> Result<Self, VoomError> {
        match value {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "terminalizing" => Ok(Self::Terminalizing),
            "terminal" => Ok(Self::Terminal),
            other => Err(VoomError::database(format!(
                "workflow_file_progress.state {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAdmissionTier {
    Interrupted,
    Pending,
}

impl FileAdmissionTier {
    const fn as_i64(self) -> i64 {
        match self {
            Self::Interrupted => 0,
            Self::Pending => 1,
        }
    }

    fn parse(value: i64) -> Result<Self, VoomError> {
        match value {
            0 => Ok(Self::Interrupted),
            1 => Ok(Self::Pending),
            other => Err(VoomError::database(format!(
                "workflow_file_progress.admission_tier {other} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFileProgress {
    pub branch_id: String,
    pub input_ordinal: u32,
    pub admission_tier: FileAdmissionTier,
    pub next_phase_ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileProgress {
    pub job_id: JobId,
    pub branch_id: String,
    pub input_ordinal: u32,
    pub admission_tier: FileAdmissionTier,
    pub state: FileProgressState,
    pub next_phase_ordinal: u32,
    pub admitted_at: Option<OffsetDateTime>,
    pub terminal_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWindow {
    pub job_id: JobId,
    pub max_in_flight_files: u32,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SqliteWorkflowProgressRepo {
    pool: SqlitePool,
}

impl Repository for SqliteWorkflowProgressRepo {}

impl SqliteWorkflowProgressRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a job's durable file-window capacity and initial pending cursors.
    ///
    /// # Errors
    /// Returns a configuration error for zero capacity or a database error when
    /// a row violates the job/run-start ownership constraints.
    pub async fn insert_file_window(
        &self,
        job_id: JobId,
        max_in_flight_files: u32,
        progress: Vec<NewFileProgress>,
        now: OffsetDateTime,
    ) -> Result<Vec<FileProgress>, VoomError> {
        if max_in_flight_files == 0 {
            return Err(VoomError::Config(
                "max_in_flight_files must be positive".to_owned(),
            ));
        }
        let mut tx = begin_write_first(&self.pool, "workflow_progress: insert_file_window").await?;
        self.insert_file_window_in_tx(&mut tx, job_id, max_in_flight_files, &progress, now)
            .await?;
        commit(tx).await?;
        self.file_progress_for_job(job_id).await
    }

    /// Insert window and progress rows in the caller's job-open transaction.
    pub async fn insert_file_window_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        job_id: JobId,
        max_in_flight_files: u32,
        progress: &[NewFileProgress],
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        if max_in_flight_files == 0 {
            return Err(VoomError::Config(
                "max_in_flight_files must be positive".to_owned(),
            ));
        }
        let timestamp = iso8601(now)?;
        sqlx::query(
            "INSERT INTO workflow_file_windows \
             (job_id, max_in_flight_files, created_at) VALUES (?, ?, ?)",
        )
        .bind(i64_from_u64(
            job_id.0,
            concat!(module_path!(), ": ", stringify!(job_id.0)),
        )?)
        .bind(i64::from(max_in_flight_files))
        .bind(&timestamp)
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("workflow file window insert", error))?;
        for input in progress {
            sqlx::query(
                "INSERT INTO workflow_file_progress \
                 (job_id, branch_id, input_ordinal, admission_tier, state, next_phase_ordinal) \
                 VALUES (?, ?, ?, ?, 'pending', ?)",
            )
            .bind(i64_from_u64(
                job_id.0,
                concat!(module_path!(), ": ", stringify!(job_id.0)),
            )?)
            .bind(&input.branch_id)
            .bind(i64::from(input.input_ordinal))
            .bind(input.admission_tier.as_i64())
            .bind(i64::from(input.next_phase_ordinal))
            .execute(&mut **tx)
            .await
            .map_err(|error| VoomError::database_context("workflow file progress insert", error))?;
        }
        Ok(())
    }

    /// Project already-completed branches as terminal in a newly opened resume job.
    pub async fn mark_file_progress_terminal_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        job_id: JobId,
        branches: &[String],
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let timestamp = iso8601(now)?;
        for branch_id in branches {
            let result = sqlx::query(
                "UPDATE workflow_file_progress \
                 SET state = 'terminal', admitted_at = ?, terminal_at = ? \
                 WHERE job_id = ? AND branch_id = ? AND state = 'pending'",
            )
            .bind(&timestamp)
            .bind(&timestamp)
            .bind(i64_from_u64(
                job_id.0,
                concat!(module_path!(), ": ", stringify!(job_id.0)),
            )?)
            .bind(branch_id)
            .execute(&mut **tx)
            .await
            .map_err(|error| {
                VoomError::database_context("resume terminal progress projection", error)
            })?;
            if result.rows_affected() != 1 {
                return Err(VoomError::Conflict(format!(
                    "resume terminal progress {job_id}/{branch_id} was not pending"
                )));
            }
        }
        Ok(())
    }

    /// Admit the next pending file when the durable window has capacity.
    pub async fn admit_next_file(
        &self,
        job_id: JobId,
        now: OffsetDateTime,
    ) -> Result<Option<FileProgress>, VoomError> {
        let mut tx = begin_write_first(&self.pool, "workflow_progress: admit_next_file").await?;
        let timestamp = iso8601(now)?;
        let sql = format!(
            "UPDATE workflow_file_progress SET state = 'active', admitted_at = ? \
             WHERE (job_id, branch_id) = ( \
                 SELECT pending.job_id, pending.branch_id \
                 FROM workflow_file_progress AS pending \
                 JOIN workflow_file_windows AS window ON window.job_id = pending.job_id \
                 JOIN jobs ON jobs.id = pending.job_id \
                 WHERE pending.job_id = ? AND pending.state = 'pending' \
                   AND jobs.state = 'open' \
                   AND (SELECT COUNT(*) FROM workflow_file_progress AS active \
                        WHERE active.job_id = pending.job_id \
                          AND active.state IN ('active', 'terminalizing')) \
                       < window.max_in_flight_files \
                 ORDER BY pending.admission_tier, pending.input_ordinal LIMIT 1 \
             ) \
             RETURNING {FILE_PROGRESS_COLUMNS}"
        );
        let row: Option<FileProgressRow> = sqlx::query_as(&sql)
            .bind(timestamp)
            .bind(i64_from_u64(
                job_id.0,
                concat!(module_path!(), ": ", stringify!(job_id.0)),
            )?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| VoomError::database_context("file admission", error))?;
        if row.is_none() {
            let state: Option<String> = sqlx::query_scalar(
                "SELECT jobs.state FROM workflow_file_windows \
                 JOIN jobs ON jobs.id = workflow_file_windows.job_id \
                 WHERE workflow_file_windows.job_id = ?",
            )
            .bind(i64_from_u64(
                job_id.0,
                concat!(module_path!(), ": ", stringify!(job_id.0)),
            )?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| VoomError::database_context("file window existence", error))?;
            match state.as_deref() {
                None => {
                    return Err(VoomError::NotFound(format!(
                        "workflow file window for job {job_id}"
                    )));
                }
                Some("open") => {}
                Some("cancelled") => {
                    return Err(VoomError::UserCancellation(format!(
                        "workflow file window job {job_id} is cancelled"
                    )));
                }
                Some("failed") => {
                    return Err(VoomError::PolicyExecution(format!(
                        "workflow file window job {job_id} is failed"
                    )));
                }
                Some("succeeded") => {
                    return Err(VoomError::Conflict(format!(
                        "workflow file window job {job_id} is succeeded"
                    )));
                }
                Some(other) => {
                    return Err(VoomError::database(format!(
                        "jobs.state {other:?} not in vocab during file admission"
                    )));
                }
            }
        }
        commit(tx).await?;
        row.map(decode_file_progress).transpose()
    }

    pub async fn advance_file_progress(
        &self,
        job_id: JobId,
        branch_id: &str,
        expected_phase_ordinal: u32,
        next_phase_ordinal: u32,
    ) -> Result<bool, VoomError> {
        let mut tx =
            begin_write_first(&self.pool, "workflow_progress: advance_file_progress").await?;
        let advanced = advance_file_progress_in_tx(
            &mut tx,
            job_id,
            branch_id,
            expected_phase_ordinal,
            next_phase_ordinal,
        )
        .await?;
        commit(tx).await?;
        Ok(advanced)
    }

    pub async fn upsert_file_phase_summary_and_advance(
        &self,
        input: NewFilePhaseSummary,
        expected_phase_ordinal: u32,
        next_phase_ordinal: u32,
        now: OffsetDateTime,
    ) -> Result<FilePhaseSummary, VoomError> {
        let mut tx = begin_write_first(
            &self.pool,
            "workflow_progress: upsert_file_phase_summary_and_advance",
        )
        .await?;
        let summary_repo = SqliteWorkflowSummaryRepo::new(self.pool.clone());
        let row = summary_repo
            .upsert_file_phase_summary_in_tx(&mut tx, input, now)
            .await?;
        let advanced = advance_file_progress_in_tx(
            &mut tx,
            row.job_id,
            &row.branch_id,
            expected_phase_ordinal,
            next_phase_ordinal,
        )
        .await?;
        if !advanced {
            let progress = fetch_file_progress(&mut *tx, row.job_id, &row.branch_id)
                .await?
                .ok_or_else(|| {
                    VoomError::NotFound(format!("file progress {}/{}", row.job_id, row.branch_id))
                })?;
            if progress.next_phase_ordinal != next_phase_ordinal
                || !matches!(
                    progress.state,
                    FileProgressState::Active
                        | FileProgressState::Terminalizing
                        | FileProgressState::Terminal
                )
            {
                return Err(VoomError::Conflict(format!(
                    "file progress cursor for branch {} did not advance from phase {}",
                    row.branch_id, expected_phase_ordinal
                )));
            }
        }
        commit(tx).await?;
        Ok(row)
    }

    pub async fn mark_file_terminal(
        &self,
        job_id: JobId,
        branch_id: &str,
        now: OffsetDateTime,
    ) -> Result<FileProgress, VoomError> {
        let timestamp = iso8601(now)?;
        sqlx::query(
            "UPDATE workflow_file_progress SET state = 'terminal', terminal_at = ? \
             WHERE job_id = ? AND branch_id = ? AND state = 'terminalizing'",
        )
        .bind(timestamp)
        .bind(i64_from_u64(
            job_id.0,
            concat!(module_path!(), ": ", stringify!(job_id.0)),
        )?)
        .bind(branch_id)
        .execute(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("file terminal transition", error))?;
        self.file_progress(job_id, branch_id)
            .await?
            .filter(|row| row.state == FileProgressState::Terminal)
            .ok_or_else(|| {
                VoomError::Conflict(format!(
                    "file progress {job_id}/{branch_id} is not active or terminal"
                ))
            })
    }

    pub async fn begin_file_terminalization(
        &self,
        job_id: JobId,
        branch_id: &str,
    ) -> Result<FileProgress, VoomError> {
        sqlx::query(
            "UPDATE workflow_file_progress SET state = 'terminalizing' \
             WHERE job_id = ? AND branch_id = ? AND state = 'active'",
        )
        .bind(i64_from_u64(
            job_id.0,
            concat!(module_path!(), ": ", stringify!(job_id.0)),
        )?)
        .bind(branch_id)
        .execute(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("file terminalization begin", error))?;
        self.file_progress(job_id, branch_id)
            .await?
            .filter(|row| row.state == FileProgressState::Terminalizing)
            .ok_or_else(|| {
                VoomError::Conflict(format!(
                    "file progress {job_id}/{branch_id} is not active or terminalizing"
                ))
            })
    }

    pub async fn file_progress(
        &self,
        job_id: JobId,
        branch_id: &str,
    ) -> Result<Option<FileProgress>, VoomError> {
        fetch_file_progress(&self.pool, job_id, branch_id).await
    }

    pub async fn file_progress_for_job(
        &self,
        job_id: JobId,
    ) -> Result<Vec<FileProgress>, VoomError> {
        let sql = format!(
            "SELECT {FILE_PROGRESS_COLUMNS} FROM workflow_file_progress \
             WHERE job_id = ? ORDER BY input_ordinal"
        );
        let rows: Vec<FileProgressRow> = sqlx::query_as(&sql)
            .bind(i64_from_u64(
                job_id.0,
                concat!(module_path!(), ": ", stringify!(job_id.0)),
            )?)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| VoomError::database_context("workflow file progress list", error))?;
        rows.into_iter().map(decode_file_progress).collect()
    }

    /// Resolve the durable branch assigned to one job-owned input ordinal.
    pub async fn branch_for_input_ordinal(
        &self,
        job_id: JobId,
        input_ordinal: u32,
    ) -> Result<Option<String>, VoomError> {
        let job_id = i64::try_from(job_id.0).map_err(|error| {
            VoomError::database_context("workflow branch job id exceeds SQLite i64", error)
        })?;
        sqlx::query_scalar(
            "SELECT branch_id FROM workflow_file_progress \
             WHERE job_id = ? AND input_ordinal = ?",
        )
        .bind(job_id)
        .bind(i64::from(input_ordinal))
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("workflow branch by input ordinal", error))
    }

    pub async fn file_window(&self, job_id: JobId) -> Result<Option<FileWindow>, VoomError> {
        let row: Option<(i64, i64, String)> = sqlx::query_as(
            "SELECT job_id, max_in_flight_files, created_at \
             FROM workflow_file_windows WHERE job_id = ?",
        )
        .bind(i64_from_u64(
            job_id.0,
            concat!(module_path!(), ": ", stringify!(job_id.0)),
        )?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("workflow file window get", error))?;
        row.map(|(job_id, maximum, created_at)| {
            Ok(FileWindow {
                job_id: JobId(u64_from_i64(
                    job_id,
                    concat!(module_path!(), ": ", stringify!(job_id)),
                )?),
                max_in_flight_files: u32_from_i64(maximum)?,
                created_at: parse_iso8601(&created_at)?,
            })
        })
        .transpose()
    }

    pub async fn upsert_file_phase_entry(
        &self,
        input: NewFilePhaseEntry,
        now: OffsetDateTime,
    ) -> Result<FilePhaseEntry, VoomError> {
        let created_at = iso8601(now)?;
        sqlx::query(
            "INSERT INTO workflow_file_phase_entries \
             (job_id, phase_ordinal, branch_id, media_snapshot_id, gate_admitted, created_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (job_id, phase_ordinal, branch_id) DO NOTHING",
        )
        .bind(i64_from_u64(
            input.job_id.0,
            concat!(module_path!(), ": ", stringify!(input.job_id.0)),
        )?)
        .bind(i64::from(input.phase_ordinal))
        .bind(&input.branch_id)
        .bind(i64_from_u64(
            input.media_snapshot_id.0,
            concat!(module_path!(), ": ", stringify!(input.media_snapshot_id.0)),
        )?)
        .bind(input.gate_admitted)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("workflow file phase entry insert", error))?;
        let stored = self
            .file_phase_entry(input.job_id, input.phase_ordinal, &input.branch_id)
            .await?
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "workflow file phase entry vanished for {}/{}/{}",
                    input.job_id, input.phase_ordinal, input.branch_id
                ))
            })?;
        if stored.media_snapshot_id != input.media_snapshot_id
            || stored.gate_admitted != input.gate_admitted
        {
            return Err(VoomError::Conflict(format!(
                "workflow file phase entry replay disagrees for {}/{}/{}",
                input.job_id, input.phase_ordinal, input.branch_id
            )));
        }
        Ok(stored)
    }

    pub async fn file_phase_entries_for_job(
        &self,
        job_id: JobId,
    ) -> Result<Vec<FilePhaseEntry>, VoomError> {
        let rows: Vec<(i64, i64, String, i64, bool, String)> = sqlx::query_as(
            "SELECT job_id, phase_ordinal, branch_id, media_snapshot_id, \
                    gate_admitted, created_at \
             FROM workflow_file_phase_entries WHERE job_id = ? \
             ORDER BY phase_ordinal, branch_id",
        )
        .bind(i64_from_u64(
            job_id.0,
            concat!(module_path!(), ": ", stringify!(job_id.0)),
        )?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("workflow file phase entry list", error))?;
        rows.into_iter().map(decode_file_phase_entry).collect()
    }

    async fn file_phase_entry(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        branch_id: &str,
    ) -> Result<Option<FilePhaseEntry>, VoomError> {
        let row: Option<(i64, i64, String, i64, bool, String)> = sqlx::query_as(
            "SELECT job_id, phase_ordinal, branch_id, media_snapshot_id, \
                    gate_admitted, created_at \
             FROM workflow_file_phase_entries \
             WHERE job_id = ? AND phase_ordinal = ? AND branch_id = ?",
        )
        .bind(i64_from_u64(
            job_id.0,
            concat!(module_path!(), ": ", stringify!(job_id.0)),
        )?)
        .bind(i64::from(phase_ordinal))
        .bind(branch_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("workflow file phase entry get", error))?;
        row.map(decode_file_phase_entry).transpose()
    }
}

async fn advance_file_progress_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    job_id: JobId,
    branch_id: &str,
    expected_phase_ordinal: u32,
    next_phase_ordinal: u32,
) -> Result<bool, VoomError> {
    let result = sqlx::query(
        "UPDATE workflow_file_progress SET next_phase_ordinal = ? \
             WHERE job_id = ? AND branch_id = ? AND state = 'active' \
               AND next_phase_ordinal = ?",
    )
    .bind(i64::from(next_phase_ordinal))
    .bind(i64_from_u64(
        job_id.0,
        concat!(module_path!(), ": ", stringify!(job_id.0)),
    )?)
    .bind(branch_id)
    .bind(i64::from(expected_phase_ordinal))
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("file progress advance", error))?;
    Ok(result.rows_affected() == 1)
}

type FileProgressRow = (
    i64,
    String,
    i64,
    i64,
    String,
    i64,
    Option<String>,
    Option<String>,
);

fn decode_file_progress(row: FileProgressRow) -> Result<FileProgress, VoomError> {
    Ok(FileProgress {
        job_id: JobId(u64_from_i64(
            row.0,
            concat!(module_path!(), ": ", stringify!(row.0)),
        )?),
        branch_id: row.1,
        input_ordinal: u32_from_i64(row.2)?,
        admission_tier: FileAdmissionTier::parse(row.3)?,
        state: FileProgressState::parse(&row.4)?,
        next_phase_ordinal: u32_from_i64(row.5)?,
        admitted_at: row.6.as_deref().map(parse_iso8601).transpose()?,
        terminal_at: row.7.as_deref().map(parse_iso8601).transpose()?,
    })
}

fn decode_file_phase_entry(
    row: (i64, i64, String, i64, bool, String),
) -> Result<FilePhaseEntry, VoomError> {
    Ok(FilePhaseEntry {
        job_id: JobId(u64_from_i64(
            row.0,
            concat!(module_path!(), ": ", stringify!(row.0)),
        )?),
        phase_ordinal: u32_from_i64(row.1)?,
        branch_id: row.2,
        media_snapshot_id: MediaSnapshotId(u64_from_i64(
            row.3,
            concat!(module_path!(), ": ", stringify!(row.3)),
        )?),
        gate_admitted: row.4,
        created_at: parse_iso8601(&row.5)?,
    })
}

async fn commit(tx: Transaction<'_, Sqlite>) -> Result<(), VoomError> {
    tx.commit()
        .await
        .map_err(|error| VoomError::database_context("commit workflow progress transaction", error))
}

async fn fetch_file_progress<'e, E>(
    exec: E,
    job_id: JobId,
    branch_id: &str,
) -> Result<Option<FileProgress>, VoomError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let sql = format!(
        "SELECT {FILE_PROGRESS_COLUMNS} FROM workflow_file_progress \
         WHERE job_id = ? AND branch_id = ?"
    );
    let row: Option<FileProgressRow> = sqlx::query_as(&sql)
        .bind(i64_from_u64(
            job_id.0,
            concat!(module_path!(), ": ", stringify!(job_id.0)),
        )?)
        .bind(branch_id)
        .fetch_optional(exec)
        .await
        .map_err(|error| VoomError::database_context("workflow file progress get", error))?;
    row.map(decode_file_progress).transpose()
}

const FILE_PROGRESS_COLUMNS: &str = "job_id, branch_id, input_ordinal, admission_tier, state, next_phase_ordinal, \
     admitted_at, terminal_at";

#[cfg(test)]
#[path = "workflow_progress_test.rs"]
mod tests;
