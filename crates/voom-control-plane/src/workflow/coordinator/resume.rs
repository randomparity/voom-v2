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
    FilePhaseOutcome, FilePhaseSummary, FileProgress, FileProgressState, FileRunHistory,
    FileRunStart, NewFileRunHistory, NewFileRunStart,
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
    pub(super) outcome: FilePhaseOutcome,
}

#[derive(Debug)]
pub(super) struct ResumePreparation {
    pub(super) files: Vec<PhaseFile>,
    pub(super) run_starts: Vec<NewFileRunStart>,
    pub(super) history: Vec<NewFileRunHistory>,
    pub(super) seeds: Vec<PreparedResumeSeed>,
    pub(super) max_in_flight_files: u32,
}

struct PriorBranch<'a> {
    start: &'a FileRunStart,
    progress: &'a FileProgress,
    rows: &'a [&'a FilePhaseSummary],
    inherited: &'a [&'a FileRunHistory],
}

struct PreparedResumeBranch {
    survivor: Option<PhaseFile>,
    run_start: NewFileRunStart,
    history: Vec<NewFileRunHistory>,
    seeds: Vec<PreparedResumeSeed>,
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
        let inherited = self
            .workflow_summaries
            .file_run_history_for_job(prior_job_id)
            .await?;
        let window = self
            .workflow_summaries
            .file_window(prior_job_id)
            .await?
            .ok_or_else(|| {
                resume_incomplete(format!("missing file window for job {prior_job_id}"))
            })?;
        let progress = self
            .workflow_summaries
            .file_progress_for_job(prior_job_id)
            .await?;
        validate_branch_sets(&files, &starts, &rows, &inherited, &progress)?;

        let starts = starts
            .into_iter()
            .map(|start| (start.branch_id.clone(), start))
            .collect::<BTreeMap<_, _>>();
        let progress = progress
            .into_iter()
            .map(|row| (row.branch_id.clone(), row))
            .collect::<BTreeMap<_, _>>();
        let (rows_by_branch, inherited_by_branch) = index_prior_rows(&rows, &inherited);
        let mut survivors = Vec::with_capacity(files.len());
        let mut run_starts = Vec::with_capacity(files.len());
        let mut history = Vec::new();
        let mut seeds = Vec::new();
        for file in files {
            let start = starts.get(&file.branch_id).ok_or_else(|| {
                resume_incomplete(format!("missing start for branch {}", file.branch_id))
            })?;
            let progress = progress.get(&file.branch_id).ok_or_else(|| {
                resume_incomplete(format!("missing progress for branch {}", file.branch_id))
            })?;
            let branch_rows = rows_by_branch
                .get(file.branch_id.as_str())
                .map_or(&[][..], Vec::as_slice);
            let inherited_rows = inherited_by_branch
                .get(file.branch_id.as_str())
                .map_or(&[][..], Vec::as_slice);
            let prepared = self
                .prepare_resume_branch(
                    prior_job_id,
                    file,
                    PriorBranch {
                        start,
                        progress,
                        rows: branch_rows,
                        inherited: inherited_rows,
                    },
                    phase_count,
                )
                .await?;
            run_starts.push(prepared.run_start);
            history.extend(prepared.history);
            seeds.extend(prepared.seeds);
            if let Some(file) = prepared.survivor {
                survivors.push(file);
            }
        }
        history.sort_by(|left, right| {
            left.branch_id
                .cmp(&right.branch_id)
                .then(left.phase_ordinal.cmp(&right.phase_ordinal))
        });
        Ok(ResumePreparation {
            files: survivors,
            run_starts,
            history,
            seeds,
            max_in_flight_files: window.max_in_flight_files,
        })
    }

    async fn prepare_resume_branch(
        &self,
        prior_job_id: JobId,
        mut file: PhaseFile,
        prior: PriorBranch<'_>,
        phase_count: u32,
    ) -> Result<PreparedResumeBranch, VoomError> {
        self.validate_resume_lineage(&file, prior.start, prior.rows)
            .await?;
        let state = validate_prior_row_shape(prior.start, prior.rows, phase_count)?;
        validate_prior_progress(prior.start, prior.progress, &state)?;
        let mut phase_history = validate_prior_history(prior.start, prior.inherited, phase_count)?;
        merge_prior_rows(&file.branch_id, &mut phase_history, prior.rows)?;
        let mut seeds = Vec::new();
        for row in prior.rows {
            if row.outcome == FilePhaseOutcome::Verified {
                seeds.push(PreparedResumeSeed {
                    phase_ordinal: row.phase_ordinal,
                    branch_id: file.branch_id.clone(),
                    produced: ProducedRefs::verified_seed(row)?,
                    outcome: FilePhaseOutcome::Verified,
                });
            }
        }
        let mut next_ordinal = state.next_ordinal;
        if prior.progress.state == FileProgressState::Terminal {
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
                outcome: FilePhaseOutcome::Committed,
            });
            merge_phase_outcome(
                &file.branch_id,
                &mut phase_history,
                next_ordinal,
                FilePhaseOutcome::Committed,
            )?;
            next_ordinal += 1;
        } else if let Some(produced) = self
            .unfinalized_verified_refs(prior_job_id, next_ordinal, &file)
            .await?
        {
            seeds.push(PreparedResumeSeed {
                phase_ordinal: next_ordinal,
                branch_id: file.branch_id.clone(),
                produced,
                outcome: FilePhaseOutcome::Verified,
            });
            merge_phase_outcome(
                &file.branch_id,
                &mut phase_history,
                next_ordinal,
                FilePhaseOutcome::Verified,
            )?;
            next_ordinal += 1;
        }
        let run_start = NewFileRunStart {
            branch_id: file.branch_id.clone(),
            starting_file_version_id: file.version_id,
            starting_phase_ordinal: if state.phase_complete {
                phase_count
            } else {
                next_ordinal
            },
        };
        let mut history = Vec::with_capacity(phase_history.len());
        for (&phase_ordinal, &outcome) in &phase_history {
            history.push(NewFileRunHistory {
                branch_id: file.branch_id.clone(),
                phase_ordinal,
                outcome,
            });
        }
        let survivor = (prior.progress.state != FileProgressState::Terminal).then(|| {
            file.resume_ordinal = if state.phase_complete {
                phase_count
            } else {
                next_ordinal
            };
            file.phase_history = phase_history;
            file
        });
        Ok(PreparedResumeBranch {
            survivor,
            run_start,
            history,
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
    phase_complete: bool,
}

type PhaseRowsByBranch<'a> = BTreeMap<&'a str, Vec<&'a FilePhaseSummary>>;
type HistoryRowsByBranch<'a> = BTreeMap<&'a str, Vec<&'a FileRunHistory>>;

fn index_prior_rows<'a>(
    rows: &'a [FilePhaseSummary],
    inherited: &'a [FileRunHistory],
) -> (PhaseRowsByBranch<'a>, HistoryRowsByBranch<'a>) {
    let mut rows_by_branch = PhaseRowsByBranch::new();
    for row in rows {
        rows_by_branch
            .entry(row.branch_id.as_str())
            .or_default()
            .push(row);
    }
    let mut inherited_by_branch = HistoryRowsByBranch::new();
    for row in inherited {
        inherited_by_branch
            .entry(row.branch_id.as_str())
            .or_default()
            .push(row);
    }
    (rows_by_branch, inherited_by_branch)
}

fn validate_branch_sets(
    files: &[PhaseFile],
    starts: &[FileRunStart],
    rows: &[FilePhaseSummary],
    inherited: &[FileRunHistory],
    progress: &[FileProgress],
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
    let progress_branches = progress
        .iter()
        .map(|row| row.branch_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if progress_branches != prior || progress.len() != starts.len() {
        return Err(resume_incomplete(format!(
            "progress branches {progress_branches:?} do not match prior starts {prior:?}"
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
    if let Some(row) = inherited
        .iter()
        .find(|row| !prior.contains(row.branch_id.as_str()))
    {
        return Err(resume_incomplete(format!(
            "inherited phase outcome references unmatched branch {}",
            row.branch_id
        )));
    }
    Ok(())
}

fn validate_prior_history(
    start: &FileRunStart,
    rows: &[&FileRunHistory],
    phase_count: u32,
) -> Result<BTreeMap<u32, FilePhaseOutcome>, VoomError> {
    let mut history = BTreeMap::new();
    for row in rows {
        if row.phase_ordinal >= start.starting_phase_ordinal || row.phase_ordinal >= phase_count {
            return Err(resume_incomplete(format!(
                "branch {} inherited phase {} is not before run start {} below {phase_count}",
                start.branch_id, row.phase_ordinal, start.starting_phase_ordinal
            )));
        }
        if row.outcome == FilePhaseOutcome::Blocked {
            return Err(resume_incomplete(format!(
                "branch {} inherited blocked phase {}",
                start.branch_id, row.phase_ordinal
            )));
        }
        merge_phase_outcome(
            &start.branch_id,
            &mut history,
            row.phase_ordinal,
            row.outcome,
        )?;
    }
    Ok(history)
}

fn merge_prior_rows(
    branch_id: &str,
    history: &mut BTreeMap<u32, FilePhaseOutcome>,
    rows: &[&FilePhaseSummary],
) -> Result<(), VoomError> {
    for row in rows {
        if row.outcome == FilePhaseOutcome::Blocked {
            continue;
        }
        merge_phase_outcome(branch_id, history, row.phase_ordinal, row.outcome)?;
    }
    Ok(())
}

fn merge_phase_outcome(
    branch_id: &str,
    history: &mut BTreeMap<u32, FilePhaseOutcome>,
    phase_ordinal: u32,
    outcome: FilePhaseOutcome,
) -> Result<(), VoomError> {
    if let Some(existing) = history.insert(phase_ordinal, outcome)
        && existing != outcome
    {
        return Err(resume_incomplete(format!(
            "branch {branch_id} has conflicting outcomes for phase {phase_ordinal}"
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
    while let Some(seed) = rows
        .get(index)
        .filter(|row| row.phase_ordinal < start.starting_phase_ordinal)
    {
        validate_seed_row(start, seed)?;
        index += 1;
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
    let phase_complete = rows
        .last()
        .is_some_and(|row| row.outcome == FilePhaseOutcome::Blocked)
        || next == phase_count;
    Ok(PriorBranchState {
        next_ordinal: next,
        recorded_tip,
        phase_complete,
    })
}

fn validate_prior_progress(
    start: &FileRunStart,
    progress: &FileProgress,
    state: &PriorBranchState,
) -> Result<(), VoomError> {
    if progress.next_phase_ordinal != state.next_ordinal {
        return Err(resume_incomplete(format!(
            "branch {} cursor {} disagrees with phase-row tail {}",
            start.branch_id, progress.next_phase_ordinal, state.next_ordinal
        )));
    }
    match progress.state {
        FileProgressState::Pending => {
            if state.next_ordinal != start.starting_phase_ordinal || state.phase_complete {
                return Err(resume_incomplete(format!(
                    "pending branch {} already has completed phase work",
                    start.branch_id
                )));
            }
        }
        FileProgressState::Active => {}
        FileProgressState::Terminalizing => {
            if !state.phase_complete {
                return Err(resume_incomplete(format!(
                    "terminalizing branch {} has incomplete phases",
                    start.branch_id
                )));
            }
        }
        FileProgressState::Terminal => {
            if !state.phase_complete {
                return Err(resume_incomplete(format!(
                    "terminal branch {} has incomplete phases",
                    start.branch_id
                )));
            }
        }
    }
    Ok(())
}

fn validate_seed_row(start: &FileRunStart, row: &FilePhaseSummary) -> Result<(), VoomError> {
    if !matches!(
        row.outcome,
        FilePhaseOutcome::Committed | FilePhaseOutcome::Verified
    ) || !row.ticket_ids.is_empty()
    {
        return Err(resume_incomplete(format!(
            "branch {} phase {} is not an advancing empty-ticket reconciliation seed",
            start.branch_id, row.phase_ordinal
        )));
    }
    Ok(())
}

fn resume_incomplete(detail: impl std::fmt::Display) -> VoomError {
    VoomError::PolicyExecution(format!("resume state is incomplete: {detail}"))
}
