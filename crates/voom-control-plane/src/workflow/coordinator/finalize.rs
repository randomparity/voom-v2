//! Per-file/per-phase durable row writing and the small payload/sqlite helpers
//! the projection and promotion code share.
//!
//! Writes the per-`(file, phase)` summary rows as the loop advances, records the
//! files that committed inline before a dispatch failure, and finalizes the owned
//! job (succeeded or zero-phase) into a [`CoordinatorOutcome`].

use sqlx::Row;
use voom_core::ids::ArtifactVerificationId;
use voom_core::{
    ArtifactHandleId, FileAssetId, FileLocationId, FileVersionId, JobId, MediaSnapshotId, TicketId,
    VoomError,
};
use voom_store::repo::identity::{IdentityRepo, MediaSnapshot};
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, NewFilePhaseSummary, PhaseSummary,
};

use crate::ControlPlane;
use crate::workflow::coordinator::planning::{job_grain_summary, zero_phase_summary};
use crate::workflow::coordinator::{
    CoordinatorError, CoordinatorOutcome, Disposition, PhaseDispatchFailure, PhaseFile,
};
use crate::workflow::plan::policy_bridge::policy_workflow_node_id;
use crate::workflow::ticket_results::PolicyVerificationTicketResult;

type VerifiedEvidenceRow = (
    i64,
    i64,
    String,
    String,
    i64,
    i64,
    String,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

/// The durable references a committed file-phase row requires (NOT NULL by DB
/// CHECK): the produced version, its live location, and its reprobe snapshot.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::struct_field_names,
    reason = "fields mirror the NewFilePhaseSummary produced_*/reprobe_* id columns"
)]
pub(super) struct ProducedRefs {
    file_version_id: Option<FileVersionId>,
    file_location_id: Option<FileLocationId>,
    artifact_handle_id: Option<ArtifactHandleId>,
    artifact_verification_id: Option<ArtifactVerificationId>,
    reprobe_snapshot_id: Option<MediaSnapshotId>,
}

impl ProducedRefs {
    pub(super) async fn resolve(
        control_plane: &ControlPlane,
        file_version_id: FileVersionId,
        snapshot: &MediaSnapshot,
    ) -> Result<Self, VoomError> {
        let location = control_plane
            .identity
            .list_live_file_locations_by_version(file_version_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "committed version {file_version_id} has no live location"
                ))
            })?;
        Ok(Self {
            file_version_id: Some(file_version_id),
            file_location_id: Some(location.id),
            artifact_handle_id: None,
            artifact_verification_id: None,
            reprobe_snapshot_id: Some(snapshot.id),
        })
    }

    pub(super) fn seed(
        self,
        job_id: JobId,
        phase_ordinal: u32,
        branch_id: String,
        outcome: FilePhaseOutcome,
    ) -> NewFilePhaseSummary {
        NewFilePhaseSummary {
            job_id,
            phase_ordinal,
            branch_id,
            ticket_ids: Vec::new(),
            produced_file_version_id: self.file_version_id,
            produced_file_location_id: self.file_location_id,
            artifact_handle_id: self.artifact_handle_id,
            artifact_verification_id: self.artifact_verification_id,
            reprobe_snapshot_id: self.reprobe_snapshot_id,
            outcome,
        }
    }

    pub(super) fn verified_seed(row: &FilePhaseSummary) -> Result<Self, VoomError> {
        if row.outcome != FilePhaseOutcome::Verified {
            return Err(VoomError::Internal(format!(
                "phase row {} is not verified",
                row.id
            )));
        }
        Ok(Self {
            file_version_id: row.produced_file_version_id,
            file_location_id: row.produced_file_location_id,
            artifact_handle_id: row.artifact_handle_id,
            artifact_verification_id: row.artifact_verification_id,
            reprobe_snapshot_id: row.reprobe_snapshot_id,
        })
    }

    fn verified(result: &PolicyVerificationTicketResult) -> Self {
        Self {
            file_version_id: Some(result.source_file_version_id),
            file_location_id: Some(result.source_location_id),
            artifact_handle_id: Some(result.artifact_handle_id),
            artifact_verification_id: Some(result.artifact_verification_id),
            reprobe_snapshot_id: Some(result.source_media_snapshot_id),
        }
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
        let ticket_ids: Vec<(i64,)> =
            sqlx::query_as("SELECT id FROM tickets WHERE job_id = ? ORDER BY id ASC")
                .bind(sqlite_i64(job_id.0, "promotion job id")?)
                .fetch_all(&self.pool)
                .await
                .map_err(|error| VoomError::database_context("promotion job tickets", error))?;
        let ticket_ids = ticket_ids
            .into_iter()
            .map(|(id,)| sqlite_u64(id, "promotion ticket id"))
            .map(|result| result.map(TicketId))
            .collect::<Result<Vec<_>, _>>()?;
        self.ticket_result_location_ids_for_tickets(&ticket_ids)
            .await
    }

    pub(super) async fn ticket_result_location_ids_for_tickets(
        &self,
        ticket_ids: &[TicketId],
    ) -> Result<Vec<FileLocationId>, VoomError> {
        if ticket_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ticket_ids = ticket_ids
            .iter()
            .map(|id| sqlite_i64(id.0, "promotion ticket id"))
            .collect::<Result<Vec<_>, _>>()?;
        let ticket_ids = serde_json::to_string(&ticket_ids)
            .map_err(|error| VoomError::Internal(format!("promotion tickets encode: {error}")))?;
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT result FROM tickets \
             WHERE id IN (SELECT value FROM json_each(?)) \
               AND state = 'succeeded' AND result IS NOT NULL \
             ORDER BY id ASC",
        )
        .bind(&ticket_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("promotion ticket results", e))?;
        let mut ids = Vec::new();
        for (result,) in rows {
            ids.extend(
                crate::workflow::ticket_results::result_location_ids(&result)?
                    .into_iter()
                    .map(FileLocationId),
            );
        }
        Ok(ids)
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
            let (tip, snapshot) = self
                .identity
                .get_active_version_with_snapshot(file.asset_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "committed file asset {} lost its snapshot",
                        file.asset_id
                    ))
                })?;
            let workflow_node_id = policy_workflow_node_id(node_id);
            let ticket_ids = self
                .ticket_ids_for_phase_node(job_id, phase_ordinal, &workflow_node_id)
                .await?;
            if let Some(verified) = self.verified_refs_for_tickets(file, &ticket_ids).await? {
                let row = self
                    .write_file_row(
                        job_id,
                        phase_ordinal,
                        file,
                        FilePhaseOutcome::Verified,
                        &ticket_ids,
                        Some(verified),
                    )
                    .await?;
                file_phases.push(row);
                continue;
            }
            if tip.id == file.version_id {
                continue;
            }
            let produced = ProducedRefs::resolve(self, tip.id, &snapshot).await?;
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
                file.phase_history
                    .insert(phase_ordinal, FilePhaseOutcome::Skipped);
                Ok((row, file.snapshot.clone(), Some(file)))
            }
            Disposition::Planned { node_id } => {
                let workflow_node_id = policy_workflow_node_id(node_id);
                let ticket_ids = self
                    .ticket_ids_for_phase_node(job_id, phase_ordinal, &workflow_node_id)
                    .await?;
                let (tip, snapshot) = self
                    .identity
                    .get_active_version_with_snapshot(file.asset_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::Internal(format!(
                            "committed file asset {} lost its snapshot",
                            file.asset_id
                        ))
                    })?;
                if tip.id == file.version_id {
                    if let Some(verified) =
                        self.verified_refs_for_tickets(&file, &ticket_ids).await?
                    {
                        let row = self
                            .write_file_row(
                                job_id,
                                phase_ordinal,
                                &file,
                                FilePhaseOutcome::Verified,
                                &ticket_ids,
                                Some(verified),
                            )
                            .await?;
                        file.phase_history
                            .insert(phase_ordinal, FilePhaseOutcome::Verified);
                        return Ok((row, file.snapshot.clone(), Some(file)));
                    }
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
                    file.phase_history
                        .insert(phase_ordinal, FilePhaseOutcome::Skipped);
                    return Ok((row, file.snapshot.clone(), Some(file)));
                }
                let produced = ProducedRefs::resolve(self, tip.id, &snapshot).await?;
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
                file.phase_history
                    .insert(phase_ordinal, FilePhaseOutcome::Committed);
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
                    artifact_handle_id: produced.artifact_handle_id,
                    artifact_verification_id: produced.artifact_verification_id,
                    reprobe_snapshot_id: produced.reprobe_snapshot_id,
                    outcome,
                },
                self.clock().now(),
            )
            .await
    }

    async fn verified_refs_for_tickets(
        &self,
        file: &PhaseFile,
        ticket_ids: &[TicketId],
    ) -> Result<Option<ProducedRefs>, VoomError> {
        if ticket_ids.is_empty() {
            return Ok(None);
        }
        let ticket_ids = ticket_ids
            .iter()
            .map(|id| sqlite_i64(id.0, "verification ticket id"))
            .collect::<Result<Vec<_>, _>>()?;
        let ids = serde_json::to_string(&ticket_ids).map_err(|error| {
            VoomError::Internal(format!("verification tickets encode: {error}"))
        })?;
        let results: Vec<(i64, String)> = sqlx::query_as(
            "SELECT id, result FROM tickets \
             WHERE id IN (SELECT value FROM json_each(?)) \
               AND state = 'succeeded' \
               AND json_extract(result, '$.status') = 'verified' \
             ORDER BY id",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("verified ticket results", error))?;
        let [(ticket_id, result)] = results.as_slice() else {
            if results.is_empty() {
                return Ok(None);
            }
            return Err(VoomError::Conflict(format!(
                "branch {} has {} successful verification ticket results",
                file.branch_id,
                results.len()
            )));
        };
        let ticket_id = TicketId(sqlite_u64(*ticket_id, "verification result ticket id")?);
        let result: PolicyVerificationTicketResult =
            serde_json::from_str(result).map_err(|error| {
                VoomError::database_context("verified workflow ticket result", error)
            })?;
        self.validate_verified_ticket_result(file, ticket_id, &result)
            .await?;
        Ok(Some(ProducedRefs::verified(&result)))
    }

    pub(super) async fn unfinalized_verified_refs(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        file: &PhaseFile,
    ) -> Result<Option<ProducedRefs>, VoomError> {
        let workflow_id = format!("workflow-{}-phase-{phase_ordinal}", job_id.0);
        let ticket_ids: Vec<TicketId> = sqlx::query_scalar(
            "SELECT id FROM tickets \
             WHERE job_id = ? AND state = 'succeeded' \
               AND kind = 'synthetic.workflow.operation.verify_artifact' \
               AND json_extract(payload, '$.workflow_id') = ? \
               AND json_extract(payload, '$.rendered_payload.source_file_version_id') = ? \
             ORDER BY id",
        )
        .bind(sqlite_i64(job_id.0, "verification recovery job id")?)
        .bind(workflow_id)
        .bind(sqlite_i64(
            file.version_id.0,
            "verification recovery file version id",
        )?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            VoomError::database_context("unfinalized verification ticket lookup", error)
        })?
        .into_iter()
        .map(|id: i64| sqlite_u64(id, "unfinalized verification ticket id").map(TicketId))
        .collect::<Result<Vec<_>, _>>()?;
        self.verified_refs_for_tickets(file, &ticket_ids).await
    }

    async fn validate_verified_ticket_result(
        &self,
        file: &PhaseFile,
        ticket_id: TicketId,
        result: &PolicyVerificationTicketResult,
    ) -> Result<(), VoomError> {
        if result.source_file_version_id != file.version_id
            || result.source_media_snapshot_id != file.snapshot.id
            || result.observed_size_bytes != Some(result.expected_size_bytes)
            || result.observed_checksum.as_deref() != Some(result.expected_checksum.as_str())
        {
            return Err(VoomError::Conflict(format!(
                "verified ticket result does not match branch {} selected facts",
                file.branch_id
            )));
        }
        let evidence: Option<VerifiedEvidenceRow> = sqlx::query_as(
            "SELECT v.artifact_handle_id, v.artifact_location_id, v.status, v.path, \
                    v.workflow_ticket_id, v.expected_size_bytes, v.expected_checksum, \
                    v.observed_size_bytes, v.observed_checksum, \
                    fl.file_version_id, fl.value \
             FROM artifact_verifications v \
             JOIN leases l \
               ON l.id = v.workflow_lease_id AND l.ticket_id = v.workflow_ticket_id \
             LEFT JOIN file_locations fl \
               ON fl.id = ? AND fl.retired_at IS NULL \
             WHERE v.id = ?",
        )
        .bind(sqlite_i64(
            result.source_location_id.0,
            "verified file location id",
        )?)
        .bind(sqlite_i64(
            result.artifact_verification_id.0,
            "artifact verification id",
        )?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("verified evidence lookup", error))?;
        let Some((
            handle_id,
            artifact_location_id,
            status,
            evidence_path,
            evidence_ticket_id,
            expected_size_bytes,
            expected_checksum,
            observed_size_bytes,
            observed_checksum,
            source_version_id,
            source_path,
        )) = evidence
        else {
            return Err(VoomError::NotFound(format!(
                "artifact_verification {}",
                result.artifact_verification_id
            )));
        };
        if sqlite_u64(handle_id, "verification artifact handle")? != result.artifact_handle_id.0
            || sqlite_u64(artifact_location_id, "verification artifact location")?
                != result.artifact_location_id.0
            || sqlite_u64(evidence_ticket_id, "verification workflow ticket")? != ticket_id.0
            || status != "succeeded"
            || evidence_path != result.path
            || sqlite_u64(expected_size_bytes, "verification expected size")?
                != result.expected_size_bytes
            || expected_checksum != result.expected_checksum
            || observed_size_bytes
                .map(|size| sqlite_u64(size, "verification observed size"))
                .transpose()?
                != result.observed_size_bytes
            || observed_checksum != result.observed_checksum
            || source_version_id
                .map(|id| sqlite_u64(id, "verified location version"))
                .transpose()?
                != Some(file.version_id.0)
            || source_path.as_deref() != Some(result.path.as_str())
        {
            return Err(VoomError::Conflict(format!(
                "artifact_verification {} does not match verified ticket result",
                result.artifact_verification_id
            )));
        }
        Ok(())
    }

    /// Ticket ids whose invocation and payload `node_id` match a phase node.
    pub(super) async fn ticket_ids_for_phase_node(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        workflow_node_id: &str,
    ) -> Result<Vec<TicketId>, VoomError> {
        let workflow_id = format!("workflow-{}-phase-{phase_ordinal}", job_id.0);
        let rows = sqlx::query(
            "SELECT id FROM tickets \
             WHERE job_id = ? AND json_extract(payload, '$.workflow_id') = ? \
               AND json_extract(payload, '$.node_id') = ? ORDER BY id ASC",
        )
        .bind(
            i64::try_from(job_id.0)
                .map_err(|e| VoomError::Internal(format!("job id exceeds SQLite integer: {e}")))?,
        )
        .bind(workflow_id)
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
