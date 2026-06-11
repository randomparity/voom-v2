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
    Skipped,
    Blocked,
}

impl FilePhaseOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Skipped => "skipped",
            Self::Blocked => "blocked",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "committed" => Ok(Self::Committed),
            "skipped" => Ok(Self::Skipped),
            "blocked" => Ok(Self::Blocked),
            other => Err(VoomError::database(format!(
                "workflow_file_phase_summaries.outcome {other:?} not in vocab"
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

/// Per-`(file, phase)` summary input. Produced references are `Some` only for a
/// `Committed` outcome (enforced by a DB CHECK).
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
    pub reprobe_snapshot_id: Option<MediaSnapshotId>,
    pub outcome: FilePhaseOutcome,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SqliteWorkflowSummaryRepo {
    pool: SqlitePool,
}

impl SqliteWorkflowSummaryRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteWorkflowSummaryRepo {}

const SUMMARY_COLS: &str = "job_id, branch_count, ticket_count, dispatch_count, retry_count, \
     failure_count, peak_active_workflow_leases, elapsed_ns, per_operation, created_at";

const PHASE_COLS: &str =
    "id, job_id, phase_ordinal, phase_name, report_id, report, outcome, created_at";

const FILE_PHASE_COLS: &str = "id, job_id, phase_ordinal, branch_id, ticket_ids, \
     produced_file_version_id, produced_file_location_id, artifact_handle_id, \
     reprobe_snapshot_id, outcome, created_at";

impl SqliteWorkflowSummaryRepo {
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
              produced_file_location_id, artifact_handle_id, reprobe_snapshot_id, outcome, \
              created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (job_id, phase_ordinal, branch_id) DO NOTHING",
        )
        .bind(i64_from_u64(input.job_id.0))
        .bind(i64::from(input.phase_ordinal))
        .bind(&input.branch_id)
        .bind(&ticket_ids)
        .bind(input.produced_file_version_id.map(|i| i64_from_u64(i.0)))
        .bind(input.produced_file_location_id.map(|i| i64_from_u64(i.0)))
        .bind(input.artifact_handle_id.map(|i| i64_from_u64(i.0)))
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
        reprobe_snapshot_id: opt_id(row, "reprobe_snapshot_id", MediaSnapshotId)?,
        outcome: FilePhaseOutcome::parse(&outcome)?,
        created_at: parse_iso8601(&created)?,
    })
}

#[cfg(test)]
#[path = "workflow_summaries_test.rs"]
mod tests;
