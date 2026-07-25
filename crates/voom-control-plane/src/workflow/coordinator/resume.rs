//! Resume reconciliation and chain-tip/snapshot projection.
//!
//! Reconciles a new resume job against the most-recently-failed run's per-`(file,
//! phase)` rows (ADR-0009), derives stable per-file branch ids, and projects a
//! committed file version's reprobe snapshot into the planner input the next
//! phase plans against.

use std::collections::BTreeMap;

use voom_core::{FileVersionId, JobId, VoomError};
use voom_store::repo::identity::{FileLocationKind, IdentityRepo};
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, FileRunStart, NewFileRunStart,
};

use crate::ControlPlane;
use crate::workflow::coordinator::PhaseFile;
use crate::workflow::coordinator::finalize::ProducedRefs;
use crate::workflow::coordinator::promotion::ensure_unique_selected_branch_ids;
use crate::workflow::plan::expansion::branch_ids_from_paths;

#[derive(Debug)]
pub(super) struct PreparedResumeSeed {
    pub(super) phase_ordinal: u32,
    pub(super) branch_id: String,
    pub(super) produced: ProducedRefs,
}

#[derive(Debug)]
pub(super) struct ResumePreparation {
    pub(super) files: Vec<PhaseFile>,
    pub(super) run_starts: Vec<NewFileRunStart>,
    pub(super) seeds: Vec<PreparedResumeSeed>,
}

impl ControlPlane {
    /// Derive stable branch ids from the input set's selected versions,
    /// disambiguating colliding path stems while preserving stem-only ids for
    /// non-colliding paths. The selected source path stays stable when the
    /// active version advances to an artifact with a different filename.
    pub(super) async fn selected_branch_ids(
        &self,
        selected: &[FileVersionId],
    ) -> Result<Vec<(FileVersionId, String)>, VoomError> {
        let mut paths = Vec::with_capacity(selected.len());
        for &file_version_id in selected {
            paths.push((
                file_version_id,
                self.file_branch_path(file_version_id).await?,
            ));
        }
        let path_values = paths
            .iter()
            .map(|(_, path)| path.clone())
            .collect::<Vec<_>>();
        let branch_ids = branch_ids_from_paths(&path_values)?;
        let branch_ids = paths
            .into_iter()
            .zip(branch_ids)
            .map(|((file_version_id, _), branch_id)| (file_version_id, branch_id))
            .collect::<Vec<_>>();
        ensure_unique_selected_branch_ids(&branch_ids)?;
        Ok(branch_ids)
    }

    async fn file_branch_path(&self, file_version_id: FileVersionId) -> Result<String, VoomError> {
        let locations = self
            .identity
            .list_live_file_locations_by_version(file_version_id)
            .await?;
        let path = locations
            .iter()
            .find(|location| location.kind == FileLocationKind::LocalPath)
            .or_else(|| locations.first())
            .map(|location| location.value.clone())
            .ok_or_else(|| {
                VoomError::NotFound(format!(
                    "file version {file_version_id} has no live location to derive a branch id"
                ))
            })?;
        Ok(path)
    }

    /// Validate and reconcile a prior run without writing durable state.
    pub(super) async fn prepare_resume(
        &self,
        prior_job_id: JobId,
        files: Vec<PhaseFile>,
        phase_count: u32,
    ) -> Result<ResumePreparation, VoomError> {
        let starts = self
            .workflow_summaries
            .file_run_starts_for_job(prior_job_id)
            .await?;
        let rows = self
            .workflow_summaries
            .file_phases_for_job(prior_job_id)
            .await?;
        validate_branch_sets(&files, &starts, &rows)?;

        let starts = starts
            .into_iter()
            .map(|start| (start.branch_id.clone(), start))
            .collect::<BTreeMap<_, _>>();
        let mut rows_by_branch = BTreeMap::<&str, Vec<&FilePhaseSummary>>::new();
        for row in &rows {
            rows_by_branch
                .entry(row.branch_id.as_str())
                .or_default()
                .push(row);
        }
        let mut survivors = Vec::with_capacity(files.len());
        let mut run_starts = Vec::with_capacity(files.len());
        let mut seeds = Vec::new();
        for mut file in files {
            let start = starts.get(&file.branch_id).ok_or_else(|| {
                resume_incomplete(format!("missing start for branch {}", file.branch_id))
            })?;
            let branch_rows = rows_by_branch
                .get(file.branch_id.as_str())
                .map_or(&[][..], Vec::as_slice);
            self.validate_resume_lineage(&file, start, branch_rows)
                .await?;
            let state = validate_prior_row_shape(start, branch_rows, phase_count)?;
            let mut next_ordinal = state.next_ordinal;
            if state.terminal {
                if file.version_id != state.recorded_tip {
                    return Err(resume_incomplete(format!(
                        "terminal branch {} changed from version {} to {}",
                        file.branch_id, state.recorded_tip, file.version_id
                    )));
                }
                next_ordinal = phase_count;
            } else if file.version_id != state.recorded_tip {
                let produced = ProducedRefs::resolve(self, file.version_id, &file.snapshot).await?;
                seeds.push(PreparedResumeSeed {
                    phase_ordinal: next_ordinal,
                    branch_id: file.branch_id.clone(),
                    produced,
                });
                next_ordinal += 1;
            }
            run_starts.push(NewFileRunStart {
                branch_id: file.branch_id.clone(),
                starting_file_version_id: file.version_id,
                starting_phase_ordinal: next_ordinal,
            });
            if next_ordinal < phase_count && !state.terminal {
                file.resume_ordinal = next_ordinal;
                survivors.push(file);
            }
        }
        Ok(ResumePreparation {
            files: survivors,
            run_starts,
            seeds,
        })
    }

    async fn validate_resume_lineage(
        &self,
        file: &PhaseFile,
        start: &FileRunStart,
        rows: &[&FilePhaseSummary],
    ) -> Result<(), VoomError> {
        let starting_version = self
            .identity
            .get_file_version(start.starting_file_version_id)
            .await?
            .ok_or_else(|| {
                resume_incomplete(format!(
                    "branch {} starting version {} is missing",
                    file.branch_id, start.starting_file_version_id
                ))
            })?;
        if starting_version.file_asset_id != file.asset_id {
            return Err(resume_incomplete(format!(
                "branch {} starting version {} belongs to file asset {}, expected {}",
                file.branch_id,
                start.starting_file_version_id,
                starting_version.file_asset_id,
                file.asset_id
            )));
        }
        for row in rows {
            let Some(version_id) = row.produced_file_version_id else {
                continue;
            };
            let version = self
                .identity
                .get_file_version(version_id)
                .await?
                .ok_or_else(|| {
                    resume_incomplete(format!(
                        "branch {} phase {} produced missing version {version_id}",
                        file.branch_id, row.phase_ordinal
                    ))
                })?;
            if version.file_asset_id != file.asset_id {
                return Err(resume_incomplete(format!(
                    "branch {} phase {} produced version {} from file asset {}, expected {}",
                    file.branch_id,
                    row.phase_ordinal,
                    version_id,
                    version.file_asset_id,
                    file.asset_id
                )));
            }
        }
        Ok(())
    }
}

struct PriorBranchState {
    next_ordinal: u32,
    recorded_tip: FileVersionId,
    terminal: bool,
}

fn validate_branch_sets(
    files: &[PhaseFile],
    starts: &[FileRunStart],
    rows: &[FilePhaseSummary],
) -> Result<(), VoomError> {
    let current = files
        .iter()
        .map(|file| file.branch_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let prior = starts
        .iter()
        .map(|start| start.branch_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if current != prior {
        return Err(resume_incomplete(format!(
            "current branches {current:?} do not match prior starts {prior:?}"
        )));
    }
    if let Some(row) = rows
        .iter()
        .find(|row| !prior.contains(row.branch_id.as_str()))
    {
        return Err(resume_incomplete(format!(
            "phase row references unmatched branch {}",
            row.branch_id
        )));
    }
    Ok(())
}

fn validate_prior_row_shape(
    start: &FileRunStart,
    rows: &[&FilePhaseSummary],
    phase_count: u32,
) -> Result<PriorBranchState, VoomError> {
    if start.starting_phase_ordinal > phase_count {
        return Err(resume_incomplete(format!(
            "branch {} starts at phase {}, beyond phase count {phase_count}",
            start.branch_id, start.starting_phase_ordinal
        )));
    }
    let mut next = start.starting_phase_ordinal;
    let mut index = 0;
    if let Some(seed) = rows.first().filter(|row| {
        start.starting_phase_ordinal > 0
            && row.phase_ordinal.checked_add(1) == Some(start.starting_phase_ordinal)
    }) {
        validate_seed_row(start, seed)?;
        index = 1;
    }
    for (tail_index, row) in rows[index..].iter().enumerate() {
        if row.phase_ordinal >= phase_count || row.phase_ordinal != next {
            return Err(resume_incomplete(format!(
                "branch {} has invalid phase {} while expecting {next} below {phase_count}",
                start.branch_id, row.phase_ordinal
            )));
        }
        if row.outcome == FilePhaseOutcome::Blocked && tail_index + index + 1 != rows.len() {
            return Err(resume_incomplete(format!(
                "branch {} has rows after blocked phase {}",
                start.branch_id, row.phase_ordinal
            )));
        }
        next += 1;
    }
    let recorded_tip = rows
        .iter()
        .rev()
        .find_map(|row| row.produced_file_version_id)
        .unwrap_or(start.starting_file_version_id);
    let terminal = rows
        .last()
        .is_some_and(|row| row.outcome == FilePhaseOutcome::Blocked)
        || next == phase_count;
    Ok(PriorBranchState {
        next_ordinal: next,
        recorded_tip,
        terminal,
    })
}

fn validate_seed_row(start: &FileRunStart, row: &FilePhaseSummary) -> Result<(), VoomError> {
    if row.outcome != FilePhaseOutcome::Committed || !row.ticket_ids.is_empty() {
        return Err(resume_incomplete(format!(
            "branch {} phase {} is not a committed empty-ticket reconciliation seed",
            start.branch_id, row.phase_ordinal
        )));
    }
    Ok(())
}

fn resume_incomplete(detail: impl std::fmt::Display) -> VoomError {
    VoomError::PolicyExecution(format!("resume state is incomplete: {detail}"))
}
