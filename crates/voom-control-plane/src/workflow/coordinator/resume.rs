//! Resume reconciliation and chain-tip/snapshot projection.
//!
//! Reconciles a new resume job against the most-recently-failed run's per-`(file,
//! phase)` rows (ADR-0009), derives stable per-file branch ids, and projects a
//! committed file version's reprobe snapshot into the planner input the next
//! phase plans against.

use std::collections::BTreeMap;

use voom_core::{FileVersionId, JobId, TicketId, VoomError};
use voom_store::repo::execution::workflow_progress::{
    FileAdmissionTier, FileProgress, FileProgressState, NewFileProgress,
};
use voom_store::repo::execution::workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, FileRunHistory, FileRunStart, NewFileRunHistory,
    NewFileRunStart,
};
use voom_store::repo::media::identity::{FileLocationAddress, FileLocationRepo, FileVersionRepo};

use crate::ControlPlane;
use crate::workflow::coordinator::PhaseFile;
use crate::workflow::coordinator::finalize::ProducedRefs;
use crate::workflow::coordinator::promotion::ensure_unique_selected_branch_ids;
use crate::workflow::plan::expansion::branch_ids_from_paths;

#[derive(Debug)]
pub(super) struct PreparedResumeSeed {
    pub(super) phase_ordinal: u32,
    pub(super) branch_id: String,
    pub(super) ticket_ids: Vec<TicketId>,
    pub(super) produced: ProducedRefs,
    pub(super) outcome: FilePhaseOutcome,
}

#[derive(Debug)]
pub(super) struct ResumePreparation {
    pub(super) files: Vec<PhaseFile>,
    pub(super) run_starts: Vec<NewFileRunStart>,
    pub(super) history: Vec<NewFileRunHistory>,
    pub(super) seeds: Vec<PreparedResumeSeed>,
    pub(super) terminal_progress: Vec<NewFileProgress>,
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
    terminal_progress: Option<NewFileProgress>,
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
            .find_map(|location| match &location.address {
                FileLocationAddress::Rooted {
                    provider_relative_locator,
                    ..
                } => Some(provider_relative_locator.as_str().to_owned()),
                FileLocationAddress::UnassignedLegacy { .. } => None,
            })
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
            .workflow_progress
            .file_window(prior_job_id)
            .await?
            .ok_or_else(|| {
                resume_incomplete(format!("missing file window for job {prior_job_id}"))
            })?;
        let progress = self
            .workflow_progress
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
        let mut terminal_progress = Vec::new();
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
            terminal_progress.extend(prepared.terminal_progress);
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
            terminal_progress,
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
        let mut phase_history = validate_prior_history(prior.start, prior.inherited, phase_count)?;
        let state = validate_prior_row_shape(prior.start, prior.rows, &phase_history, phase_count)?;
        validate_prior_progress(prior.start, prior.progress, &state)?;
        merge_prior_rows(&file.branch_id, &mut phase_history, prior.rows)?;
        let mut seeds = prior_row_seeds(&file.branch_id, prior.rows);
        let mut next_ordinal = state.next_ordinal;
        if state.phase_complete && file.version_id != state.recorded_tip {
            return Err(resume_incomplete(format!(
                "phase-complete branch {} changed from version {} to {}",
                file.branch_id, state.recorded_tip, file.version_id
            )));
        }
        if prior.progress.state == FileProgressState::Terminal {
            next_ordinal = phase_count;
        } else if file.version_id != state.recorded_tip {
            let (produced, ticket_ids) = self
                .unfinalized_committed_refs(prior_job_id, next_ordinal, &file)
                .await?
                .ok_or_else(|| {
                    resume_incomplete(format!(
                        "branch {} changed from version {} to {} without committed \
                         prior-job evidence for phase {next_ordinal}",
                        file.branch_id, state.recorded_tip, file.version_id
                    ))
                })?;
            seeds.push(PreparedResumeSeed {
                phase_ordinal: next_ordinal,
                branch_id: file.branch_id.clone(),
                ticket_ids,
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
                ticket_ids: Vec::new(),
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
        let terminal_progress = terminal_resume_progress(prior.progress, &file, phase_count);
        let survivor = (prior.progress.state != FileProgressState::Terminal).then(|| {
            file.ordinal = prior.progress.input_ordinal;
            file.admission_tier = resume_admission_tier(prior.progress);
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
            terminal_progress,
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

fn resume_admission_tier(progress: &FileProgress) -> FileAdmissionTier {
    match progress.state {
        FileProgressState::Active | FileProgressState::Terminalizing => {
            FileAdmissionTier::Interrupted
        }
        FileProgressState::Pending | FileProgressState::Terminal => progress.admission_tier,
    }
}

fn terminal_resume_progress(
    prior: &FileProgress,
    file: &PhaseFile,
    phase_count: u32,
) -> Option<NewFileProgress> {
    (prior.state == FileProgressState::Terminal).then(|| NewFileProgress {
        branch_id: file.branch_id.clone(),
        input_ordinal: prior.input_ordinal,
        admission_tier: prior.admission_tier,
        next_phase_ordinal: phase_count,
    })
}

fn prior_row_seeds(branch_id: &str, rows: &[&FilePhaseSummary]) -> Vec<PreparedResumeSeed> {
    rows.iter()
        .map(|row| PreparedResumeSeed {
            phase_ordinal: row.phase_ordinal,
            branch_id: branch_id.to_owned(),
            ticket_ids: row.ticket_ids.clone(),
            produced: ProducedRefs::resume_seed(row),
            outcome: row.outcome,
        })
        .collect()
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
        merge_phase_outcome(
            &start.branch_id,
            &mut history,
            row.phase_ordinal,
            row.outcome,
        )?;
    }
    if let Some((&blocked_ordinal, _)) = history
        .iter()
        .find(|(_, outcome)| **outcome == FilePhaseOutcome::Blocked)
        && history.keys().any(|ordinal| *ordinal > blocked_ordinal)
    {
        return Err(resume_incomplete(format!(
            "branch {} has inherited outcomes after blocked phase {blocked_ordinal}",
            start.branch_id
        )));
    }
    Ok(history)
}

fn merge_prior_rows(
    branch_id: &str,
    history: &mut BTreeMap<u32, FilePhaseOutcome>,
    rows: &[&FilePhaseSummary],
) -> Result<(), VoomError> {
    for row in rows {
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
    inherited: &BTreeMap<u32, FilePhaseOutcome>,
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
        validate_seed_row(start, seed, inherited, phase_count)?;
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

fn validate_seed_row(
    start: &FileRunStart,
    row: &FilePhaseSummary,
    inherited: &BTreeMap<u32, FilePhaseOutcome>,
    phase_count: u32,
) -> Result<(), VoomError> {
    let valid = if row.outcome == FilePhaseOutcome::Blocked {
        start.starting_phase_ordinal == phase_count
    } else {
        inherited.get(&row.phase_ordinal) == Some(&row.outcome)
    };
    if !valid {
        return Err(resume_incomplete(format!(
            "branch {} phase {} seed {:?} does not match inherited history",
            start.branch_id, row.phase_ordinal, row.outcome
        )));
    }
    Ok(())
}

fn resume_incomplete(detail: impl std::fmt::Display) -> VoomError {
    VoomError::PolicyExecution(format!("resume state is incomplete: {detail}"))
}
