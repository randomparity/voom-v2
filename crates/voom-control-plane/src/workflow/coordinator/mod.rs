//! Multi-file sliding-window policy coordinator.
//!
//! `run_phase_barrier` retains its public name and one-job durability contract,
//! but execution no longer forms whole-input barriers. A durable admission
//! cursor bounds the active file pipelines. Each admitted file plans and runs
//! its phases in order against its refreshed chain tip, promotes its terminal
//! artifact, reclaims superseded intermediates, and then releases its slot.
//! Phase-level reports are folded from durable per-file results after the window
//! drains.
//!
//! Responsibility map of the child modules:
//! - [`planning`] — phase planning/policy projection and report/summary aggregation.
//! - [`promotion`] — terminal-artifact placement into the operator output dir.
//! - [`finalize`] — per-file/per-phase durable row writing and payload/sqlite helpers.
//! - [`resume`] — resume reconciliation and chain-tip/snapshot projection.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use voom_core::{FileAssetId, FileVersionId, JobId, PolicyInputSetId, PolicyVersionId, VoomError};
use voom_plan::{ExecutionPlan, PlanningContext, PlanningRequest};
use voom_policy::PolicyInputSetDraft;
use voom_store::repo::execution::jobs::{JobState, NewJob};
use voom_store::repo::execution::tickets::TicketState;
use voom_store::repo::execution::workflow_progress::{
    FileAdmissionTier, FileProgress, NewFilePhaseEntry, NewFileProgress,
};
use voom_store::repo::execution::workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, NewFileRunHistory, NewFileRunStart, NewPhaseSummary,
    PhaseSummary, WorkflowSummary,
};
use voom_store::repo::media::identity::{MediaSnapshot, MediaSnapshotRepo};

use crate::ControlPlane;
use crate::cases::commit_tx;
use crate::cases::policy::compliance::{ComplianceExecutionOptions, PromotionPlan};
use crate::cases::policy::plans::plan_compiled_policy_with_input;
use crate::cases::policy::tool_preflight::PolicyToolTarget;

use super::execution::WorkerRuntimeRegistry;
use super::execution::executor::{
    PlannedLineageGuard, RunFailureMode, WORKFLOW_JOB_KIND, WorkflowExecutor,
    WorkflowExecutorOptions, WorkflowFailureDisposition,
};
use super::plan::policy_bridge::{WorkflowExecutionShape, workflow_plan_from_compliance};

mod finalize;
mod planning;
mod promotion;
mod resume;

use finalize::phase_ordinal;
use planning::{
    classify_phase, initial_phase_files, job_grain_summary, phase_draft, phase_outcome,
    regenerate_phase_report, reject_unpublished_on_error, resolved_phase_policy,
};
use resume::{PreparedResumeSeed, ResumePreparation};

#[cfg(test)]
use planning::zero_phase_summary;
use voom_store::tx::begin_write_first;

/// A file the coordinator is advancing through phases. `version_id`/`snapshot`
/// track the file's current chain tip and are refreshed after each commit.
#[derive(Debug, Clone)]
struct PhaseFile {
    pub(super) asset_id: FileAssetId,
    pub(super) version_id: FileVersionId,
    pub(super) snapshot: MediaSnapshot,
    pub(super) branch_id: String,
    pub(super) ordinal: u32,
    pub(super) admission_tier: FileAdmissionTier,
    /// First phase ordinal this file participates in (`0` for a fresh run; set by
    /// resume reconciliation). The loop passes a file through phases below this
    /// untouched (#165).
    pub(super) resume_ordinal: u32,
    /// Committed/skipped outcomes by phase ordinal. Fresh runs build this map
    /// as phases finish; resumed runs restore the inherited durable projection.
    pub(super) phase_history: BTreeMap<u32, FilePhaseOutcome>,
}

fn run_starts_for_files(files: &[PhaseFile]) -> Vec<NewFileRunStart> {
    files
        .iter()
        .map(|file| NewFileRunStart {
            branch_id: file.branch_id.clone(),
            starting_file_version_id: file.version_id,
            starting_phase_ordinal: 0,
        })
        .collect()
}

fn phase_gate_admission(
    policy: &voom_policy::CompiledPolicy,
    phase_name: &str,
    files: &[PhaseFile],
) -> Result<Vec<bool>, VoomError> {
    let phase = policy
        .phases
        .iter()
        .find(|phase| phase.name == phase_name)
        .ok_or_else(|| {
            VoomError::PolicyExecution(format!(
                "phase `{phase_name}` is in phase_order but has no compiled phase"
            ))
        })?;
    let Some(gate) = &phase.run_if else {
        return Ok(vec![true; files.len()]);
    };
    let current_ordinal = policy
        .phase_order
        .iter()
        .position(|name| name == phase_name);
    let referenced_ordinal = policy
        .phase_order
        .iter()
        .position(|name| name == &gate.phase);
    let Some((current_ordinal, referenced_ordinal)) = current_ordinal.zip(referenced_ordinal)
    else {
        return Err(VoomError::PolicyExecution(format!(
            "phase `{phase_name}` run_if references phase `{}` outside phase_order",
            gate.phase
        )));
    };
    if referenced_ordinal >= current_ordinal {
        return Err(VoomError::PolicyExecution(format!(
            "phase `{phase_name}` run_if must reference an earlier phase, got `{}`",
            gate.phase
        )));
    }
    let referenced_ordinal = u32::try_from(referenced_ordinal)
        .map_err(|error| VoomError::Internal(format!("phase ordinal overflow: {error}")))?;

    files
        .iter()
        .map(|file| {
            let outcome = file.phase_history.get(&referenced_ordinal).ok_or_else(|| {
                VoomError::PolicyExecution(format!(
                    "phase `{phase_name}` run_if cannot evaluate branch `{}`: \
                     phase `{}` outcome is missing",
                    file.branch_id, gate.phase
                ))
            })?;
            match (gate.trigger, outcome) {
                (
                    voom_policy::RunIfTrigger::Completed,
                    FilePhaseOutcome::Committed
                    | FilePhaseOutcome::Verified
                    | FilePhaseOutcome::Skipped,
                )
                | (voom_policy::RunIfTrigger::Modified, FilePhaseOutcome::Committed) => Ok(true),
                (
                    voom_policy::RunIfTrigger::Modified,
                    FilePhaseOutcome::Verified | FilePhaseOutcome::Skipped,
                ) => Ok(false),
                (
                    voom_policy::RunIfTrigger::Completed | voom_policy::RunIfTrigger::Modified,
                    FilePhaseOutcome::Blocked,
                ) => Err(VoomError::PolicyExecution(format!(
                    "phase `{phase_name}` run_if found blocked predecessor `{}` \
                     for surviving branch `{}`",
                    gate.phase, file.branch_id
                ))),
            }
        })
        .collect()
}

/// How all of a single file's nodes resolved for one phase.
#[derive(Clone, Debug)]
enum Disposition {
    Blocked,
    Skipped,
    Planned { node_ids: Vec<String> },
}

struct PhaseDispatchScope {
    planned_count: usize,
    lineage_guard: PlannedLineageGuard,
}

fn phase_dispatch_scope(
    files: &[PhaseFile],
    dispositions: &[Disposition],
) -> Result<Option<PhaseDispatchScope>, VoomError> {
    if files.len() != dispositions.len() {
        return Err(VoomError::Internal(format!(
            "phase dispatch has {} files but {} dispositions",
            files.len(),
            dispositions.len()
        )));
    }
    let planned_count = dispositions
        .iter()
        .filter(|disposition| matches!(disposition, Disposition::Planned { .. }))
        .count();
    if planned_count == 0 {
        return Ok(None);
    }
    let expectations = files
        .iter()
        .zip(dispositions)
        .filter_map(|(file, disposition)| {
            matches!(disposition, Disposition::Planned { .. })
                .then_some((file.asset_id, file.version_id))
        })
        .collect::<Vec<_>>();
    let lineage_guard = PlannedLineageGuard::new(planned_count, expectations)?;
    Ok(Some(PhaseDispatchScope {
        planned_count,
        lineage_guard,
    }))
}

fn continued_disposition(
    disposition: &Disposition,
    ticket_states: &[TicketState],
) -> Result<Disposition, VoomError> {
    let Disposition::Planned { node_ids } = disposition else {
        return Ok(disposition.clone());
    };
    if ticket_states.is_empty() {
        return Err(VoomError::Internal(format!(
            "continued phase has no tickets for planned nodes `{}`",
            node_ids.join("`, `")
        )));
    }
    let mut failed = false;
    for state in ticket_states {
        match state {
            TicketState::Succeeded => {}
            TicketState::Failed => failed = true,
            TicketState::Pending | TicketState::Ready | TicketState::Leased => {
                return Err(VoomError::Internal(format!(
                    "continued phase nodes `{}` retained non-terminal ticket state `{}`",
                    node_ids.join("`, `"),
                    state.as_str()
                )));
            }
        }
    }
    if failed {
        Ok(Disposition::Blocked)
    } else {
        Ok(disposition.clone())
    }
}

fn phase_error_strategy(
    policy: &voom_policy::CompiledPolicy,
    phase_name: &str,
) -> Result<voom_policy::ErrorStrategy, VoomError> {
    let phase = policy
        .phases
        .iter()
        .find(|phase| phase.name == phase_name)
        .ok_or_else(|| {
            VoomError::PolicyExecution(format!(
                "phase `{phase_name}` is in phase_order but has no compiled phase"
            ))
        })?;
    match phase.on_error {
        None | Some(voom_policy::ErrorStrategy::Abort) => Ok(voom_policy::ErrorStrategy::Abort),
        Some(voom_policy::ErrorStrategy::Continue) => Ok(voom_policy::ErrorStrategy::Continue),
        Some(voom_policy::ErrorStrategy::Skip) => Err(VoomError::PolicyValidationError(format!(
            "phase `{phase_name}` declares unpublished on_error `skip`"
        ))),
    }
}

/// Durable result of a phase-barrier run: the owning job's summary plus the
/// per-phase and per-`(file, phase)` rows the run wrote.
#[derive(Debug, Clone)]
pub struct CoordinatorOutcome {
    pub job_id: JobId,
    pub summary: WorkflowSummary,
    pub phases: Vec<PhaseSummary>,
    pub file_phases: Vec<FilePhaseSummary>,
}

/// A phase-barrier run that failed after the job opened. `partial` carries the
/// per-`(file, phase)` rows for files that committed inline before the failure.
#[derive(Debug)]
pub struct CoordinatorError {
    pub source: VoomError,
    pub partial: Option<CoordinatorOutcome>,
}

impl From<VoomError> for CoordinatorError {
    /// Errors with no inline-committed work carry no partial outcome.
    fn from(source: VoomError) -> Self {
        Self {
            source,
            partial: None,
        }
    }
}

fn coordinator_cleanup_error(
    original: CoordinatorError,
    job_id: JobId,
    cleanup: &VoomError,
) -> CoordinatorError {
    CoordinatorError {
        source: VoomError::Internal(format!(
            "coordinator failed for job {job_id}: {}; finalizing the job also failed: {cleanup}",
            original.source
        )),
        partial: original.partial,
    }
}

/// A phase that failed during dispatch. `run_summary` is `Some` once the
/// executor actually ran the workflow (and so some files may have committed
/// inline before draining), `None` for a pre-dispatch bridge failure.
struct PhaseDispatchFailure {
    pub(super) source: VoomError,
    pub(super) run_summary: Option<crate::workflow::WorkflowRunSummary>,
    pub(super) job_failed: bool,
    pub(super) disposition: WorkflowFailureDisposition,
}

fn should_continue_after_dispatch_failure(
    failure: &PhaseDispatchFailure,
    error_strategy: voom_policy::ErrorStrategy,
) -> bool {
    failure.disposition == WorkflowFailureDisposition::IsolatedTicket
        && error_strategy == voom_policy::ErrorStrategy::Continue
}

/// Shared inputs for a fresh or resumed phase-barrier run. Everything here is
/// prepared before a new job opens, so validation failures do not create a job
/// that immediately needs cleanup.
pub(crate) struct PhaseBarrierRunInputs {
    policy: voom_policy::CompiledPolicy,
    context: PlanningContext,
    base_draft: PolicyInputSetDraft,
    files: Vec<PhaseFile>,
}

pub(crate) struct PreparedResumeRunInputs {
    policy: voom_policy::CompiledPolicy,
    context: PlanningContext,
    base_draft: PolicyInputSetDraft,
    preparation: ResumePreparation,
}

/// Everything the phase-loop runner owns once an in-job run starts.
struct PhaseLoopInputs {
    job_id: JobId,
    policy: voom_policy::CompiledPolicy,
    context: PlanningContext,
    base_draft: PolicyInputSetDraft,
    files: Vec<PhaseFile>,
    seed_file_phases: Vec<FilePhaseSummary>,
    options: ComplianceExecutionOptions,
    runtimes: WorkerRuntimeRegistry,
}

/// Files split by whether this phase should advance them or preserve them until
/// their resume phase.
struct PhaseEntry {
    entering: Vec<PhaseFile>,
    passthrough: Vec<PhaseFile>,
}

/// Planner output plus the per-file dispositions the dispatcher and persistence
/// code both need for a phase.
struct PlannedPhase {
    plan: ExecutionPlan,
    report: voom_plan::ComplianceReport,
    dispositions: Vec<Disposition>,
    error_strategy: voom_policy::ErrorStrategy,
}

#[derive(Clone)]
struct FilePhaseObservation {
    phase_ordinal: u32,
    phase_name: String,
    branch_id: String,
    input_ordinal: u32,
    snapshot: MediaSnapshot,
    gate_admitted: bool,
}

struct FilePipelineOutcome {
    last_run: Option<crate::workflow::WorkflowRunSummary>,
    continued_error: Option<VoomError>,
}

struct FilePipelineFailure {
    source: VoomError,
    last_run: Option<crate::workflow::WorkflowRunSummary>,
}

#[derive(Clone)]
struct FileAdmissionGate {
    open: Arc<AtomicBool>,
    admission_lock: Arc<Mutex<()>>,
}

impl FileAdmissionGate {
    fn new() -> Self {
        Self {
            open: Arc::new(AtomicBool::new(true)),
            admission_lock: Arc::new(Mutex::new(())),
        }
    }

    fn close(&self) {
        self.open.store(false, Ordering::Release);
    }

    async fn admit_next_file(
        &self,
        control_plane: &ControlPlane,
        job_id: JobId,
    ) -> Result<Option<FileProgress>, VoomError> {
        let _admission = self.admission_lock.lock().await;
        if !self.open.load(Ordering::Acquire) {
            return Ok(None);
        }
        control_plane
            .workflow_progress
            .admit_next_file(job_id, control_plane.clock().now())
            .await
    }
}

struct FileAdmissionFailureGuard {
    gate: FileAdmissionGate,
    armed: bool,
}

impl FileAdmissionFailureGuard {
    fn new(gate: FileAdmissionGate) -> Self {
        Self { gate, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for FileAdmissionFailureGuard {
    fn drop(&mut self) {
        if self.armed {
            self.gate.close();
        }
    }
}

async fn run_guarded_file_pipeline(
    gate: FileAdmissionGate,
    pipeline: impl Future<Output = Result<FilePipelineOutcome, FilePipelineFailure>>,
) -> Result<FilePipelineOutcome, FilePipelineFailure> {
    let mut failure_guard = FileAdmissionFailureGuard::new(gate);
    let result = pipeline.await;
    if result.is_ok() {
        failure_guard.disarm();
    }
    result
}

async fn close_admission_during_recovery<T>(
    gate: &FileAdmissionGate,
    recovery: impl Future<Output = T>,
) -> T {
    gate.close();
    recovery.await
}

struct FileWindowRefill<'a> {
    pending: &'a mut BTreeMap<String, PhaseFile>,
    seeds_by_branch: &'a mut BTreeMap<String, Vec<FilePhaseSummary>>,
    active: &'a mut JoinSet<Result<FilePipelineOutcome, FilePipelineFailure>>,
    promotion_source_root: &'a Path,
    admission_gate: &'a FileAdmissionGate,
}

fn prepare_file_window_queues(
    inputs: &mut PhaseLoopInputs,
) -> (
    BTreeMap<String, PhaseFile>,
    BTreeMap<String, Vec<FilePhaseSummary>>,
) {
    let pending = inputs
        .files
        .iter()
        .cloned()
        .map(|file| (file.branch_id.clone(), file))
        .collect();
    let mut seeds_by_branch = BTreeMap::<String, Vec<FilePhaseSummary>>::new();
    for row in std::mem::take(&mut inputs.seed_file_phases) {
        seeds_by_branch
            .entry(row.branch_id.clone())
            .or_default()
            .push(row);
    }
    (pending, seeds_by_branch)
}

fn file_pipeline_failure(
    source: VoomError,
    last_run: Option<&crate::workflow::WorkflowRunSummary>,
) -> FilePipelineFailure {
    FilePipelineFailure {
        source,
        last_run: last_run.cloned(),
    }
}

fn merge_run_summary(
    accumulated: &mut Option<crate::workflow::WorkflowRunSummary>,
    next: Option<crate::workflow::WorkflowRunSummary>,
) {
    let Some(next) = next else {
        return;
    };
    if let Some(summary) = accumulated {
        summary.merge_invocation(next);
    } else {
        *accumulated = Some(next);
    }
}

/// State for the phase-barrier loop. The loop has to coordinate planning,
/// dispatch, durable summaries, and resume handoff; keeping those transitions
/// named here prevents the top-level coordinator from becoming a mixed
/// responsibility control flow block.
struct PhaseLoop<'a> {
    control_plane: &'a ControlPlane,
    job_id: JobId,
    policy: voom_policy::CompiledPolicy,
    context: PlanningContext,
    base_draft: PolicyInputSetDraft,
    executor: WorkflowExecutor,
    files: Vec<PhaseFile>,
    promotion: PromotionPlan,
    file_phases: Vec<FilePhaseSummary>,
    last_run: Option<crate::workflow::WorkflowRunSummary>,
    continued_error: Option<VoomError>,
    promotable_branches: BTreeSet<String>,
    branch_id: Option<String>,
    promotion_source_root: PathBuf,
    promotion_source_dir: Option<PathBuf>,
    admission_gate: FileAdmissionGate,
}

impl<'a> PhaseLoop<'a> {
    fn new(
        control_plane: &'a ControlPlane,
        inputs: PhaseLoopInputs,
        promotion_source_root: PathBuf,
        promotion_source_dir: Option<PathBuf>,
        admission_gate: FileAdmissionGate,
    ) -> Self {
        // Derive promotion pairs from the operator output dirs before the options
        // are converted (the conversion repoints commit targets to working dirs).
        let promotion = inputs.options.promotion_plan();
        let mut base_draft = inputs.base_draft;
        base_draft.media_snapshots.clear();
        let executor = WorkflowExecutor::with_options(
            control_plane.clone(),
            inputs.runtimes,
            WorkflowExecutorOptions::from(inputs.options),
        );
        let promotable_branches = inputs
            .files
            .iter()
            .filter(|file| {
                !file
                    .phase_history
                    .values()
                    .any(|outcome| *outcome == FilePhaseOutcome::Blocked)
            })
            .map(|file| file.branch_id.clone())
            .collect();
        let branch_id = inputs.files.first().map(|file| file.branch_id.clone());
        Self {
            control_plane,
            job_id: inputs.job_id,
            policy: inputs.policy,
            context: inputs.context,
            base_draft,
            executor,
            files: inputs.files,
            promotion,
            file_phases: inputs.seed_file_phases,
            last_run: None,
            continued_error: None,
            promotable_branches,
            branch_id,
            promotion_source_root,
            promotion_source_dir,
            admission_gate,
        }
    }

    async fn run_file_pipeline(self) -> Result<FilePipelineOutcome, FilePipelineFailure> {
        self.run_file_pipeline_after_phase_plan(|_| std::future::ready(Ok(())))
            .await
    }

    async fn run_file_pipeline_after_phase_plan<F, Fut>(
        mut self,
        mut after_phase_plan: F,
    ) -> Result<FilePipelineOutcome, FilePipelineFailure>
    where
        F: FnMut(u32) -> Fut,
        Fut: Future<Output = Result<(), VoomError>>,
    {
        let phase_order = self.policy.phase_order.clone();
        for (index, phase_name) in phase_order.iter().enumerate() {
            if self.files.is_empty() {
                break;
            }
            let phase_ordinal = phase_ordinal(index)
                .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
            let Some(mut entry) = self.enter_phase(phase_ordinal) else {
                continue;
            };
            if entry.entering.len() != 1 {
                return Err(file_pipeline_failure(
                    VoomError::Internal(format!(
                        "file pipeline phase {phase_ordinal} entered {} files",
                        entry.entering.len()
                    )),
                    self.last_run.as_ref(),
                ));
            }
            let mut planned = self
                .plan_phase_for_files(phase_name, &entry.entering)
                .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
            self.record_phase_entry(phase_ordinal, phase_name, &entry.entering)
                .await
                .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
            after_phase_plan(phase_ordinal)
                .await
                .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
            self.resolve_file_dispatch(phase_ordinal, &entry.entering, &mut planned)
                .await?;
            if matches!(planned.dispositions.as_slice(), [Disposition::Blocked]) {
                self.promotable_branches.clear();
            }
            let (rows, refreshed) = self
                .control_plane
                .finalize_phase(
                    self.job_id,
                    phase_ordinal,
                    &mut entry.entering,
                    &planned.dispositions,
                )
                .await
                .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
            if refreshed.len() != 1 || rows.len() != 1 {
                return Err(FilePipelineFailure {
                    source: VoomError::Internal(format!(
                        "branch pipeline phase {phase_ordinal} produced {} rows and {} snapshots",
                        rows.len(),
                        refreshed.len()
                    )),
                    last_run: self.last_run,
                });
            }
            self.file_phases.extend(rows);
            self.recombine_survivors(entry);
        }

        self.finish_file_pipeline().await
    }

    async fn resolve_file_dispatch(
        &mut self,
        phase_ordinal: u32,
        entering: &[PhaseFile],
        planned: &mut PlannedPhase,
    ) -> Result<(), FilePipelineFailure> {
        let Err(mut failure) = self
            .dispatch_phase_work(phase_ordinal, entering, planned)
            .await
        else {
            return Ok(());
        };
        debug_assert!(
            !failure.job_failed || failure.disposition == WorkflowFailureDisposition::Fatal,
            "a durable job failure cannot be isolated"
        );
        if !should_continue_after_dispatch_failure(&failure, planned.error_strategy) {
            let admission_gate = self.admission_gate.clone();
            return close_admission_during_recovery(&admission_gate, async {
                let phase_dispatched = failure.run_summary.is_some();
                if let Some(run) = failure.run_summary.take() {
                    self.record_run(run);
                }
                if phase_dispatched {
                    let [file] = entering else {
                        return Err(file_pipeline_failure(
                            VoomError::Internal(format!(
                                "failed file pipeline phase {phase_ordinal} had {} inputs",
                                entering.len()
                            )),
                            self.last_run.as_ref(),
                        ));
                    };
                    let [disposition] = planned.dispositions.as_slice() else {
                        return Err(file_pipeline_failure(
                            VoomError::Internal(format!(
                                "failed file pipeline phase {phase_ordinal} had {} dispositions",
                                planned.dispositions.len()
                            )),
                            self.last_run.as_ref(),
                        ));
                    };
                    self.control_plane
                        .finalize_failed_file_phase(self.job_id, phase_ordinal, file, disposition)
                        .await
                        .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
                }
                Err(file_pipeline_failure(
                    failure.source,
                    self.last_run.as_ref(),
                ))
            })
            .await;
        }
        planned.dispositions = self
            .control_plane
            .continued_dispositions(self.job_id, phase_ordinal, entering, &planned.dispositions)
            .await
            .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
        if matches!(planned.dispositions.first(), Some(Disposition::Blocked)) {
            self.promotable_branches.clear();
        }
        if let Some(summary) = failure.run_summary {
            self.record_run(summary);
        }
        if self.continued_error.is_none() {
            self.continued_error = Some(failure.source);
        }
        Ok(())
    }

    async fn record_phase_entry(
        &self,
        phase_ordinal: u32,
        phase_name: &str,
        entering: &[PhaseFile],
    ) -> Result<(), VoomError> {
        let [file] = entering else {
            return Err(VoomError::Internal(format!(
                "phase entry {phase_ordinal} has {} files",
                entering.len()
            )));
        };
        let gate_admission = phase_gate_admission(&self.policy, phase_name, entering)?;
        let [gate_admitted] = gate_admission.as_slice() else {
            return Err(VoomError::Internal(format!(
                "phase entry {phase_ordinal} did not produce one gate decision"
            )));
        };
        self.control_plane
            .workflow_progress
            .upsert_file_phase_entry(
                NewFilePhaseEntry {
                    job_id: self.job_id,
                    phase_ordinal,
                    branch_id: file.branch_id.clone(),
                    media_snapshot_id: file.snapshot.id,
                    gate_admitted: *gate_admitted,
                },
                self.control_plane.clock().now(),
            )
            .await?;
        Ok(())
    }

    async fn finish_file_pipeline(self) -> Result<FilePipelineOutcome, FilePipelineFailure> {
        let branch_id = self.branch_id.clone().ok_or_else(|| {
            file_pipeline_failure(
                VoomError::Internal("file pipeline lost its branch id".to_owned()),
                self.last_run.as_ref(),
            )
        })?;
        self.control_plane
            .workflow_progress
            .begin_file_terminalization(self.job_id, &branch_id)
            .await
            .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
        if self.promotable_branches.contains(&branch_id) {
            self.control_plane
                .validated_committed_location_ids_for_rows(&self.file_phases)
                .await
                .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
            let location_ids = self
                .control_plane
                .promotion_location_ids_for_branches(
                    &self.file_phases,
                    std::slice::from_ref(&branch_id),
                )
                .await
                .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
            self.control_plane
                .promote_terminal_artifacts(
                    &self.promotion,
                    &location_ids,
                    &self.promotion_source_root,
                    self.promotion_source_dir.as_deref(),
                )
                .await
                .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
            self.control_plane
                .reclaim_superseded_intermediates(&self.promotion, &self.file_phases)
                .await
                .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
        }
        self.control_plane
            .workflow_progress
            .mark_file_terminal(self.job_id, &branch_id, self.control_plane.clock().now())
            .await
            .map_err(|source| file_pipeline_failure(source, self.last_run.as_ref()))?;
        Ok(FilePipelineOutcome {
            last_run: self.last_run,
            continued_error: self.continued_error,
        })
    }

    fn enter_phase(&mut self, phase_ordinal: u32) -> Option<PhaseEntry> {
        // Files below their resume ordinal pass through untouched and rejoin
        // once the loop reaches their own phase.
        let (entering, passthrough): (Vec<PhaseFile>, Vec<PhaseFile>) =
            std::mem::take(&mut self.files)
                .into_iter()
                .partition(|file| file.resume_ordinal <= phase_ordinal);
        if entering.is_empty() {
            self.files = passthrough;
            None
        } else {
            Some(PhaseEntry {
                entering,
                passthrough,
            })
        }
    }

    fn plan_phase_for_files(
        &self,
        phase_name: &str,
        entering: &[PhaseFile],
    ) -> Result<PlannedPhase, VoomError> {
        let gate_admission = phase_gate_admission(&self.policy, phase_name, entering)?;
        let admitted = entering
            .iter()
            .zip(&gate_admission)
            .filter(|(_, admitted)| **admitted)
            .map(|(file, _)| file.clone())
            .collect::<Vec<_>>();
        let suppress_operations = admitted.is_empty();
        let planning_files = if suppress_operations {
            entering
        } else {
            admitted.as_slice()
        };
        let draft = phase_draft(&self.base_draft, planning_files);
        let policy = resolved_phase_policy(&self.policy, phase_name, suppress_operations)?;
        let plan = voom_plan::plan_phase(
            PlanningRequest {
                policy,
                input: draft,
                context: self.context.clone(),
            },
            phase_name,
        )
        .map_err(voom_plan::PlanGenerationError::into_voom_error)?;
        let report = voom_plan::generate_compliance_report(&plan)
            .map_err(voom_plan::ComplianceReportError::into_voom_error)?;
        let dispositions = classify_phase(entering, &plan)?;
        let error_strategy = phase_error_strategy(&self.policy, phase_name)?;
        Ok(PlannedPhase {
            plan,
            report,
            dispositions,
            error_strategy,
        })
    }

    async fn dispatch_phase_work(
        &mut self,
        phase_ordinal: u32,
        entering: &[PhaseFile],
        planned: &PlannedPhase,
    ) -> Result<(), PhaseDispatchFailure> {
        let run = self
            .control_plane
            .dispatch_phase(
                &self.executor,
                self.job_id,
                phase_ordinal,
                entering,
                planned,
            )
            .await?;
        if let Some(run) = run {
            self.record_run(run);
        }
        Ok(())
    }

    fn record_run(&mut self, run: crate::workflow::WorkflowRunSummary) {
        if let Some(summary) = &mut self.last_run {
            summary.merge_invocation(run);
        } else {
            self.last_run = Some(run);
        }
    }

    fn recombine_survivors(&mut self, entry: PhaseEntry) {
        self.files = entry.entering;
        self.files.extend(entry.passthrough);
    }
}

impl ControlPlane {
    async fn continued_dispositions(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        files: &[PhaseFile],
        dispositions: &[Disposition],
    ) -> Result<Vec<Disposition>, VoomError> {
        if files.len() != dispositions.len() {
            return Err(VoomError::Internal(format!(
                "continued phase has {} files but {} dispositions",
                files.len(),
                dispositions.len()
            )));
        }
        let mut resolved = Vec::with_capacity(dispositions.len());
        for (file, disposition) in files.iter().zip(dispositions) {
            let Disposition::Planned { node_ids } = disposition else {
                resolved.push(disposition.clone());
                continue;
            };
            let mut states = Vec::with_capacity(node_ids.len());
            for node_id in node_ids {
                let workflow_node_id = super::plan::policy_bridge::policy_workflow_node_id(node_id);
                let ticket_ids = self
                    .ticket_ids_for_phase_node(
                        job_id,
                        phase_ordinal,
                        &workflow_node_id,
                        file.version_id,
                    )
                    .await?;
                for ticket_id in ticket_ids {
                    let ticket = self.tickets.get(ticket_id).await?.ok_or_else(|| {
                        VoomError::NotFound(format!(
                            "continued phase ticket {ticket_id} for node `{node_id}`"
                        ))
                    })?;
                    states.push(ticket.state);
                }
            }
            resolved.push(continued_disposition(disposition, &states)?);
        }
        Ok(resolved)
    }

    /// Drive the existing workflow executor one phase at a time across every
    /// file in a policy input set, phases acting as barriers across files
    /// (issue #162, Sprint 16 §3/§6). The coordinator owns one job for the whole
    /// run (ADR-0007) and persists a durable per-phase / per-`(file, phase)`
    /// summary.
    ///
    /// # Errors
    /// Returns [`CoordinatorError`] when durable inputs are missing, the policy
    /// fails to compile, or a phase's tickets fail. Any error after the job
    /// opens finalizes the job as `failed`.
    pub async fn run_phase_barrier(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let runtimes = self.policy_runtime_registry().await?;
        Box::pin(self.run_phase_barrier_with_runtimes(
            policy_version_id,
            input_set_id,
            options,
            runtimes,
        ))
        .await
    }

    /// [`Self::run_phase_barrier`] with an injected worker-runtime registry, so
    /// tests can drive the loop against in-process fakes without discovering
    /// workers.
    ///
    /// # Errors
    /// See [`Self::run_phase_barrier`].
    pub(crate) async fn run_phase_barrier_with_runtimes(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let (_, inputs) = self
            .prepare_phase_barrier_run_inputs(policy_version_id, input_set_id)
            .await?;
        Box::pin(self.run_prepared_phase_barrier(inputs, options, runtimes)).await
    }

    pub(crate) async fn run_prepared_phase_barrier(
        &self,
        inputs: PhaseBarrierRunInputs,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let max_in_flight_files = options.file_window_limit()?;
        let starts = run_starts_for_files(&inputs.files);
        let (job, _) = self
            .open_sliding_file_job(
                &starts,
                Vec::new(),
                Vec::new(),
                &inputs.files,
                max_in_flight_files,
            )
            .await?;
        let result = self
            .run_phase_barrier_in_job(job.id, inputs, options, runtimes)
            .await;
        self.finish_phase_barrier_job(job.id, result).await
    }

    /// Resume a crashed or failed phase-barrier run (issue #165, spec §3/§8).
    /// Opens a **new** job and reconciles each file against `prior_job_id`'s
    /// per-`(file, phase)` rows (ADR-0009). Pass the **most-recently-failed**
    /// run's job id (the latest [`CoordinatorError::partial`] outcome's
    /// `job_id`).
    ///
    /// # Errors
    /// Returns [`CoordinatorError`] when `prior_job_id` does not exist, durable
    /// inputs are missing, the policy declares an unsupported `on_error`, or a
    /// phase's tickets fail.
    pub async fn resume_phase_barrier(
        &self,
        prior_job_id: JobId,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let runtimes = self.policy_runtime_registry().await?;
        Box::pin(self.resume_phase_barrier_with_runtimes(
            prior_job_id,
            policy_version_id,
            input_set_id,
            options,
            runtimes,
        ))
        .await
    }

    /// [`Self::resume_phase_barrier`] with an injected worker-runtime registry, so
    /// tests can drive resume against in-process fakes.
    ///
    /// # Errors
    /// See [`Self::resume_phase_barrier`].
    pub(crate) async fn resume_phase_barrier_with_runtimes(
        &self,
        prior_job_id: JobId,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        if self.jobs.get(prior_job_id).await?.is_none() {
            return Err(VoomError::NotFound(format!(
                "resume: prior job {prior_job_id} does not exist"
            ))
            .into());
        }
        let (_, inputs) = self
            .prepare_phase_barrier_run_inputs(policy_version_id, input_set_id)
            .await?;
        let prepared = self
            .prepare_resume_phase_barrier_run_inputs(prior_job_id, inputs)
            .await?;
        Box::pin(self.run_prepared_resume_phase_barrier(prepared, options, runtimes)).await
    }

    pub(crate) async fn prepare_resume_phase_barrier_run_inputs(
        &self,
        prior_job_id: JobId,
        inputs: PhaseBarrierRunInputs,
    ) -> Result<PreparedResumeRunInputs, VoomError> {
        let phase_count = u32::try_from(inputs.policy.phase_order.len())
            .map_err(|e| VoomError::Internal(format!("phase count overflow: {e}")))?;
        let preparation = self
            .prepare_resume(prior_job_id, inputs.files, phase_count)
            .await?;
        Ok(PreparedResumeRunInputs {
            policy: inputs.policy,
            context: inputs.context,
            base_draft: inputs.base_draft,
            preparation,
        })
    }

    pub(crate) async fn run_prepared_resume_phase_barrier(
        &self,
        inputs: PreparedResumeRunInputs,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let max_in_flight_files = options.file_window_limit()?;
        let PreparedResumeRunInputs {
            policy,
            context,
            base_draft,
            preparation:
                ResumePreparation {
                    files,
                    run_starts,
                    history,
                    seeds,
                    terminal_progress,
                    max_in_flight_files: prior_max_in_flight_files,
                },
        } = inputs;
        if max_in_flight_files != prior_max_in_flight_files {
            return Err(VoomError::Conflict(format!(
                "resume file window {max_in_flight_files} does not match prior durable window \
                 {prior_max_in_flight_files}"
            ))
            .into());
        }
        let (job, seed_file_phases) = self
            .open_sliding_file_job_with_terminal_progress(
                &run_starts,
                history,
                seeds,
                &files,
                terminal_progress,
                max_in_flight_files,
            )
            .await?;
        let result = self
            .drive_phase_loop(PhaseLoopInputs {
                job_id: job.id,
                policy,
                context,
                base_draft,
                files,
                seed_file_phases,
                options,
                runtimes,
            })
            .await;
        self.finish_phase_barrier_job(job.id, result).await
    }

    /// Prepare all shared phase-barrier inputs that are independent of the new
    /// job. Both fresh and resume runs use the same policy/input identity,
    /// projected planning context, base draft, and active branch-id set.
    pub(crate) async fn prepare_phase_barrier_run_inputs(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<(ExecutionPlan, PhaseBarrierRunInputs), VoomError> {
        let inputs = self
            .load_current_accepted_policy_and_input(policy_version_id, input_set_id)
            .await?;
        let mut policy = self.compiled_policy_for_version(&inputs.version).await?;
        reject_unpublished_on_error(&policy)?;
        let stored = self
            .resolve_stored_planning_input(&policy, inputs.input)
            .await?;
        if stored.files.is_empty() {
            crate::cases::policy::tool_preflight::normalize_policy_tool_requirements(&mut policy)?;
        } else {
            let targets = stored
                .files
                .iter()
                .map(|file| PolicyToolTarget {
                    ordinal: file.ordinal,
                    file_version_id: file.selected_version_id,
                })
                .collect::<Vec<_>>();
            self.preflight_policy_tools(&mut policy, &targets).await?;
            self.ensure_policy_verifier(&policy).await?;
        }
        let context = PlanningContext {
            policy_document_id: Some(inputs.version.policy_document_id),
            policy_version_id: Some(policy_version_id),
            policy_input_set_id: Some(input_set_id),
            ..PlanningContext::default()
        };
        let initial_plan =
            plan_compiled_policy_with_input(policy.clone(), stored.draft.clone(), context.clone())?;
        let selected = stored
            .files
            .iter()
            .map(|file| file.selected_version_id)
            .collect::<Vec<_>>();
        let branch_ids = self.selected_branch_ids(&selected).await?;
        let files = initial_phase_files(stored.files, branch_ids)?;
        Ok((
            initial_plan,
            PhaseBarrierRunInputs {
                policy,
                context,
                base_draft: stored.draft,
                files,
            },
        ))
    }

    async fn ensure_policy_verifier(
        &self,
        policy: &voom_policy::CompiledPolicy,
    ) -> Result<(), VoomError> {
        let needs_verifier = policy.phases.iter().any(|phase| {
            phase.operations.iter().any(|operation| {
                matches!(operation, voom_policy::CompiledOperation::VerifyArtifact(_))
            })
        });
        if !needs_verifier {
            return Ok(());
        }
        let mut tx = begin_write_first(&self.pool, "coordinator: ensure_policy_verifier").await?;
        crate::artifact::bootstrap::ensure_builtin_verify_artifact_worker_in_tx(self, &mut tx)
            .await?;
        commit_tx(tx).await
    }

    /// Open the owned workflow job, run the supplied in-job phase-barrier work,
    /// and fail the job on every error that escapes after opening.
    #[cfg(test)]
    async fn with_phase_barrier_job<F, Fut>(
        &self,
        run: F,
    ) -> Result<CoordinatorOutcome, CoordinatorError>
    where
        F: FnOnce(JobId) -> Fut,
        Fut: Future<Output = Result<CoordinatorOutcome, CoordinatorError>>,
    {
        let (job, _) = self
            .open_sliding_file_job(&[], Vec::new(), Vec::new(), &[], 1)
            .await?;
        let result = run(job.id).await;
        self.finish_phase_barrier_job(job.id, result).await
    }

    async fn open_sliding_file_job(
        &self,
        run_starts: &[NewFileRunStart],
        history: Vec<NewFileRunHistory>,
        seeds: Vec<PreparedResumeSeed>,
        files: &[PhaseFile],
        max_in_flight_files: u32,
    ) -> Result<
        (
            voom_store::repo::execution::jobs::Job,
            Vec<FilePhaseSummary>,
        ),
        VoomError,
    > {
        self.open_sliding_file_job_with_terminal_progress(
            run_starts,
            history,
            seeds,
            files,
            Vec::new(),
            max_in_flight_files,
        )
        .await
    }

    async fn open_sliding_file_job_with_terminal_progress(
        &self,
        run_starts: &[NewFileRunStart],
        history: Vec<NewFileRunHistory>,
        seeds: Vec<PreparedResumeSeed>,
        files: &[PhaseFile],
        terminal_progress: Vec<NewFileProgress>,
        max_in_flight_files: u32,
    ) -> Result<
        (
            voom_store::repo::execution::jobs::Job,
            Vec<FilePhaseSummary>,
        ),
        VoomError,
    > {
        let now = self.clock().now();
        let mut tx = begin_write_first(
            &self.pool,
            "coordinator: open_sliding_file_job_with_terminal_progress",
        )
        .await?;
        let job = self
            .open_job_in_tx(
                &mut tx,
                NewJob {
                    kind: WORKFLOW_JOB_KIND.to_owned(),
                    priority: 0,
                    created_at: now,
                },
            )
            .await?;
        self.workflow_summaries
            .insert_file_run_starts_in_tx(&mut tx, job.id, run_starts)
            .await?;
        self.workflow_summaries
            .insert_file_run_history_in_tx(&mut tx, job.id, &history)
            .await?;
        let mut progress = files
            .iter()
            .map(|file| NewFileProgress {
                branch_id: file.branch_id.clone(),
                input_ordinal: file.ordinal,
                admission_tier: file.admission_tier,
                next_phase_ordinal: file.resume_ordinal,
            })
            .collect::<Vec<_>>();
        let terminal_branches = terminal_progress
            .iter()
            .map(|row| row.branch_id.clone())
            .collect::<Vec<_>>();
        progress.extend(terminal_progress);
        self.workflow_progress
            .insert_file_window_in_tx(&mut tx, job.id, max_in_flight_files, &progress, now)
            .await?;
        self.workflow_progress
            .mark_file_progress_terminal_in_tx(&mut tx, job.id, &terminal_branches, now)
            .await?;
        let mut rows = Vec::with_capacity(seeds.len());
        for seed in seeds {
            rows.push(
                self.workflow_summaries
                    .upsert_file_phase_summary_in_tx(
                        &mut tx,
                        seed.produced.seed(
                            job.id,
                            seed.phase_ordinal,
                            seed.branch_id,
                            seed.ticket_ids,
                            seed.outcome,
                        ),
                        now,
                    )
                    .await?,
            );
        }
        commit_tx(tx).await?;
        Ok((job, rows))
    }

    async fn finish_phase_barrier_job(
        &self,
        job_id: JobId,
        result: Result<CoordinatorOutcome, CoordinatorError>,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        // Job-cleanup contract: once the job is open, every error path finalizes
        // it as `failed` rather than orphaning it in `open`. A dispatch failure
        // already failed the job inside `run_plan_in_job` (and `fail_job` is a
        // no-op on an already-failed job), so this `fail_job` only matters for
        // pre-dispatch errors that leave the job open. Committed per-`(file,
        // phase)` rows are durable before the error returns (queryable via
        // `file_phases_for_job` and carried in `partial`), satisfying ADR-0007.
        match result {
            Ok(outcome) => Ok(outcome),
            Err(err) => self.finalize_failed_phase_barrier_job(job_id, err).await,
        }
    }

    async fn finalize_failed_phase_barrier_job(
        &self,
        job_id: JobId,
        err: CoordinatorError,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let state = match self.jobs.get(job_id).await {
            Ok(Some(job)) => job.state,
            Ok(None) => {
                return Err(coordinator_cleanup_error(
                    err,
                    job_id,
                    &VoomError::NotFound(format!("job {job_id}")),
                ));
            }
            Err(cleanup) => return Err(coordinator_cleanup_error(err, job_id, &cleanup)),
        };
        match state {
            JobState::Open => {
                if let Err(cleanup) = self
                    .fail_job(job_id, err.source.to_string(), self.clock().now())
                    .await
                {
                    return Err(coordinator_cleanup_error(err, job_id, &cleanup));
                }
                Err(err)
            }
            JobState::Failed | JobState::Cancelled => Err(err),
            JobState::Succeeded => Err(coordinator_cleanup_error(
                err,
                job_id,
                &VoomError::Conflict(format!(
                    "coordinator error cannot finalize succeeded job {job_id}"
                )),
            )),
        }
    }

    async fn run_phase_barrier_in_job(
        &self,
        job_id: JobId,
        inputs: PhaseBarrierRunInputs,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let PhaseBarrierRunInputs {
            policy,
            context,
            base_draft,
            files,
        } = inputs;
        if files.is_empty() || policy.phase_order.is_empty() {
            return Ok(self.finalize_zero_phase_run(job_id, Vec::new()).await?);
        }
        self.drive_phase_loop(PhaseLoopInputs {
            job_id,
            policy,
            context,
            base_draft,
            files,
            seed_file_phases: Vec::new(),
            options,
            runtimes,
        })
        .await
    }

    /// Run the phase loop across `files`, each file participating only in phases
    /// at or above its `resume_ordinal` (`0` for a fresh run). `seed_file_phases`
    /// pre-loads rows a resume backfilled before the loop. Files below their
    /// `resume_ordinal` pass through a phase untouched and rejoin at their own
    /// resume phase (#165).
    async fn drive_phase_loop(
        &self,
        inputs: PhaseLoopInputs,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        if inputs.files.is_empty() || inputs.policy.phase_order.is_empty() {
            let PhaseLoopInputs {
                job_id,
                seed_file_phases,
                ..
            } = inputs;
            return Ok(self
                .finalize_zero_phase_run(job_id, seed_file_phases)
                .await?);
        }
        self.run_sliding_file_window(inputs).await
    }

    async fn run_sliding_file_window(
        &self,
        mut inputs: PhaseLoopInputs,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let started = Instant::now();
        let promotion_source_root = self.promotion_source_root(&inputs.base_draft).await?;
        inputs.base_draft.media_snapshots.clear();
        let max_in_flight_files = inputs.options.file_window_limit()? as usize;
        let (mut pending, mut seeds_by_branch) = prepare_file_window_queues(&mut inputs);
        let mut active = JoinSet::new();
        let admission_gate = FileAdmissionGate::new();
        let mut last_run = None;
        let mut continued_error = None;
        let mut supervisor_error = None;
        let mut pipeline_error = None;
        loop {
            if supervisor_error.is_none() && pipeline_error.is_none() {
                match self.file_window_job_error(inputs.job_id).await {
                    Ok(error) => supervisor_error = error,
                    Err(source) => supervisor_error = Some(source),
                }
            }
            if supervisor_error.is_none()
                && pipeline_error.is_none()
                && let Err(source) = self
                    .fill_file_window(
                        &inputs,
                        max_in_flight_files,
                        FileWindowRefill {
                            pending: &mut pending,
                            seeds_by_branch: &mut seeds_by_branch,
                            active: &mut active,
                            promotion_source_root: &promotion_source_root,
                            admission_gate: &admission_gate,
                        },
                    )
                    .await
            {
                supervisor_error = Some(source);
            }
            let Some(joined) = active.join_next().await else {
                break;
            };
            match joined {
                Ok(Ok(outcome)) => {
                    merge_run_summary(&mut last_run, outcome.last_run);
                    if continued_error.is_none() {
                        continued_error = outcome.continued_error;
                    }
                }
                Ok(Err(failure)) => {
                    merge_run_summary(&mut last_run, failure.last_run);
                    if pipeline_error.is_none() {
                        pipeline_error = Some(failure.source);
                    }
                }
                Err(error) => {
                    if pipeline_error.is_none() {
                        pipeline_error = Some(VoomError::Internal(format!(
                            "file pipeline task failed to join: {error}"
                        )));
                    }
                }
            }
        }
        if !pending.is_empty() && supervisor_error.is_none() && pipeline_error.is_none() {
            return Err(VoomError::Conflict(format!(
                "{} file pipelines remain pending after the sliding window drained",
                pending.len()
            ))
            .into());
        }
        let mut job_run = last_run.unwrap_or_else(|| {
            crate::workflow::WorkflowRunSummary::empty(inputs.job_id, started.elapsed())
        });
        job_run
            .refresh_counts(
                &self.tickets,
                &self.leases,
                inputs.job_id,
                started.elapsed(),
            )
            .await?;
        let last_run = Some(job_run);
        let (phases, file_phases) = self.persist_sliding_phase_summaries(&inputs).await?;
        let failure = pipeline_error.or(supervisor_error).or(continued_error);
        if let Some(source) = failure {
            return self
                .finish_sliding_failure(
                    inputs.job_id,
                    last_run.as_ref(),
                    phases,
                    file_phases,
                    source,
                )
                .await;
        }
        self.finalize_succeeded_run(inputs.job_id, last_run.as_ref(), phases, file_phases)
            .await
            .map_err(CoordinatorError::from)
    }

    async fn file_window_job_error(&self, job_id: JobId) -> Result<Option<VoomError>, VoomError> {
        let job = self
            .jobs
            .get(job_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("sliding-window job {job_id}")))?;
        Ok(match job.state {
            JobState::Open => None,
            JobState::Cancelled => Some(VoomError::UserCancellation(format!(
                "sliding-window job {job_id} was cancelled"
            ))),
            JobState::Failed => Some(VoomError::PolicyExecution(format!(
                "sliding-window job {job_id} failed while pipelines were active"
            ))),
            JobState::Succeeded => Some(VoomError::Conflict(format!(
                "sliding-window job {job_id} succeeded before its pipelines drained"
            ))),
        })
    }

    async fn fill_file_window(
        &self,
        inputs: &PhaseLoopInputs,
        max_in_flight_files: usize,
        refill: FileWindowRefill<'_>,
    ) -> Result<(), VoomError> {
        while refill.active.len() < max_in_flight_files {
            let Some(progress) = refill
                .admission_gate
                .admit_next_file(self, inputs.job_id)
                .await?
            else {
                break;
            };
            let file = refill.pending.remove(&progress.branch_id).ok_or_else(|| {
                VoomError::Conflict(format!(
                    "admitted branch {} has no prepared pipeline input",
                    progress.branch_id
                ))
            })?;
            let promotion_source_dir = self
                .asset_source_path(file.asset_id)
                .await?
                .and_then(|path| path.parent().map(Path::to_path_buf));
            let control_plane = self.clone();
            let promotion_source_root = refill.promotion_source_root.to_path_buf();
            let pipeline_admission_gate = refill.admission_gate.clone();
            let file_inputs = PhaseLoopInputs {
                job_id: inputs.job_id,
                policy: inputs.policy.clone(),
                context: inputs.context.clone(),
                base_draft: inputs.base_draft.clone(),
                files: vec![file],
                seed_file_phases: refill
                    .seeds_by_branch
                    .remove(&progress.branch_id)
                    .unwrap_or_default(),
                options: inputs.options.clone(),
                runtimes: inputs.runtimes.clone(),
            };
            refill.active.spawn(async move {
                run_guarded_file_pipeline(
                    pipeline_admission_gate.clone(),
                    Box::pin(async {
                        PhaseLoop::new(
                            &control_plane,
                            file_inputs,
                            promotion_source_root,
                            promotion_source_dir,
                            pipeline_admission_gate,
                        )
                        .run_file_pipeline()
                        .await
                    }),
                )
                .await
            });
        }
        Ok(())
    }

    async fn persist_sliding_phase_summaries(
        &self,
        inputs: &PhaseLoopInputs,
    ) -> Result<(Vec<PhaseSummary>, Vec<FilePhaseSummary>), VoomError> {
        let file_phases = self
            .workflow_summaries
            .file_phases_for_job(inputs.job_id)
            .await?;
        let observations = self
            .durable_phase_observations(inputs, &file_phases)
            .await?;
        let mut by_phase = BTreeMap::<u32, Vec<FilePhaseObservation>>::new();
        for observation in observations {
            by_phase
                .entry(observation.phase_ordinal)
                .or_default()
                .push(observation);
        }
        let mut outcomes_by_branch = BTreeMap::<(u32, String), FilePhaseOutcome>::new();
        for row in &file_phases {
            outcomes_by_branch.insert((row.phase_ordinal, row.branch_id.clone()), row.outcome);
        }
        let mut phases = Vec::with_capacity(by_phase.len());
        for (phase_ordinal, mut phase_observations) in by_phase {
            phase_observations.sort_by_key(|observation| observation.input_ordinal);
            let phase_name = phase_observations
                .first()
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "phase {phase_ordinal} has no durable observations"
                    ))
                })?
                .phase_name
                .clone();
            let refreshed = phase_observations
                .iter()
                .map(|observation| (observation.input_ordinal, observation.snapshot.clone()))
                .collect::<Vec<_>>();
            let gate_admission = phase_observations
                .iter()
                .map(|observation| observation.gate_admitted)
                .collect::<Vec<_>>();
            let report = regenerate_phase_report(
                &inputs.policy,
                &inputs.context,
                &inputs.base_draft,
                &phase_name,
                &refreshed,
                &gate_admission,
            )?;
            let outcomes = phase_observations
                .iter()
                .map(|observation| {
                    outcomes_by_branch
                        .get(&(phase_ordinal, observation.branch_id.clone()))
                        .copied()
                        .unwrap_or(FilePhaseOutcome::Blocked)
                })
                .collect::<Vec<_>>();
            phases.push(
                self.workflow_summaries
                    .upsert_phase_summary(
                        NewPhaseSummary {
                            job_id: inputs.job_id,
                            phase_ordinal,
                            phase_name,
                            report: Some(report),
                            outcome: phase_outcome(&outcomes),
                        },
                        self.clock().now(),
                    )
                    .await?,
            );
        }
        Ok((phases, file_phases))
    }

    async fn durable_phase_observations(
        &self,
        inputs: &PhaseLoopInputs,
        file_phases: &[FilePhaseSummary],
    ) -> Result<Vec<FilePhaseObservation>, VoomError> {
        let progress = self
            .workflow_progress
            .file_progress_for_job(inputs.job_id)
            .await?
            .into_iter()
            .map(|row| (row.branch_id, row.input_ordinal))
            .collect::<BTreeMap<_, _>>();
        let entries = self
            .workflow_progress
            .file_phase_entries_for_job(inputs.job_id)
            .await?;
        let completed_snapshots = file_phases
            .iter()
            .filter_map(|row| {
                row.reprobe_snapshot_id
                    .map(|snapshot_id| ((row.branch_id.as_str(), row.phase_ordinal), snapshot_id))
            })
            .collect::<BTreeMap<_, _>>();
        let mut observations = Vec::with_capacity(entries.len());
        for entry in entries {
            let input_ordinal = progress.get(&entry.branch_id).ok_or_else(|| {
                VoomError::NotFound(format!(
                    "workflow file progress {}/{}",
                    inputs.job_id, entry.branch_id
                ))
            })?;
            let phase_name = inputs
                .policy
                .phase_order
                .get(usize::try_from(entry.phase_ordinal).map_err(|error| {
                    VoomError::Internal(format!("phase ordinal conversion failed: {error}"))
                })?)
                .ok_or_else(|| {
                    VoomError::Conflict(format!(
                        "durable branch {} has out-of-range phase {}",
                        entry.branch_id, entry.phase_ordinal
                    ))
                })?
                .clone();
            let snapshot_id = completed_snapshots
                .get(&(entry.branch_id.as_str(), entry.phase_ordinal))
                .copied()
                .unwrap_or(entry.media_snapshot_id);
            let snapshot = self
                .identity
                .get_media_snapshot(snapshot_id)
                .await?
                .ok_or_else(|| {
                    VoomError::NotFound(format!(
                        "media snapshot {snapshot_id} for durable phase entry"
                    ))
                })?;
            observations.push(FilePhaseObservation {
                phase_ordinal: entry.phase_ordinal,
                phase_name,
                branch_id: entry.branch_id,
                input_ordinal: *input_ordinal,
                snapshot,
                gate_admitted: entry.gate_admitted,
            });
        }
        Ok(observations)
    }

    async fn finish_sliding_failure(
        &self,
        job_id: JobId,
        last_run: Option<&crate::workflow::WorkflowRunSummary>,
        phases: Vec<PhaseSummary>,
        file_phases: Vec<FilePhaseSummary>,
        source: VoomError,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let now = self.clock().now();
        let summary = self
            .workflow_summaries
            .insert_summary(job_grain_summary(job_id, last_run), now)
            .await?;
        Err(CoordinatorError {
            source,
            partial: Some(CoordinatorOutcome {
                job_id,
                summary,
                phases,
                file_phases,
            }),
        })
    }

    /// Bridge the phase's planned nodes to a workflow and run them in the owned
    /// job, fanning out across the active files. Returns `None` when the phase
    /// has no planned work (every file blocked, skipped, or compliant).
    async fn dispatch_phase(
        &self,
        executor: &WorkflowExecutor,
        job_id: JobId,
        phase_ordinal: u32,
        entering: &[PhaseFile],
        planned_phase: &PlannedPhase,
    ) -> Result<Option<crate::workflow::WorkflowRunSummary>, PhaseDispatchFailure> {
        let scope =
            phase_dispatch_scope(entering, &planned_phase.dispositions).map_err(|source| {
                PhaseDispatchFailure {
                    source,
                    run_summary: None,
                    job_failed: false,
                    disposition: WorkflowFailureDisposition::Fatal,
                }
            })?;
        let Some(scope) = scope else {
            return Ok(None);
        };
        let shape = WorkflowExecutionShape::new(scope.planned_count, scope.planned_count).map_err(
            |source| PhaseDispatchFailure {
                source,
                run_summary: None,
                job_failed: false,
                disposition: WorkflowFailureDisposition::Fatal,
            },
        )?;
        let bridge =
            workflow_plan_from_compliance(&planned_phase.plan, &planned_phase.report, shape)
                .map_err(|source| PhaseDispatchFailure {
                    source,
                    run_summary: None,
                    job_failed: false,
                    disposition: WorkflowFailureDisposition::Fatal,
                })?;
        let Some(workflow) = bridge.workflow else {
            return Ok(None);
        };
        // On a ticket failure the executor drains every in-flight dispatch to a
        // terminal state (so any inline commit has landed) and fails the job;
        // carry its run summary so the partial outcome reports the job-cumulative
        // counts including the failure.
        let invocation_id = match entering {
            [file] => format!("file-{}-phase-{phase_ordinal}", file.ordinal),
            _ => format!("phase-{phase_ordinal}"),
        };
        let run = Box::pin(executor.submit_and_run_guarded_invocation_in_job(
            job_id,
            &invocation_id,
            workflow,
            match (entering, planned_phase.error_strategy) {
                ([_], _) | (_, voom_policy::ErrorStrategy::Continue) => {
                    RunFailureMode::ContinueIndependent
                }
                (_, voom_policy::ErrorStrategy::Abort | voom_policy::ErrorStrategy::Skip) => {
                    RunFailureMode::AbortJob
                }
            },
            scope.lineage_guard,
        ))
        .await
        .map_err(|err| {
            let run_summary = err.dispatch_started.then_some(err.summary);
            PhaseDispatchFailure {
                source: err.source,
                run_summary,
                job_failed: err.job_failed,
                disposition: err.disposition,
            }
        })?;
        Ok(Some(run))
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
