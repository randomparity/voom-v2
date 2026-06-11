//! Per-file/per-phase durable row writing and the small payload/sqlite helpers
//! the projection and promotion code share.
//!
//! Writes the per-`(file, phase)` summary rows as the loop advances, records the
//! files that committed inline before a dispatch failure, and finalizes the owned
//! job (succeeded or zero-phase) into a [`CoordinatorOutcome`].

use serde_json::Value;
use sqlx::Row;
use voom_core::{
    FileAssetId, FileLocationId, FileVersionId, JobId, MediaSnapshotId, TicketId, VoomError,
};
use voom_store::repo::identity::{FileVersion, IdentityRepo, MediaSnapshot};
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, NewFilePhaseSummary, PhaseSummary,
};

use crate::ControlPlane;
use crate::workflow::coordinator::planning::{job_grain_summary, zero_phase_summary};
use crate::workflow::coordinator::resume::active_version_with_snapshot;
use crate::workflow::coordinator::{
    CoordinatorError, CoordinatorOutcome, Disposition, PhaseDispatchFailure, PhaseFile,
};
use crate::workflow::plan::policy_bridge::policy_workflow_node_id;

/// The durable references a committed file-phase row requires (NOT NULL by DB
/// CHECK): the produced version, its live location, and its reprobe snapshot.
#[derive(Default)]
#[expect(
    clippy::struct_field_names,
    reason = "fields mirror the NewFilePhaseSummary produced_*/reprobe_* id columns"
)]
pub(super) struct ProducedRefs {
    file_version_id: Option<FileVersionId>,
    file_location_id: Option<FileLocationId>,
    reprobe_snapshot_id: Option<MediaSnapshotId>,
}

impl ProducedRefs {
    pub(super) async fn resolve(
        control_plane: &ControlPlane,
        tip: &FileVersion,
        snapshot: &MediaSnapshot,
    ) -> Result<Self, VoomError> {
        let location = control_plane
            .identity
            .list_live_file_locations_by_version(tip.id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                VoomError::Internal(format!("committed version {} has no live location", tip.id))
            })?;
        Ok(Self {
            file_version_id: Some(tip.id),
            file_location_id: Some(location.id),
            reprobe_snapshot_id: Some(snapshot.id),
        })
    }
}

/// A live local-path artifact location considered for promotion, with the asset
/// it belongs to (to test whether it is the chain tip).
pub(super) struct WorkingDirArtifact {
    pub(super) location_id: FileLocationId,
    pub(super) asset_id: FileAssetId,
    pub(super) value: String,
    pub(super) epoch: u64,
}

impl WorkingDirArtifact {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, VoomError> {
        let location_id: i64 = row
            .try_get("id")
            .map_err(|e| VoomError::database_context("promotion location id", e))?;
        let asset_id: i64 = row
            .try_get("file_asset_id")
            .map_err(|e| VoomError::database_context("promotion location asset", e))?;
        let value: String = row
            .try_get("value")
            .map_err(|e| VoomError::database_context("promotion location value", e))?;
        let epoch: i64 = row
            .try_get("epoch")
            .map_err(|e| VoomError::database_context("promotion location epoch", e))?;
        Ok(Self {
            location_id: FileLocationId(sqlite_u64(location_id, "promotion location id")?),
            asset_id: FileAssetId(sqlite_u64(asset_id, "promotion location asset id")?),
            value,
            epoch: sqlite_u64(epoch, "promotion location epoch")?,
        })
    }
}

pub(super) fn phase_ordinal(index: usize) -> Result<u32, VoomError> {
    u32::try_from(index).map_err(|e| VoomError::Internal(format!("phase ordinal overflow: {e}")))
}

pub(super) fn sqlite_u64(value: i64, field: &str) -> Result<u64, VoomError> {
    u64::try_from(value)
        .map_err(|e| VoomError::database_context(format!("{field} {value} does not fit u64"), e))
}

pub(super) fn sqlite_i64(value: u64, field: &str) -> Result<i64, VoomError> {
    i64::try_from(value).map_err(|e| {
        VoomError::database_context(format!("{field} {value} does not fit SQLite i64"), e)
    })
}

pub(super) fn first_stream_of_kind<'a>(payload: &'a Value, kind: &str) -> Option<&'a Value> {
    payload
        .get("streams")
        .and_then(Value::as_array)?
        .iter()
        .find(|stream| stream.get("kind").and_then(Value::as_str) == Some(kind))
}

pub(super) fn payload_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Snapshot dimensions arrive as JSON `u64`, but planner dimensions are `u32`.
pub(super) fn payload_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

impl ControlPlane {
    /// Succeed the owned job and write its job-grain summary, returning the
    /// completed [`CoordinatorOutcome`].
    pub(super) async fn finalize_succeeded_run(
        &self,
        job_id: JobId,
        last_run: Option<&crate::workflow::WorkflowRunSummary>,
        phases: Vec<PhaseSummary>,
        file_phases: Vec<FilePhaseSummary>,
    ) -> Result<CoordinatorOutcome, VoomError> {
        let now = self.clock().now();
        self.succeed_job(job_id, now).await?;
        let summary = self
            .workflow_summaries
            .insert_summary(job_grain_summary(job_id, last_run), now)
            .await?;
        Ok(CoordinatorOutcome {
            job_id,
            summary,
            phases,
            file_phases,
        })
    }

    pub(super) async fn ticket_result_location_ids(
        &self,
        job_id: JobId,
    ) -> Result<Vec<FileLocationId>, VoomError> {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT json_extract(result, '$.result_file_location_id') \
             FROM tickets \
             WHERE job_id = ? \
               AND state = 'succeeded' \
               AND result IS NOT NULL \
               AND json_type(result, '$.result_file_location_id') = 'integer' \
             ORDER BY id ASC",
        )
        .bind(sqlite_i64(job_id.0, "promotion job id")?)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("promotion ticket results", e))?;
        rows.into_iter()
            .map(|(id,)| sqlite_u64(id, "promotion ticket result location id"))
            .map(|result| result.map(FileLocationId))
            .collect()
    }

    /// Scoped live local-path chain-tip file locations, paired with their owning
    /// asset. The caller filters to those under a working dir after canonicalizing
    /// both sides so symlinked staging roots still match.
    pub(super) async fn working_dir_artifacts(
        &self,
        location_ids: &[FileLocationId],
    ) -> Result<Vec<WorkingDirArtifact>, VoomError> {
        if location_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = location_ids
            .iter()
            .map(|id| sqlite_i64(id.0, "promotion location id"))
            .collect::<Result<Vec<_>, _>>()?;
        let ids_json = serde_json::to_string(&ids)
            .map_err(|e| VoomError::Internal(format!("promotion location ids encode: {e}")))?;
        let rows = sqlx::query(
            "SELECT fl.id, fv.file_asset_id, fl.value, fl.epoch \
             FROM file_locations fl \
             JOIN file_versions fv ON fv.id = fl.file_version_id \
             WHERE fl.id IN (SELECT value FROM json_each(?)) \
               AND fl.retired_at IS NULL \
               AND fl.kind = 'local_path' \
               AND NOT EXISTS ( \
                   SELECT 1 FROM file_versions newer \
                   WHERE newer.file_asset_id = fv.file_asset_id \
                     AND newer.retired_at IS NULL \
                     AND newer.id > fv.id \
               ) \
             ORDER BY fl.id ASC",
        )
        .bind(ids_json)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("promotion scoped locations", e))?;
        let mut artifacts = Vec::with_capacity(rows.len());
        for row in rows {
            artifacts.push(WorkingDirArtifact::from_row(&row)?);
        }
        Ok(artifacts)
    }

    /// Finalize a run whose phase failed during dispatch: record every file that
    /// committed inline before the failure (the executor drained in-flight
    /// dispatches, so their commits have landed), then return the partial
    /// outcome inside the error. No phase-grain row is written for the failed
    /// phase, and the job is already `failed`.
    #[expect(
        clippy::too_many_arguments,
        reason = "threads the in-progress run's accumulated phase/file rows into the partial"
    )]
    pub(super) async fn finalize_failed_phase(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        files: &[PhaseFile],
        dispositions: &[Disposition],
        failure: PhaseDispatchFailure,
        phases: Vec<PhaseSummary>,
        mut file_phases: Vec<FilePhaseSummary>,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let Some(run_summary) = failure.run_summary else {
            // A pre-dispatch bridge failure ran no tickets, so nothing committed.
            return Err(failure.source.into());
        };
        for (file, disposition) in files.iter().zip(dispositions) {
            let Disposition::Planned { node_id } = disposition else {
                continue;
            };
            let (tip, snapshot) = active_version_with_snapshot(&self.identity, file.asset_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "committed file asset {} lost its snapshot",
                        file.asset_id
                    ))
                })?;
            if tip.id == file.version_id {
                continue;
            }
            let workflow_node_id = policy_workflow_node_id(node_id);
            let ticket_ids = self.ticket_ids_for_node(job_id, &workflow_node_id).await?;
            let produced = ProducedRefs::resolve(self, &tip, &snapshot).await?;
            let row = self
                .write_file_row(
                    job_id,
                    phase_ordinal,
                    file,
                    FilePhaseOutcome::Committed,
                    &ticket_ids,
                    Some(produced),
                )
                .await?;
            file_phases.push(row);
        }
        let summary = self
            .workflow_summaries
            .insert_summary(
                job_grain_summary(job_id, Some(&run_summary)),
                self.clock().now(),
            )
            .await?;
        Err(CoordinatorError {
            source: failure.source,
            partial: Some(CoordinatorOutcome {
                job_id,
                summary,
                phases,
                file_phases,
            }),
        })
    }

    /// Write each active file's per-`(file, phase)` row and advance the working
    /// set: drop blocked files, refresh committed files' chain tips. Returns the
    /// rows alongside each entered file's `(ordinal, refreshed snapshot)` — the
    /// in-hand inputs the regenerated per-phase report re-projects, so it needs
    /// no further database reads (ADR-0008).
    pub(super) async fn finalize_phase(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        files: &mut Vec<PhaseFile>,
        dispositions: &[Disposition],
    ) -> Result<(Vec<FilePhaseSummary>, Vec<(u32, MediaSnapshot)>), VoomError> {
        let mut rows = Vec::with_capacity(dispositions.len());
        let mut refreshed = Vec::with_capacity(dispositions.len());
        let mut survivors = Vec::with_capacity(files.len());
        for (file, disposition) in std::mem::take(files).into_iter().zip(dispositions) {
            let ordinal = file.ordinal;
            let (row, snapshot, keep) = self
                .finalize_file(job_id, phase_ordinal, file, disposition)
                .await?;
            rows.push(row);
            refreshed.push((ordinal, snapshot));
            if let Some(file) = keep {
                survivors.push(file);
            }
        }
        *files = survivors;
        Ok((rows, refreshed))
    }

    /// Resolve one file's outcome for a phase. Returns the summary row, the
    /// file's **refreshed** chain-tip snapshot (committed → the produced
    /// version's re-probe snapshot, otherwise unchanged) for the regenerated
    /// per-phase report, and the (possibly advanced) file if it stays active.
    async fn finalize_file(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        mut file: PhaseFile,
        disposition: &Disposition,
    ) -> Result<(FilePhaseSummary, MediaSnapshot, Option<PhaseFile>), VoomError> {
        match disposition {
            Disposition::Blocked => {
                let row = self
                    .write_file_row(
                        job_id,
                        phase_ordinal,
                        &file,
                        FilePhaseOutcome::Blocked,
                        &[],
                        None,
                    )
                    .await?;
                Ok((row, file.snapshot, None))
            }
            Disposition::Skipped => {
                let row = self
                    .write_file_row(
                        job_id,
                        phase_ordinal,
                        &file,
                        FilePhaseOutcome::Skipped,
                        &[],
                        None,
                    )
                    .await?;
                Ok((row, file.snapshot.clone(), Some(file)))
            }
            Disposition::Planned { node_id } => {
                let workflow_node_id = policy_workflow_node_id(node_id);
                let ticket_ids = self.ticket_ids_for_node(job_id, &workflow_node_id).await?;
                let (tip, snapshot) = active_version_with_snapshot(&self.identity, file.asset_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::Internal(format!(
                            "committed file asset {} lost its snapshot",
                            file.asset_id
                        ))
                    })?;
                if tip.id == file.version_id {
                    // Planned but the chain tip did not advance: no commit landed
                    // (e.g. a no-op transform). Record it as skipped, keep active.
                    let row = self
                        .write_file_row(
                            job_id,
                            phase_ordinal,
                            &file,
                            FilePhaseOutcome::Skipped,
                            &ticket_ids,
                            None,
                        )
                        .await?;
                    return Ok((row, file.snapshot.clone(), Some(file)));
                }
                let produced = ProducedRefs::resolve(self, &tip, &snapshot).await?;
                let row = self
                    .write_file_row(
                        job_id,
                        phase_ordinal,
                        &file,
                        FilePhaseOutcome::Committed,
                        &ticket_ids,
                        Some(produced),
                    )
                    .await?;
                file.version_id = tip.id;
                file.snapshot = snapshot;
                Ok((row, file.snapshot.clone(), Some(file)))
            }
        }
    }

    pub(super) async fn write_file_row(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        file: &PhaseFile,
        outcome: FilePhaseOutcome,
        ticket_ids: &[TicketId],
        produced: Option<ProducedRefs>,
    ) -> Result<FilePhaseSummary, VoomError> {
        let produced = produced.unwrap_or_default();
        self.workflow_summaries
            .upsert_file_phase_summary(
                NewFilePhaseSummary {
                    job_id,
                    phase_ordinal,
                    branch_id: file.branch_id.clone(),
                    ticket_ids: ticket_ids.to_vec(),
                    produced_file_version_id: produced.file_version_id,
                    produced_file_location_id: produced.file_location_id,
                    artifact_handle_id: None,
                    reprobe_snapshot_id: produced.reprobe_snapshot_id,
                    outcome,
                },
                self.clock().now(),
            )
            .await
    }

    /// Ticket ids whose payload `node_id` matches a workflow node, in id order.
    pub(super) async fn ticket_ids_for_node(
        &self,
        job_id: JobId,
        workflow_node_id: &str,
    ) -> Result<Vec<TicketId>, VoomError> {
        let rows = sqlx::query(
            "SELECT id FROM tickets \
             WHERE job_id = ? AND json_extract(payload, '$.node_id') = ? ORDER BY id ASC",
        )
        .bind(
            i64::try_from(job_id.0)
                .map_err(|e| VoomError::Internal(format!("job id exceeds SQLite integer: {e}")))?,
        )
        .bind(workflow_node_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("phase ticket ids", e))?;
        rows.into_iter()
            .map(|row| {
                let id: i64 = row
                    .try_get("id")
                    .map_err(|e| VoomError::database_context("phase ticket id", e))?;
                u64::try_from(id)
                    .map(TicketId)
                    .map_err(|e| VoomError::database_context("phase ticket id negative", e))
            })
            .collect()
    }

    /// Succeed the job and write a zero-count job-grain summary for a run with no
    /// active files or no declared phases (no work, no phase or file rows).
    pub(super) async fn finalize_zero_phase_run(
        &self,
        job_id: JobId,
        seed_file_phases: Vec<FilePhaseSummary>,
    ) -> Result<CoordinatorOutcome, VoomError> {
        let now = self.clock().now();
        self.succeed_job(job_id, now).await?;
        let summary = self
            .workflow_summaries
            .insert_summary(zero_phase_summary(job_id), now)
            .await?;
        Ok(CoordinatorOutcome {
            job_id,
            summary,
            phases: Vec::new(),
            file_phases: seed_file_phases,
        })
    }
}
