//! `SqliteWorkflowSummaryRepo` — durable three-grain workflow summaries.
//!
//! A job-level parent (`workflow_summaries`), a per-phase child
//! (`workflow_phase_summaries`) carrying that phase's folded compliance report,
//! and a per-`(file, phase)` grandchild (`workflow_file_phase_summaries`) linking
//! each advanced file to its tickets, produced artifacts, and re-probe snapshot.
//! Child writes are idempotent first-write-wins so the Sprint 16 coordinator's
//! finalize/resume backfill paths never collide. Shape and rationale:
//! `docs/adr/0006-workflow-summary-schema.md`.

use std::time::Duration;

use serde_json::Value;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::ids::ArtifactVerificationId;
use voom_core::{
    ArtifactHandleId, FileLocationId, FileVersionId, JobId, MediaSnapshotId, TicketId, VoomError,
};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};

/// Outcome of a whole phase across the input set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseOutcome {
    Completed,
    PartiallyCommitted,
    Skipped,
    Blocked,
}

impl PhaseOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::PartiallyCommitted => "partially-committed",
            Self::Skipped => "skipped",
            Self::Blocked => "blocked",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "completed" => Ok(Self::Completed),
            "partially-committed" => Ok(Self::PartiallyCommitted),
            "skipped" => Ok(Self::Skipped),
            "blocked" => Ok(Self::Blocked),
            other => Err(VoomError::database(format!(
                "workflow_phase_summaries.outcome {other:?} not in vocab"
            ))),
        }
    }
}

/// Outcome of one file within a phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePhaseOutcome {
    Committed,
    Verified,
    Skipped,
    Blocked,
}

impl FilePhaseOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Verified => "verified",
            Self::Skipped => "skipped",
            Self::Blocked => "blocked",
        }
    }

    fn parse(s: &str, table: &'static str) -> Result<Self, VoomError> {
        match s {
            "committed" => Ok(Self::Committed),
            "verified" => Ok(Self::Verified),
            "skipped" => Ok(Self::Skipped),
            "blocked" => Ok(Self::Blocked),
            other => Err(VoomError::database(format!(
                "{table}.outcome {other:?} not in vocab"
            ))),
        }
    }
}

/// A phase's content-addressed compliance report. `report_id` and `report` live
/// or die together; modeling them as one optional value makes the both-or-neither
/// invariant unrepresentable when violated.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseReport {
    pub report_id: String,
    pub report: Value,
}

/// Job-level summary input: the `WorkflowRunSummary` counters plus the
/// `per_operation` rollup (an opaque JSON document the caller serializes).
#[derive(Debug, Clone, PartialEq)]
pub struct NewWorkflowSummary {
    pub job_id: JobId,
    pub branch_count: u32,
    pub ticket_count: u32,
    pub dispatch_count: u64,
    pub retry_count: u64,
    pub failure_count: u64,
    pub peak_active_workflow_leases: u32,
    pub elapsed: Duration,
    pub per_operation: Value,
}

/// Job-level summary row.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowSummary {
    pub job_id: JobId,
    pub branch_count: u32,
    pub ticket_count: u32,
    pub dispatch_count: u64,
    pub retry_count: u64,
    pub failure_count: u64,
    pub peak_active_workflow_leases: u32,
    pub elapsed: Duration,
    pub per_operation: Value,
    pub created_at: OffsetDateTime,
}

/// Per-phase summary input.
#[derive(Debug, Clone, PartialEq)]
pub struct NewPhaseSummary {
    pub job_id: JobId,
    pub phase_ordinal: u32,
    pub phase_name: String,
    pub report: Option<PhaseReport>,
    pub outcome: PhaseOutcome,
}

/// Per-phase summary row.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseSummary {
    pub id: u64,
    pub job_id: JobId,
    pub phase_ordinal: u32,
    pub phase_name: String,
    pub report: Option<PhaseReport>,
    pub outcome: PhaseOutcome,
    pub created_at: OffsetDateTime,
}

/// Per-`(file, phase)` summary input. File references are present for advancing
/// `Committed` outcomes and read-only `Verified` outcomes (enforced by a DB
/// CHECK).
#[derive(Debug, Clone, PartialEq)]
pub struct NewFilePhaseSummary {
    pub job_id: JobId,
    pub phase_ordinal: u32,
    /// The file's branch identity. This is the executor's `branch_id` (the path
    /// stem; `workflow/binding.rs`), assumed unique within a `(job, phase)`. The
    /// idempotent upsert keys on it, so a job whose input set admits two files
    /// with the same stem would record only the first — guarding against
    /// same-stem inputs is the branch-binding layer's job, not this repo's.
    pub branch_id: String,
    pub ticket_ids: Vec<TicketId>,
    pub produced_file_version_id: Option<FileVersionId>,
    pub produced_file_location_id: Option<FileLocationId>,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_verification_id: Option<ArtifactVerificationId>,
    pub reprobe_snapshot_id: Option<MediaSnapshotId>,
    pub outcome: FilePhaseOutcome,
}

/// Per-`(file, phase)` summary row.
#[derive(Debug, Clone, PartialEq)]
pub struct FilePhaseSummary {
    pub id: u64,
    pub job_id: JobId,
    pub phase_ordinal: u32,
    pub branch_id: String,
    pub ticket_ids: Vec<TicketId>,
    pub produced_file_version_id: Option<FileVersionId>,
    pub produced_file_location_id: Option<FileLocationId>,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_verification_id: Option<ArtifactVerificationId>,
    pub reprobe_snapshot_id: Option<MediaSnapshotId>,
    pub outcome: FilePhaseOutcome,
    pub created_at: OffsetDateTime,
}

/// Immutable per-file cursor inserted when a phase-barrier job opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFileRunStart {
    pub branch_id: String,
    pub starting_file_version_id: FileVersionId,
    pub starting_phase_ordinal: u32,
}

/// Stored per-file cursor for one phase-barrier job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRunStart {
    pub job_id: JobId,
    pub branch_id: String,
    pub starting_file_version_id: FileVersionId,
    pub starting_phase_ordinal: u32,
}

/// One prior phase outcome copied into a new phase-barrier job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFileRunHistory {
    pub branch_id: String,
    pub phase_ordinal: u32,
    pub outcome: FilePhaseOutcome,
}

/// Stored prior phase outcome for one file in a phase-barrier job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRunHistory {
    pub job_id: JobId,
    pub branch_id: String,
    pub phase_ordinal: u32,
    pub outcome: FilePhaseOutcome,
}

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
pub struct SqliteWorkflowSummaryRepo {
    pool: SqlitePool,
}

impl SqliteWorkflowSummaryRepo {
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
        let mut tx = begin(&self.pool).await?;
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
        .bind(i64_from_u64(job_id.0))
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
            .bind(i64_from_u64(job_id.0))
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
            .bind(i64_from_u64(job_id.0))
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
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| VoomError::database_context("file admission begin", error))?;
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
            .bind(i64_from_u64(job_id.0))
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| VoomError::database_context("file admission", error))?;
        if row.is_none() {
            let state: Option<String> = sqlx::query_scalar(
                "SELECT jobs.state FROM workflow_file_windows \
                 JOIN jobs ON jobs.id = workflow_file_windows.job_id \
                 WHERE workflow_file_windows.job_id = ?",
            )
            .bind(i64_from_u64(job_id.0))
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
        let mut tx = begin(&self.pool).await?;
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
        let mut tx = begin(&self.pool).await?;
        let row = self
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
        .bind(i64_from_u64(job_id.0))
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
        .bind(i64_from_u64(job_id.0))
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
            .bind(i64_from_u64(job_id.0))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| VoomError::database_context("workflow file progress list", error))?;
        rows.into_iter().map(decode_file_progress).collect()
    }

    pub async fn file_window(&self, job_id: JobId) -> Result<Option<FileWindow>, VoomError> {
        let row: Option<(i64, i64, String)> = sqlx::query_as(
            "SELECT job_id, max_in_flight_files, created_at \
             FROM workflow_file_windows WHERE job_id = ?",
        )
        .bind(i64_from_u64(job_id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("workflow file window get", error))?;
        row.map(|(job_id, maximum, created_at)| {
            Ok(FileWindow {
                job_id: JobId(u64_from_i64(job_id)),
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
        .bind(i64_from_u64(input.job_id.0))
        .bind(i64::from(input.phase_ordinal))
        .bind(&input.branch_id)
        .bind(i64_from_u64(input.media_snapshot_id.0))
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
        .bind(i64_from_u64(job_id.0))
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
        .bind(i64_from_u64(job_id.0))
        .bind(i64::from(phase_ordinal))
        .bind(branch_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("workflow file phase entry get", error))?;
        row.map(decode_file_phase_entry).transpose()
    }

    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
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
    .bind(i64_from_u64(job_id.0))
    .bind(branch_id)
    .bind(i64::from(expected_phase_ordinal))
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("file progress advance", error))?;
    Ok(result.rows_affected() == 1)
}

impl Repository for SqliteWorkflowSummaryRepo {}

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
        job_id: JobId(u64_from_i64(row.0)),
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
        job_id: JobId(u64_from_i64(row.0)),
        phase_ordinal: u32_from_i64(row.1)?,
        branch_id: row.2,
        media_snapshot_id: MediaSnapshotId(u64_from_i64(row.3)),
        gate_admitted: row.4,
        created_at: parse_iso8601(&row.5)?,
    })
}

const FILE_PROGRESS_COLUMNS: &str = "job_id, branch_id, input_ordinal, admission_tier, state, next_phase_ordinal, \
     admitted_at, terminal_at";

const SUMMARY_COLS: &str = "job_id, branch_count, ticket_count, dispatch_count, retry_count, \
     failure_count, peak_active_workflow_leases, elapsed_ns, per_operation, created_at";

const PHASE_COLS: &str =
    "id, job_id, phase_ordinal, phase_name, report_id, report, outcome, created_at";

const FILE_PHASE_COLS: &str = "id, job_id, phase_ordinal, branch_id, ticket_ids, \
     produced_file_version_id, produced_file_location_id, artifact_handle_id, \
     artifact_verification_id, reprobe_snapshot_id, outcome, created_at";

const FILE_RUN_START_COLS: &str =
    "job_id, branch_id, starting_file_version_id, starting_phase_ordinal";

const FILE_RUN_HISTORY_COLS: &str = "job_id, branch_id, phase_ordinal, outcome";

impl SqliteWorkflowSummaryRepo {
    /// Insert every file cursor in the caller's transaction.
    ///
    /// # Errors
    /// Returns a database error if any cursor violates a key, foreign key, or
    /// ordinal constraint. The caller owns rollback of the complete batch.
    pub async fn insert_file_run_starts_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        job_id: JobId,
        inputs: &[NewFileRunStart],
    ) -> Result<(), VoomError> {
        for input in inputs {
            sqlx::query(
                "INSERT INTO workflow_file_run_starts \
                 (job_id, branch_id, starting_file_version_id, starting_phase_ordinal) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(i64_from_u64(job_id.0))
            .bind(&input.branch_id)
            .bind(i64_from_u64(input.starting_file_version_id.0))
            .bind(i64::from(input.starting_phase_ordinal))
            .execute(&mut **tx)
            .await
            .map_err(|error| {
                VoomError::database_context("workflow_file_run_starts insert", error)
            })?;
        }
        Ok(())
    }

    /// Insert inherited file-phase outcomes in the caller's transaction.
    ///
    /// # Errors
    /// Returns a database error if any row violates a key, parent-run, ordinal,
    /// or outcome constraint. The caller owns rollback of the complete batch.
    pub async fn insert_file_run_history_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        job_id: JobId,
        inputs: &[NewFileRunHistory],
    ) -> Result<(), VoomError> {
        for input in inputs {
            sqlx::query(
                "INSERT INTO workflow_file_run_history \
                 (job_id, branch_id, phase_ordinal, outcome) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(i64_from_u64(job_id.0))
            .bind(&input.branch_id)
            .bind(i64::from(input.phase_ordinal))
            .bind(input.outcome.as_str())
            .execute(&mut **tx)
            .await
            .map_err(|error| {
                VoomError::database_context("workflow_file_run_history insert", error)
            })?;
        }
        Ok(())
    }

    /// Atomically insert a complete run-start batch and return it in inspection
    /// order.
    ///
    /// # Errors
    /// Propagates transaction and constraint failures.
    pub async fn insert_file_run_starts(
        &self,
        job_id: JobId,
        inputs: Vec<NewFileRunStart>,
    ) -> Result<Vec<FileRunStart>, VoomError> {
        let mut tx = begin(&self.pool).await?;
        self.insert_file_run_starts_in_tx(&mut tx, job_id, &inputs)
            .await?;
        commit(tx).await?;
        self.file_run_starts_for_job(job_id).await
    }

    /// Atomically insert inherited file-phase outcomes and return them in
    /// inspection order.
    ///
    /// # Errors
    /// Propagates transaction and constraint failures.
    pub async fn insert_file_run_history(
        &self,
        job_id: JobId,
        inputs: Vec<NewFileRunHistory>,
    ) -> Result<Vec<FileRunHistory>, VoomError> {
        let mut tx = begin(&self.pool).await?;
        self.insert_file_run_history_in_tx(&mut tx, job_id, &inputs)
            .await?;
        commit(tx).await?;
        self.file_run_history_for_job(job_id).await
    }

    pub async fn insert_summary_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewWorkflowSummary,
        now: OffsetDateTime,
    ) -> Result<WorkflowSummary, VoomError> {
        let created = iso8601(now)?;
        let elapsed_ns = elapsed_to_ns(input.elapsed)?;
        let per_operation = serialize_json(&input.per_operation, "per_operation")?;
        sqlx::query(&format!(
            "INSERT INTO workflow_summaries ({SUMMARY_COLS}) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(i64_from_u64(input.job_id.0))
        .bind(i64::from(input.branch_count))
        .bind(i64::from(input.ticket_count))
        .bind(i64_from_u64(input.dispatch_count))
        .bind(i64_from_u64(input.retry_count))
        .bind(i64_from_u64(input.failure_count))
        .bind(i64::from(input.peak_active_workflow_leases))
        .bind(elapsed_ns)
        .bind(&per_operation)
        .bind(&created)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("workflow_summaries insert", e))?;
        Ok(WorkflowSummary {
            job_id: input.job_id,
            branch_count: input.branch_count,
            ticket_count: input.ticket_count,
            dispatch_count: input.dispatch_count,
            retry_count: input.retry_count,
            failure_count: input.failure_count,
            peak_active_workflow_leases: input.peak_active_workflow_leases,
            elapsed: input.elapsed,
            per_operation: input.per_operation,
            created_at: now,
        })
    }

    pub async fn insert_summary(
        &self,
        input: NewWorkflowSummary,
        now: OffsetDateTime,
    ) -> Result<WorkflowSummary, VoomError> {
        let mut tx = begin(&self.pool).await?;
        let out = self.insert_summary_in_tx(&mut tx, input, now).await?;
        commit(tx).await?;
        Ok(out)
    }

    pub async fn upsert_phase_summary_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewPhaseSummary,
        now: OffsetDateTime,
    ) -> Result<PhaseSummary, VoomError> {
        let created = iso8601(now)?;
        let (report_id, report_json) = match &input.report {
            Some(r) => (
                Some(r.report_id.clone()),
                Some(serialize_json(&r.report, "report")?),
            ),
            None => (None, None),
        };
        let res = sqlx::query(
            "INSERT INTO workflow_phase_summaries \
             (job_id, phase_ordinal, phase_name, report_id, report, outcome, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (job_id, phase_ordinal) DO NOTHING",
        )
        .bind(i64_from_u64(input.job_id.0))
        .bind(i64::from(input.phase_ordinal))
        .bind(&input.phase_name)
        .bind(report_id.as_deref())
        .bind(report_json.as_deref())
        .bind(input.outcome.as_str())
        .bind(&created)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("workflow_phase_summaries insert", e))?;

        if res.rows_affected() == 0 {
            return fetch_phase_by_key(&mut **tx, input.job_id, input.phase_ordinal)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "workflow_phase_summaries upsert: conflict row vanished \
                         job={} phase={}",
                        input.job_id, input.phase_ordinal
                    ))
                });
        }
        Ok(PhaseSummary {
            id: u64_from_i64(res.last_insert_rowid()),
            job_id: input.job_id,
            phase_ordinal: input.phase_ordinal,
            phase_name: input.phase_name,
            report: input.report,
            outcome: input.outcome,
            created_at: now,
        })
    }

    pub async fn upsert_phase_summary(
        &self,
        input: NewPhaseSummary,
        now: OffsetDateTime,
    ) -> Result<PhaseSummary, VoomError> {
        let mut tx = begin(&self.pool).await?;
        let out = self.upsert_phase_summary_in_tx(&mut tx, input, now).await?;
        commit(tx).await?;
        Ok(out)
    }

    pub async fn upsert_file_phase_summary_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewFilePhaseSummary,
        now: OffsetDateTime,
    ) -> Result<FilePhaseSummary, VoomError> {
        let created = iso8601(now)?;
        let ticket_ids = serialize_ticket_ids(&input.ticket_ids)?;
        // First-write-wins on (job_id, phase_ordinal, branch_id): the finalize
        // (§6) and resume (§8) backfill paths can re-issue this for an already-
        // recorded file, and that must be a no-op, not a UNIQUE error. This
        // relies on branch_id being unique per (job, phase) (see NewFilePhaseSummary).
        let res = sqlx::query(
            "INSERT INTO workflow_file_phase_summaries \
             (job_id, phase_ordinal, branch_id, ticket_ids, produced_file_version_id, \
              produced_file_location_id, artifact_handle_id, artifact_verification_id, \
              reprobe_snapshot_id, outcome, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (job_id, phase_ordinal, branch_id) DO NOTHING",
        )
        .bind(i64_from_u64(input.job_id.0))
        .bind(i64::from(input.phase_ordinal))
        .bind(&input.branch_id)
        .bind(&ticket_ids)
        .bind(input.produced_file_version_id.map(|i| i64_from_u64(i.0)))
        .bind(input.produced_file_location_id.map(|i| i64_from_u64(i.0)))
        .bind(input.artifact_handle_id.map(|i| i64_from_u64(i.0)))
        .bind(input.artifact_verification_id.map(|i| i64_from_u64(i.0)))
        .bind(input.reprobe_snapshot_id.map(|i| i64_from_u64(i.0)))
        .bind(input.outcome.as_str())
        .bind(&created)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("workflow_file_phase_summaries insert", e))?;

        if res.rows_affected() == 0 {
            return fetch_file_phase_by_key(
                &mut **tx,
                input.job_id,
                input.phase_ordinal,
                &input.branch_id,
            )
            .await?
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "workflow_file_phase_summaries upsert: conflict row vanished \
                     job={} phase={} branch={}",
                    input.job_id, input.phase_ordinal, input.branch_id
                ))
            });
        }
        Ok(FilePhaseSummary {
            id: u64_from_i64(res.last_insert_rowid()),
            job_id: input.job_id,
            phase_ordinal: input.phase_ordinal,
            branch_id: input.branch_id,
            ticket_ids: input.ticket_ids,
            produced_file_version_id: input.produced_file_version_id,
            produced_file_location_id: input.produced_file_location_id,
            artifact_handle_id: input.artifact_handle_id,
            artifact_verification_id: input.artifact_verification_id,
            reprobe_snapshot_id: input.reprobe_snapshot_id,
            outcome: input.outcome,
            created_at: now,
        })
    }

    pub async fn upsert_file_phase_summary(
        &self,
        input: NewFilePhaseSummary,
        now: OffsetDateTime,
    ) -> Result<FilePhaseSummary, VoomError> {
        let mut tx = begin(&self.pool).await?;
        let out = self
            .upsert_file_phase_summary_in_tx(&mut tx, input, now)
            .await?;
        commit(tx).await?;
        Ok(out)
    }

    pub async fn get_summary(&self, job_id: JobId) -> Result<Option<WorkflowSummary>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {SUMMARY_COLS} FROM workflow_summaries WHERE job_id = ?"
        ))
        .bind(i64_from_u64(job_id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workflow_summaries get", e))?;
        row.as_ref().map(row_to_summary).transpose()
    }

    pub async fn get_phase_summary(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
    ) -> Result<Option<PhaseSummary>, VoomError> {
        fetch_phase_by_key(&self.pool, job_id, phase_ordinal).await
    }

    pub async fn get_file_phase_summary(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        branch_id: &str,
    ) -> Result<Option<FilePhaseSummary>, VoomError> {
        fetch_file_phase_by_key(&self.pool, job_id, phase_ordinal, branch_id).await
    }

    pub async fn phases_for_job(&self, job_id: JobId) -> Result<Vec<PhaseSummary>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {PHASE_COLS} FROM workflow_phase_summaries \
             WHERE job_id = ? ORDER BY phase_ordinal ASC"
        ))
        .bind(i64_from_u64(job_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workflow_phase_summaries list", e))?;
        rows.iter().map(row_to_phase).collect()
    }

    pub async fn file_phases_for_job(
        &self,
        job_id: JobId,
    ) -> Result<Vec<FilePhaseSummary>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {FILE_PHASE_COLS} FROM workflow_file_phase_summaries \
             WHERE job_id = ? ORDER BY phase_ordinal ASC, branch_id ASC"
        ))
        .bind(i64_from_u64(job_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("workflow_file_phase_summaries list", e))?;
        rows.iter().map(row_to_file_phase).collect()
    }

    /// Inspect one job's immutable per-file starting cursors.
    ///
    /// # Errors
    /// Propagates repository reads and malformed persisted values.
    pub async fn file_run_starts_for_job(
        &self,
        job_id: JobId,
    ) -> Result<Vec<FileRunStart>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {FILE_RUN_START_COLS} FROM workflow_file_run_starts \
             WHERE job_id = ? ORDER BY branch_id ASC"
        ))
        .bind(i64_from_u64(job_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("workflow_file_run_starts list", error))?;
        rows.iter().map(row_to_file_run_start).collect()
    }

    /// Inspect one job's inherited per-file phase outcomes.
    ///
    /// # Errors
    /// Propagates repository reads and malformed persisted values.
    pub async fn file_run_history_for_job(
        &self,
        job_id: JobId,
    ) -> Result<Vec<FileRunHistory>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {FILE_RUN_HISTORY_COLS} FROM workflow_file_run_history \
             WHERE job_id = ? ORDER BY branch_id ASC, phase_ordinal ASC"
        ))
        .bind(i64_from_u64(job_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("workflow_file_run_history list", error))?;
        rows.iter().map(row_to_file_run_history).collect()
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
        .bind(i64_from_u64(job_id.0))
        .bind(branch_id)
        .fetch_optional(exec)
        .await
        .map_err(|error| VoomError::database_context("workflow file progress get", error))?;
    row.map(decode_file_progress).transpose()
}

async fn fetch_phase_by_key<'e, E>(
    exec: E,
    job_id: JobId,
    phase_ordinal: u32,
) -> Result<Option<PhaseSummary>, VoomError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let row = sqlx::query(&format!(
        "SELECT {PHASE_COLS} FROM workflow_phase_summaries \
         WHERE job_id = ? AND phase_ordinal = ?"
    ))
    .bind(i64_from_u64(job_id.0))
    .bind(i64::from(phase_ordinal))
    .fetch_optional(exec)
    .await
    .map_err(|e| VoomError::database_context("workflow_phase_summaries get", e))?;
    row.as_ref().map(row_to_phase).transpose()
}

async fn fetch_file_phase_by_key<'e, E>(
    exec: E,
    job_id: JobId,
    phase_ordinal: u32,
    branch_id: &str,
) -> Result<Option<FilePhaseSummary>, VoomError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let row = sqlx::query(&format!(
        "SELECT {FILE_PHASE_COLS} FROM workflow_file_phase_summaries \
         WHERE job_id = ? AND phase_ordinal = ? AND branch_id = ?"
    ))
    .bind(i64_from_u64(job_id.0))
    .bind(i64::from(phase_ordinal))
    .bind(branch_id)
    .fetch_optional(exec)
    .await
    .map_err(|e| VoomError::database_context("workflow_file_phase_summaries get", e))?;
    row.as_ref().map(row_to_file_phase).transpose()
}

fn elapsed_to_ns(elapsed: Duration) -> Result<i64, VoomError> {
    i64::try_from(elapsed.as_nanos())
        .map_err(|e| VoomError::database_context(format!("elapsed_ns overflow ({elapsed:?})"), e))
}

fn serialize_ticket_ids(ticket_ids: &[TicketId]) -> Result<String, VoomError> {
    let raw: Vec<u64> = ticket_ids.iter().map(|t| t.0).collect();
    serialize_json(&raw, "ticket_ids")
}

fn parse_json(s: &str, field: &'static str) -> Result<Value, VoomError> {
    serde_json::from_str(s).map_err(|e| VoomError::database_context(format!("parse {field}"), e))
}

fn opt_id<T>(
    row: &sqlx::sqlite::SqliteRow,
    col: &'static str,
    wrap: fn(u64) -> T,
) -> Result<Option<T>, VoomError> {
    let raw: Option<i64> = row
        .try_get(col)
        .map_err(|e| map_row_err("workflow_file_phase_summaries", &e))?;
    Ok(raw.map(|v| wrap(u64_from_i64(v))))
}

fn row_to_summary(row: &sqlx::sqlite::SqliteRow) -> Result<WorkflowSummary, VoomError> {
    let t = "workflow_summaries";
    let job_id: i64 = row.try_get("job_id").map_err(|e| map_row_err(t, &e))?;
    let branch_count: i64 = row
        .try_get("branch_count")
        .map_err(|e| map_row_err(t, &e))?;
    let ticket_count: i64 = row
        .try_get("ticket_count")
        .map_err(|e| map_row_err(t, &e))?;
    let dispatch_count: i64 = row
        .try_get("dispatch_count")
        .map_err(|e| map_row_err(t, &e))?;
    let retry_count: i64 = row.try_get("retry_count").map_err(|e| map_row_err(t, &e))?;
    let failure_count: i64 = row
        .try_get("failure_count")
        .map_err(|e| map_row_err(t, &e))?;
    let peak: i64 = row
        .try_get("peak_active_workflow_leases")
        .map_err(|e| map_row_err(t, &e))?;
    let elapsed_ns: i64 = row.try_get("elapsed_ns").map_err(|e| map_row_err(t, &e))?;
    let per_operation: String = row
        .try_get("per_operation")
        .map_err(|e| map_row_err(t, &e))?;
    let created: String = row.try_get("created_at").map_err(|e| map_row_err(t, &e))?;
    Ok(WorkflowSummary {
        job_id: JobId(u64_from_i64(job_id)),
        branch_count: u32_from_i64(branch_count)?,
        ticket_count: u32_from_i64(ticket_count)?,
        dispatch_count: u64_from_i64(dispatch_count),
        retry_count: u64_from_i64(retry_count),
        failure_count: u64_from_i64(failure_count),
        peak_active_workflow_leases: u32_from_i64(peak)?,
        elapsed: Duration::from_nanos(u64_from_i64(elapsed_ns)),
        per_operation: parse_json(&per_operation, "per_operation")?,
        created_at: parse_iso8601(&created)?,
    })
}

fn row_to_phase(row: &sqlx::sqlite::SqliteRow) -> Result<PhaseSummary, VoomError> {
    let t = "workflow_phase_summaries";
    let id: i64 = row.try_get("id").map_err(|e| map_row_err(t, &e))?;
    let job_id: i64 = row.try_get("job_id").map_err(|e| map_row_err(t, &e))?;
    let phase_ordinal: i64 = row
        .try_get("phase_ordinal")
        .map_err(|e| map_row_err(t, &e))?;
    let phase_name: String = row.try_get("phase_name").map_err(|e| map_row_err(t, &e))?;
    let report_id: Option<String> = row.try_get("report_id").map_err(|e| map_row_err(t, &e))?;
    let report: Option<String> = row.try_get("report").map_err(|e| map_row_err(t, &e))?;
    let outcome: String = row.try_get("outcome").map_err(|e| map_row_err(t, &e))?;
    let created: String = row.try_get("created_at").map_err(|e| map_row_err(t, &e))?;
    let report = match (report_id, report) {
        (Some(report_id), Some(report)) => Some(PhaseReport {
            report_id,
            report: parse_json(&report, "report")?,
        }),
        (None, None) => None,
        _ => {
            return Err(VoomError::database(format!(
                "{t}: report_id/report half-populated for id={id}"
            )));
        }
    };
    Ok(PhaseSummary {
        id: u64_from_i64(id),
        job_id: JobId(u64_from_i64(job_id)),
        phase_ordinal: u32_from_i64(phase_ordinal)?,
        phase_name,
        report,
        outcome: PhaseOutcome::parse(&outcome)?,
        created_at: parse_iso8601(&created)?,
    })
}

fn row_to_file_phase(row: &sqlx::sqlite::SqliteRow) -> Result<FilePhaseSummary, VoomError> {
    let t = "workflow_file_phase_summaries";
    let id: i64 = row.try_get("id").map_err(|e| map_row_err(t, &e))?;
    let job_id: i64 = row.try_get("job_id").map_err(|e| map_row_err(t, &e))?;
    let phase_ordinal: i64 = row
        .try_get("phase_ordinal")
        .map_err(|e| map_row_err(t, &e))?;
    let branch_id: String = row.try_get("branch_id").map_err(|e| map_row_err(t, &e))?;
    let ticket_ids: String = row.try_get("ticket_ids").map_err(|e| map_row_err(t, &e))?;
    let outcome: String = row.try_get("outcome").map_err(|e| map_row_err(t, &e))?;
    let created: String = row.try_get("created_at").map_err(|e| map_row_err(t, &e))?;
    let raw_tickets: Vec<u64> = serde_json::from_str(&ticket_ids).map_err(|e| {
        VoomError::database_context(format!("{t}: parse ticket_ids for id={id}"), e)
    })?;
    Ok(FilePhaseSummary {
        id: u64_from_i64(id),
        job_id: JobId(u64_from_i64(job_id)),
        phase_ordinal: u32_from_i64(phase_ordinal)?,
        branch_id,
        ticket_ids: raw_tickets.into_iter().map(TicketId).collect(),
        produced_file_version_id: opt_id(row, "produced_file_version_id", FileVersionId)?,
        produced_file_location_id: opt_id(row, "produced_file_location_id", FileLocationId)?,
        artifact_handle_id: opt_id(row, "artifact_handle_id", ArtifactHandleId)?,
        artifact_verification_id: opt_id(row, "artifact_verification_id", ArtifactVerificationId)?,
        reprobe_snapshot_id: opt_id(row, "reprobe_snapshot_id", MediaSnapshotId)?,
        outcome: FilePhaseOutcome::parse(&outcome, t)?,
        created_at: parse_iso8601(&created)?,
    })
}

fn row_to_file_run_start(row: &sqlx::sqlite::SqliteRow) -> Result<FileRunStart, VoomError> {
    let table = "workflow_file_run_starts";
    let job_id: i64 = row
        .try_get("job_id")
        .map_err(|error| map_row_err(table, &error))?;
    let branch_id: String = row
        .try_get("branch_id")
        .map_err(|error| map_row_err(table, &error))?;
    let version_id: i64 = row
        .try_get("starting_file_version_id")
        .map_err(|error| map_row_err(table, &error))?;
    let phase_ordinal: i64 = row
        .try_get("starting_phase_ordinal")
        .map_err(|error| map_row_err(table, &error))?;
    Ok(FileRunStart {
        job_id: JobId(u64_from_i64(job_id)),
        branch_id,
        starting_file_version_id: FileVersionId(u64_from_i64(version_id)),
        starting_phase_ordinal: u32_from_i64(phase_ordinal)?,
    })
}

fn row_to_file_run_history(row: &sqlx::sqlite::SqliteRow) -> Result<FileRunHistory, VoomError> {
    let table = "workflow_file_run_history";
    let job_id: i64 = row
        .try_get("job_id")
        .map_err(|error| map_row_err(table, &error))?;
    let branch_id: String = row
        .try_get("branch_id")
        .map_err(|error| map_row_err(table, &error))?;
    let phase_ordinal: i64 = row
        .try_get("phase_ordinal")
        .map_err(|error| map_row_err(table, &error))?;
    let outcome: String = row
        .try_get("outcome")
        .map_err(|error| map_row_err(table, &error))?;
    Ok(FileRunHistory {
        job_id: JobId(u64_from_i64(job_id)),
        branch_id,
        phase_ordinal: u32_from_i64(phase_ordinal)?,
        outcome: FilePhaseOutcome::parse(&outcome, table)?,
    })
}

#[cfg(test)]
#[path = "workflow_summaries_test.rs"]
mod tests;
