//! Per-file/per-phase durable row writing and the small payload/sqlite helpers
//! the projection and promotion code share.
//!
//! Writes the per-`(file, phase)` summary rows as the loop advances, records the
//! files that committed inline before a dispatch failure, and finalizes the owned
//! job (succeeded or zero-phase) into a [`CoordinatorOutcome`].

use std::collections::BTreeSet;

use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, FileAssetId, FileLocationId, FileVersionId, JobId, LeaseId, MediaSnapshotId,
    TicketId, TicketOperation, VoomError,
};
use voom_store::repo::execution::leases::LeaseState;
use voom_store::repo::execution::tickets::{TicketState, WorkflowPhaseScope};
use voom_store::repo::execution::workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, NewFilePhaseSummary, PhaseSummary,
};
use voom_store::repo::media::artifacts::{
    ArtifactCommitState, ArtifactVerificationStatus, CommittedTicketEvidence,
};
use voom_store::repo::media::identity::{
    FileLocationRepo, FileVersionRepo, MediaSnapshot, MediaSnapshotRepo,
};

use crate::ControlPlane;
use crate::workflow::coordinator::planning::{job_grain_summary, zero_phase_summary};
use crate::workflow::coordinator::{CoordinatorOutcome, Disposition, PhaseFile};
use crate::workflow::ticket_results::PolicyVerificationTicketResult;

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

struct JobProducedCommit {
    refs: ProducedRefs,
    snapshot: MediaSnapshot,
}

struct SameLineageCommit {
    ticket_id: TicketId,
    produced: JobProducedCommit,
}

enum CommitRelevance {
    OtherLineage,
    Sidecar,
    SameLineage,
}

#[expect(
    clippy::struct_field_names,
    reason = "fields intentionally mirror the shared durable operation-result keys"
)]
struct CommittedResultFields {
    job_id: JobId,
    ticket_id: TicketId,
    lease_id: LeaseId,
    source_file_version_id: FileVersionId,
    artifact_handle_id: ArtifactHandleId,
    verification_id: ArtifactVerificationId,
    commit_record_id: ArtifactCommitRecordId,
    result_file_version_id: FileVersionId,
    result_file_location_id: FileLocationId,
    result_media_snapshot_id: Option<MediaSnapshotId>,
}

impl ProducedRefs {
    pub(super) fn seed(
        self,
        job_id: JobId,
        phase_ordinal: u32,
        branch_id: String,
        ticket_ids: Vec<TicketId>,
        outcome: FilePhaseOutcome,
    ) -> NewFilePhaseSummary {
        NewFilePhaseSummary {
            job_id,
            phase_ordinal,
            branch_id,
            ticket_ids,
            produced_file_version_id: self.file_version_id,
            produced_file_location_id: self.file_location_id,
            artifact_handle_id: self.artifact_handle_id,
            artifact_verification_id: self.artifact_verification_id,
            reprobe_snapshot_id: self.reprobe_snapshot_id,
            outcome,
        }
    }

    pub(super) fn resume_seed(row: &FilePhaseSummary) -> Self {
        Self {
            file_version_id: row.produced_file_version_id,
            file_location_id: row.produced_file_location_id,
            artifact_handle_id: row.artifact_handle_id,
            artifact_verification_id: row.artifact_verification_id,
            reprobe_snapshot_id: row.reprobe_snapshot_id,
        }
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

impl CommittedResultFields {
    fn decode(ticket_id: TicketId, value: &serde_json::Value) -> Result<Option<Self>, VoomError> {
        let marker_fields = [
            "commit_record_id",
            "result_file_version_id",
            "result_file_location_id",
            "result_media_snapshot_id",
        ];
        if marker_fields.iter().all(|field| value.get(field).is_none()) {
            return Ok(None);
        }
        Ok(Some(Self {
            job_id: JobId(required_result_u64(value, ticket_id, "job_id")?),
            ticket_id: TicketId(required_result_u64(value, ticket_id, "ticket_id")?),
            lease_id: LeaseId(required_result_u64(value, ticket_id, "lease_id")?),
            source_file_version_id: FileVersionId(required_result_u64(
                value,
                ticket_id,
                "source_file_version_id",
            )?),
            artifact_handle_id: ArtifactHandleId(required_result_u64(
                value,
                ticket_id,
                "staged_artifact_handle_id",
            )?),
            verification_id: ArtifactVerificationId(required_result_u64(
                value,
                ticket_id,
                "verification_id",
            )?),
            commit_record_id: ArtifactCommitRecordId(required_result_u64(
                value,
                ticket_id,
                "commit_record_id",
            )?),
            result_file_version_id: FileVersionId(required_result_u64(
                value,
                ticket_id,
                "result_file_version_id",
            )?),
            result_file_location_id: FileLocationId(required_result_u64(
                value,
                ticket_id,
                "result_file_location_id",
            )?),
            result_media_snapshot_id: optional_result_u64(
                value,
                ticket_id,
                "result_media_snapshot_id",
            )?
            .map(MediaSnapshotId),
        }))
    }
}

trait CommittedEvidenceValidation {
    fn validate(
        &self,
        job_id: JobId,
        expected_asset_id: FileAssetId,
        result: &CommittedResultFields,
    ) -> Result<CommitRelevance, VoomError>;
}

impl CommittedEvidenceValidation for CommittedTicketEvidence {
    fn validate(
        &self,
        job_id: JobId,
        expected_asset_id: FileAssetId,
        result: &CommittedResultFields,
    ) -> Result<CommitRelevance, VoomError> {
        let ticket_id = self.ticket_id;
        require_evidence(self.ticket_job_id, Some(job_id), ticket_id, "ticket job")?;
        require_evidence(result.job_id, job_id, ticket_id, "result job")?;
        require_evidence(result.ticket_id, ticket_id, ticket_id, "result ticket")?;
        let source_asset_id =
            required_evidence(self.source_file_asset_id, ticket_id, "source file asset")?;
        if source_asset_id != expected_asset_id {
            return Ok(CommitRelevance::OtherLineage);
        }
        validate_commit_evidence(self, ticket_id, result)?;
        let asset_id =
            required_evidence(self.result_file_asset_id, ticket_id, "result file asset")?;
        if result.result_media_snapshot_id.is_some() {
            require_evidence(
                self.snapshot_file_version_id,
                Some(result.result_file_version_id),
                ticket_id,
                "snapshot version",
            )?;
        }
        if asset_id != expected_asset_id {
            return Ok(CommitRelevance::Sidecar);
        }
        result.result_media_snapshot_id.ok_or_else(|| {
            evidence_mismatch(ticket_id, "same-lineage result reprobe snapshot is missing")
        })?;
        Ok(CommitRelevance::SameLineage)
    }
}

fn validate_commit_evidence(
    evidence: &CommittedTicketEvidence,
    ticket_id: TicketId,
    result: &CommittedResultFields,
) -> Result<(), VoomError> {
    let commit = required_evidence(evidence.commit.as_ref(), ticket_id, "commit record")?;
    require_evidence(
        commit.id,
        result.commit_record_id,
        ticket_id,
        "commit record",
    )?;
    require_evidence(
        commit.artifact_handle_id,
        result.artifact_handle_id,
        ticket_id,
        "commit artifact",
    )?;
    require_evidence(
        commit.source_file_version_id,
        result.source_file_version_id,
        ticket_id,
        "commit source version",
    )?;
    require_evidence(
        commit.verification_id,
        result.verification_id,
        ticket_id,
        "commit verification",
    )?;
    require_evidence(
        commit.result_file_version_id,
        Some(result.result_file_version_id),
        ticket_id,
        "commit result version",
    )?;
    require_evidence(
        commit.result_file_location_id,
        Some(result.result_file_location_id),
        ticket_id,
        "commit result location",
    )?;
    require_evidence(
        commit.state,
        ArtifactCommitState::Committed,
        ticket_id,
        "commit state",
    )?;
    validate_verification_evidence(evidence, ticket_id, result)
}

fn validate_verification_evidence(
    evidence: &CommittedTicketEvidence,
    ticket_id: TicketId,
    result: &CommittedResultFields,
) -> Result<(), VoomError> {
    let verification =
        required_evidence(evidence.verification.as_ref(), ticket_id, "verification")?;
    require_evidence(
        verification.artifact_handle_id,
        result.artifact_handle_id,
        ticket_id,
        "verification artifact",
    )?;
    let lease = required_evidence(evidence.result_lease.as_ref(), ticket_id, "result lease")?;
    require_evidence(
        lease.ticket_id,
        result.ticket_id,
        ticket_id,
        "result lease ticket",
    )?;
    require_evidence(
        lease.state,
        LeaseState::Released,
        ticket_id,
        "result lease state",
    )?;
    match (
        verification.workflow_ticket_id,
        verification.workflow_lease_id,
    ) {
        (None, None) => {}
        (verification_ticket_id, verification_lease_id) => {
            require_evidence(
                verification_ticket_id,
                Some(result.ticket_id),
                ticket_id,
                "verification ticket",
            )?;
            require_evidence(
                verification_lease_id,
                Some(result.lease_id),
                ticket_id,
                "verification lease",
            )?;
        }
    }
    require_evidence(
        verification.status,
        ArtifactVerificationStatus::Succeeded,
        ticket_id,
        "verification status",
    )?;
    require_evidence(
        evidence.location_file_version_id,
        Some(result.result_file_version_id),
        ticket_id,
        "location version",
    )
}

fn required_evidence<T: Copy>(
    value: Option<T>,
    ticket_id: TicketId,
    field: &str,
) -> Result<T, VoomError> {
    value.ok_or_else(|| evidence_mismatch(ticket_id, &format!("{field} is missing")))
}

fn require_evidence<T: Copy + std::fmt::Debug + PartialEq>(
    actual: T,
    expected: T,
    ticket_id: TicketId,
    field: &str,
) -> Result<(), VoomError> {
    if actual == expected {
        return Ok(());
    }
    Err(evidence_mismatch(
        ticket_id,
        &format!("{field} mismatch: durable {actual:?}, result {expected:?}"),
    ))
}

fn evidence_mismatch(ticket_id: TicketId, detail: &str) -> VoomError {
    VoomError::Conflict(format!(
        "committed workflow ticket {ticket_id} evidence does not match: {detail}"
    ))
}

fn required_result_u64(
    value: &serde_json::Value,
    ticket_id: TicketId,
    field: &str,
) -> Result<u64, VoomError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            VoomError::database(format!(
                "committed ticket {ticket_id} result field `{field}` must be a positive integer"
            ))
        })
}

fn optional_result_u64(
    value: &serde_json::Value,
    ticket_id: TicketId,
    field: &str,
) -> Result<Option<u64>, VoomError> {
    let Some(value) = value.get(field) else {
        return Ok(None);
    };
    value
        .as_u64()
        .filter(|value| *value > 0)
        .map(Some)
        .ok_or_else(|| {
            VoomError::database(format!(
                "committed ticket {ticket_id} result field `{field}` must be a positive integer"
            ))
        })
}

fn latest_commit_index(
    candidates: &[SameLineageCommit],
    file: &PhaseFile,
) -> Result<usize, VoomError> {
    let latest_version_id = candidates
        .iter()
        .map(|candidate| candidate.produced.snapshot.file_version_id)
        .max()
        .ok_or_else(|| VoomError::Internal("latest commit requires a candidate".to_owned()))?;
    let matching = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.produced.snapshot.file_version_id == latest_version_id)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [index] = matching.as_slice() else {
        return Err(VoomError::Conflict(format!(
            "branch {} has {} commits for latest result version {latest_version_id}",
            file.branch_id,
            matching.len()
        )));
    };
    Ok(*index)
}

/// A live local-path artifact location considered for promotion, with the asset
/// it belongs to (to test whether it is the chain tip).
pub(super) struct WorkingDirArtifact {
    pub(super) location_id: FileLocationId,
    pub(super) asset_id: FileAssetId,
    pub(super) storage_root_id: voom_core::StorageRootId,
    pub(super) provider_relative_locator: voom_core::ProviderRelativeLocator,
    pub(super) epoch: u64,
}

pub(super) fn phase_ordinal(index: usize) -> Result<u32, VoomError> {
    u32::try_from(index).map_err(|e| VoomError::Internal(format!("phase ordinal overflow: {e}")))
}

fn phase_workflow_scope(job_id: JobId, phase_ordinal: u32) -> (String, String) {
    (
        format!("workflow-{}-phase-{phase_ordinal}", job_id.0),
        format!("workflow-{}-file-*-phase-{phase_ordinal}", job_id.0),
    )
}

async fn validate_carried_ticket_scope(
    control_plane: &ControlPlane,
    evidence: &CommittedTicketEvidence,
    ticket_job_id: JobId,
    row: &FilePhaseSummary,
) -> Result<(), VoomError> {
    let ticket_id = evidence.ticket_id;
    let payload_branch = evidence
        .ticket_payload
        .get("branch_id")
        .and_then(serde_json::Value::as_str)
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| evidence_mismatch(ticket_id, "payload branch is missing"))?;
    let workflow_id = evidence
        .ticket_payload
        .get("workflow_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| evidence_mismatch(ticket_id, "payload workflow id is missing"))?;
    let exact = format!("workflow-{}-phase-{}", ticket_job_id.0, row.phase_ordinal);
    let file_prefix = format!("workflow-{}-file-", ticket_job_id.0);
    let phase_suffix = format!("-phase-{}", row.phase_ordinal);
    if workflow_id == exact {
        require_evidence(
            payload_branch,
            row.branch_id.as_str(),
            ticket_id,
            "payload branch",
        )?;
        return Ok(());
    }
    let input_ordinal = workflow_id
        .strip_prefix(&file_prefix)
        .and_then(|suffix| suffix.strip_suffix(&phase_suffix))
        .and_then(|ordinal| ordinal.parse::<u32>().ok())
        .ok_or_else(|| {
            evidence_mismatch(
                ticket_id,
                "payload workflow phase does not match the carried row",
            )
        })?;
    let durable_branch = control_plane
        .workflow_progress
        .branch_for_input_ordinal(ticket_job_id, input_ordinal)
        .await?;
    if durable_branch.as_deref() != Some(row.branch_id.as_str()) {
        return Err(evidence_mismatch(
            ticket_id,
            "workflow file ordinal does not belong to the carried row branch",
        ));
    }
    Ok(())
}

fn committed_result_matches_row(result: &CommittedResultFields, row: &FilePhaseSummary) -> bool {
    row.produced_file_version_id == Some(result.result_file_version_id)
        && row.produced_file_location_id == Some(result.result_file_location_id)
        && row.artifact_handle_id == Some(result.artifact_handle_id)
        && row
            .artifact_verification_id
            .is_none_or(|id| id == result.verification_id)
        && row.reprobe_snapshot_id == result.result_media_snapshot_id
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

    /// Scoped live local-path chain-tip file locations, paired with their owning
    /// asset. The caller filters to those under a working dir after canonicalizing
    /// both sides so symlinked staging roots still match.
    pub(super) async fn working_dir_artifacts(
        &self,
        location_ids: &[FileLocationId],
    ) -> Result<Vec<WorkingDirArtifact>, VoomError> {
        self.identity
            .live_local_chain_tips(location_ids)
            .await
            .map(|locations| {
                locations
                    .into_iter()
                    .map(|location| WorkingDirArtifact {
                        location_id: location.location_id,
                        asset_id: location.file_asset_id,
                        storage_root_id: location.storage_root_id,
                        provider_relative_locator: location.provider_relative_locator,
                        epoch: location.epoch,
                    })
                    .collect()
            })
    }

    /// Finalize a run whose phase failed during dispatch: record every file that
    /// committed inline before the failure (the executor drained in-flight
    /// dispatches, so their commits have landed), then return the partial
    pub(super) async fn finalize_failed_file_phase(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        file: &PhaseFile,
        disposition: &Disposition,
    ) -> Result<Option<FilePhaseSummary>, VoomError> {
        let Disposition::Planned { .. } = disposition else {
            return Ok(None);
        };
        let ticket_ids = self
            .ticket_ids_for_phase_file(job_id, phase_ordinal, file.version_id)
            .await?;
        if let Some(verified) = self.verified_refs_for_tickets(file, &ticket_ids).await? {
            return self
                .write_file_row_and_advance(
                    job_id,
                    phase_ordinal,
                    file,
                    FilePhaseOutcome::Verified,
                    &ticket_ids,
                    Some(verified),
                )
                .await
                .map(Some);
        }
        let phase_ticket_ids = self.ticket_ids_for_phase(job_id, phase_ordinal).await?;
        let (produced, scoped_ticket_ids) = self
            .committed_refs_for_tickets(job_id, file, &phase_ticket_ids, &ticket_ids)
            .await?;
        let Some(produced) = produced else {
            return Ok(None);
        };
        self.write_file_row_and_advance(
            job_id,
            phase_ordinal,
            file,
            FilePhaseOutcome::Committed,
            &scoped_ticket_ids,
            Some(produced.refs),
        )
        .await
        .map(Some)
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
                    .write_file_row_and_advance(
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
                    .write_file_row_and_advance(
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
            Disposition::Planned { .. } => {
                let ticket_ids = self
                    .ticket_ids_for_phase_file(job_id, phase_ordinal, file.version_id)
                    .await?;
                if let Some(verified) = self.verified_refs_for_tickets(&file, &ticket_ids).await? {
                    let row = self
                        .write_file_row_and_advance(
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
                let phase_ticket_ids = self.ticket_ids_for_phase(job_id, phase_ordinal).await?;
                let (produced, scoped_ticket_ids) = self
                    .committed_refs_for_tickets(job_id, &file, &phase_ticket_ids, &ticket_ids)
                    .await?;
                let Some(produced) = produced else {
                    self.require_selected_version_still_active(&file).await?;
                    let row = self
                        .write_file_row_and_advance(
                            job_id,
                            phase_ordinal,
                            &file,
                            FilePhaseOutcome::Skipped,
                            &scoped_ticket_ids,
                            None,
                        )
                        .await?;
                    file.phase_history
                        .insert(phase_ordinal, FilePhaseOutcome::Skipped);
                    return Ok((row, file.snapshot.clone(), Some(file)));
                };
                let row = self
                    .write_file_row_and_advance(
                        job_id,
                        phase_ordinal,
                        &file,
                        FilePhaseOutcome::Committed,
                        &scoped_ticket_ids,
                        Some(produced.refs),
                    )
                    .await?;
                file.version_id = produced.snapshot.file_version_id;
                file.snapshot = produced.snapshot;
                file.phase_history
                    .insert(phase_ordinal, FilePhaseOutcome::Committed);
                Ok((row, file.snapshot.clone(), Some(file)))
            }
        }
    }

    async fn write_file_row_and_advance(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        file: &PhaseFile,
        outcome: FilePhaseOutcome,
        ticket_ids: &[TicketId],
        produced: Option<ProducedRefs>,
    ) -> Result<FilePhaseSummary, VoomError> {
        let produced = produced.unwrap_or_default();
        self.workflow_progress
            .upsert_file_phase_summary_and_advance(
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
                phase_ordinal,
                phase_ordinal + 1,
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
        let mut results = Vec::new();
        for ticket_id in ticket_ids {
            let Some(ticket) = self.tickets.get(*ticket_id).await? else {
                continue;
            };
            if ticket.state == TicketState::Succeeded
                && ticket
                    .result
                    .as_ref()
                    .and_then(|result| result.get("status"))
                    .and_then(serde_json::Value::as_str)
                    == Some("verified")
            {
                let result = ticket.result.ok_or_else(|| {
                    VoomError::Internal("verified ticket lost its selected result".to_owned())
                })?;
                results.push((ticket.id, result));
            }
        }
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
        let result: PolicyVerificationTicketResult = serde_json::from_value(result.clone())
            .map_err(|error| {
                VoomError::database_context("verified workflow ticket result", error)
            })?;
        self.validate_verified_ticket_result(file, *ticket_id, &result)
            .await?;
        Ok(Some(ProducedRefs::verified(&result)))
    }

    async fn committed_refs_for_tickets(
        &self,
        job_id: JobId,
        file: &PhaseFile,
        ticket_ids: &[TicketId],
        seed_ticket_ids: &[TicketId],
    ) -> Result<(Option<JobProducedCommit>, Vec<TicketId>), VoomError> {
        if ticket_ids.is_empty() {
            return Ok((None, seed_ticket_ids.to_vec()));
        }
        let rows = self.committed_evidence_for_tickets(ticket_ids).await?;
        let mut scoped_ticket_ids = seed_ticket_ids.to_vec();
        let mut candidates = Vec::new();
        for evidence in rows {
            let ticket_id = evidence.ticket_id;
            let Some(result) = CommittedResultFields::decode(ticket_id, &evidence.result)? else {
                continue;
            };
            match evidence.validate(job_id, file.asset_id, &result)? {
                CommitRelevance::OtherLineage => {}
                CommitRelevance::Sidecar => scoped_ticket_ids.push(ticket_id),
                CommitRelevance::SameLineage => {
                    candidates.push(SameLineageCommit {
                        ticket_id,
                        produced: self.job_produced_commit(result).await?,
                    });
                }
            }
        }
        scoped_ticket_ids.extend(candidates.iter().map(|candidate| candidate.ticket_id));
        let produced = if candidates.is_empty() {
            None
        } else {
            let index = latest_commit_index(&candidates, file)?;
            Some(candidates.swap_remove(index).produced)
        };
        scoped_ticket_ids.sort_unstable_by_key(|id| id.0);
        scoped_ticket_ids.dedup();
        Ok((produced, scoped_ticket_ids))
    }

    async fn committed_evidence_for_tickets(
        &self,
        ticket_ids: &[TicketId],
    ) -> Result<Vec<CommittedTicketEvidence>, VoomError> {
        self.artifacts.committed_ticket_evidence(ticket_ids).await
    }

    pub(super) async fn validated_committed_location_ids_for_rows(
        &self,
        rows: &[FilePhaseSummary],
    ) -> Result<Vec<FileLocationId>, VoomError> {
        let mut locations = Vec::new();
        for row in rows {
            locations.extend(self.validated_committed_locations_for_row(row).await?);
        }
        Ok(locations)
    }

    async fn validated_committed_locations_for_row(
        &self,
        row: &FilePhaseSummary,
    ) -> Result<Vec<FileLocationId>, VoomError> {
        if row.ticket_ids.is_empty() {
            if row.outcome == FilePhaseOutcome::Committed {
                return Err(VoomError::Conflict(format!(
                    "branch {} phase {} committed row has no ticket evidence",
                    row.branch_id, row.phase_ordinal
                )));
            }
            return Ok(Vec::new());
        }
        let expected_asset_id = self.file_run_asset_id(row).await?;
        let evidence_rows = self.committed_evidence_for_tickets(&row.ticket_ids).await?;
        let mut locations = Vec::new();
        let mut matched_row = false;
        for evidence in evidence_rows {
            let ticket_id = evidence.ticket_id;
            let Some(result) = CommittedResultFields::decode(ticket_id, &evidence.result)? else {
                continue;
            };
            let ticket_job_id = required_evidence(evidence.ticket_job_id, ticket_id, "ticket job")?;
            validate_carried_ticket_scope(self, &evidence, ticket_job_id, row).await?;
            match evidence.validate(ticket_job_id, expected_asset_id, &result)? {
                CommitRelevance::OtherLineage => {
                    return Err(evidence_mismatch(
                        ticket_id,
                        "cleanup ticket belongs to another source lineage",
                    ));
                }
                CommitRelevance::Sidecar => {
                    locations.push(result.result_file_location_id);
                }
                CommitRelevance::SameLineage => {
                    let tip_id = row.produced_file_version_id.ok_or_else(|| {
                        evidence_mismatch(
                            ticket_id,
                            "same-lineage commit is absent from the carried row",
                        )
                    })?;
                    let source_id = result.source_file_version_id;
                    if !self.version_descends_from(tip_id, source_id).await? {
                        return Err(evidence_mismatch(
                            ticket_id,
                            "cleanup source is outside the carried row chain",
                        ));
                    }
                    matched_row |= committed_result_matches_row(&result, row);
                    locations.push(result.result_file_location_id);
                }
            }
        }
        if row.outcome == FilePhaseOutcome::Committed && !matched_row {
            return Err(VoomError::Conflict(format!(
                "branch {} phase {} ticket evidence does not produce the recorded row",
                row.branch_id, row.phase_ordinal
            )));
        }
        Ok(locations)
    }

    async fn file_run_asset_id(&self, row: &FilePhaseSummary) -> Result<FileAssetId, VoomError> {
        self.workflow_summaries
            .file_run_asset_id(row.job_id, &row.branch_id)
            .await?
            .ok_or_else(|| {
                VoomError::Conflict(format!(
                    "branch {} phase {} has no durable file-run start",
                    row.branch_id, row.phase_ordinal
                ))
            })
    }

    async fn version_descends_from(
        &self,
        mut tip_id: FileVersionId,
        ancestor_id: FileVersionId,
    ) -> Result<bool, VoomError> {
        let mut visited = BTreeSet::new();
        loop {
            if tip_id == ancestor_id {
                return Ok(true);
            }
            if !visited.insert(tip_id) {
                return Err(VoomError::Conflict(format!(
                    "file version lineage contains a cycle at {tip_id}"
                )));
            }
            let Some(version) = self.identity.get_file_version(tip_id).await? else {
                return Ok(false);
            };
            let Some(parent_id) = version.produced_from_version_id else {
                return Ok(false);
            };
            tip_id = parent_id;
        }
    }

    pub(super) async fn unfinalized_committed_refs(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        file: &PhaseFile,
    ) -> Result<Option<(ProducedRefs, Vec<TicketId>)>, VoomError> {
        let ticket_ids = self.ticket_ids_for_phase(job_id, phase_ordinal).await?;
        let (produced, scoped_ticket_ids) = self
            .committed_refs_for_tickets(job_id, file, &ticket_ids, &[])
            .await?;
        let Some(produced) = produced else {
            return Ok(None);
        };
        if produced.snapshot.file_version_id != file.version_id {
            return Err(VoomError::Conflict(format!(
                "prior job {job_id} phase {phase_ordinal} committed version {}, \
                 but branch {} currently points at {}",
                produced.snapshot.file_version_id, file.branch_id, file.version_id
            )));
        }
        Ok(Some((produced.refs, scoped_ticket_ids)))
    }

    async fn job_produced_commit(
        &self,
        result: CommittedResultFields,
    ) -> Result<JobProducedCommit, VoomError> {
        let snapshot_id = result.result_media_snapshot_id.ok_or_else(|| {
            VoomError::Internal("validated same-lineage commit lost its snapshot id".to_owned())
        })?;
        let snapshot = self
            .identity
            .get_media_snapshot(snapshot_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("media_snapshot {snapshot_id}")))?;
        let file_version_id = result.result_file_version_id;
        if snapshot.file_version_id != file_version_id {
            return Err(VoomError::Conflict(format!(
                "committed result snapshot {snapshot_id} does not belong to {file_version_id}"
            )));
        }
        Ok(JobProducedCommit {
            refs: ProducedRefs {
                file_version_id: Some(file_version_id),
                file_location_id: Some(result.result_file_location_id),
                artifact_handle_id: Some(result.artifact_handle_id),
                artifact_verification_id: None,
                reprobe_snapshot_id: Some(snapshot_id),
            },
            snapshot,
        })
    }

    async fn require_selected_version_still_active(
        &self,
        file: &PhaseFile,
    ) -> Result<(), VoomError> {
        let mut tx = crate::cases::begin_tx(&self.pool).await?;
        self.identity
            .require_active_file_versions_in_tx(&mut tx, &[(file.asset_id, file.version_id)])
            .await?;
        crate::cases::commit_tx(tx).await
    }

    pub(super) async fn unfinalized_verified_refs(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        file: &PhaseFile,
    ) -> Result<Option<ProducedRefs>, VoomError> {
        let (workflow_id, workflow_pattern) = phase_workflow_scope(job_id, phase_ordinal);
        let scope = WorkflowPhaseScope {
            job_id,
            exact_workflow_id: &workflow_id,
            file_workflow_pattern: &workflow_pattern,
        };
        let ticket_ids = self
            .tickets
            .succeeded_ticket_ids_for_workflow_phase_file_and_operation(
                &scope,
                file.version_id,
                TicketOperation::new("synthetic.workflow.operation.verify_artifact")?,
            )
            .await?;
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
        let evidence = self
            .artifacts
            .verified_ticket_evidence(
                ticket_id,
                result.artifact_verification_id,
                result.artifact_handle_id,
                result.source_location_id,
            )
            .await?;
        let Some(evidence) = evidence else {
            return Err(VoomError::NotFound(format!(
                "artifact_verification {}",
                result.artifact_verification_id
            )));
        };
        let evidence_path = match (
            evidence.storage_root_id,
            evidence.provider_relative_locator.as_ref(),
        ) {
            (Some(storage_root_id), Some(relative_locator)) => {
                crate::operation_source::resolve_root_relative_existing_path(
                    self,
                    "workflow verification resume",
                    storage_root_id,
                    relative_locator,
                )
                .await?
            }
            _ => {
                return Err(VoomError::Conflict(format!(
                    "source file_location {} has no rooted address",
                    result.source_location_id
                )));
            }
        };
        let verification = evidence.verification;
        if verification.id != result.artifact_verification_id
            || verification.artifact_handle_id != result.artifact_handle_id
            || verification.artifact_location_id != result.artifact_location_id
            || verification.workflow_ticket_id != Some(ticket_id)
            || verification.status != ArtifactVerificationStatus::Succeeded
            || verification.path != result.path
            || verification.expected_size_bytes != result.expected_size_bytes
            || verification.expected_checksum != result.expected_checksum
            || verification.observed_size_bytes != result.observed_size_bytes
            || verification.observed_checksum != result.observed_checksum
            || evidence.file_version_id != Some(file.version_id)
            || evidence_path.to_string_lossy() != result.path
        {
            return Err(VoomError::Conflict(format!(
                "artifact_verification {} does not match verified ticket result",
                result.artifact_verification_id
            )));
        }
        Ok(())
    }

    /// Every ticket id in one job-owned phase invocation.
    async fn ticket_ids_for_phase(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
    ) -> Result<Vec<TicketId>, VoomError> {
        let (workflow_id, workflow_pattern) = phase_workflow_scope(job_id, phase_ordinal);
        self.tickets
            .ticket_ids_for_workflow_phase(&WorkflowPhaseScope {
                job_id,
                exact_workflow_id: &workflow_id,
                file_workflow_pattern: &workflow_pattern,
            })
            .await
    }

    /// Ticket ids whose invocation and payload `node_id` match a phase node.
    pub(super) async fn ticket_ids_for_phase_node(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        workflow_node_id: &str,
        file_version_id: FileVersionId,
    ) -> Result<Vec<TicketId>, VoomError> {
        self.ticket_ids_for_phase_scope(
            job_id,
            phase_ordinal,
            workflow_node_id,
            Some(file_version_id),
        )
        .await
    }

    async fn ticket_ids_for_phase_file(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        file_version_id: FileVersionId,
    ) -> Result<Vec<TicketId>, VoomError> {
        let (workflow_id, workflow_pattern) = phase_workflow_scope(job_id, phase_ordinal);
        self.tickets
            .ticket_ids_for_workflow_phase_file(
                &WorkflowPhaseScope {
                    job_id,
                    exact_workflow_id: &workflow_id,
                    file_workflow_pattern: &workflow_pattern,
                },
                file_version_id,
            )
            .await
    }

    pub(super) async fn ticket_ids_for_phase_scope(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        workflow_node_id: &str,
        file_version_id: Option<FileVersionId>,
    ) -> Result<Vec<TicketId>, VoomError> {
        let (workflow_id, workflow_pattern) = phase_workflow_scope(job_id, phase_ordinal);
        self.tickets
            .ticket_ids_for_workflow_phase_scope(
                &WorkflowPhaseScope {
                    job_id,
                    exact_workflow_id: &workflow_id,
                    file_workflow_pattern: &workflow_pattern,
                },
                workflow_node_id,
                file_version_id,
            )
            .await
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

#[cfg(test)]
#[path = "finalize_test.rs"]
mod tests;
