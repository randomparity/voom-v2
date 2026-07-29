//! Multi-file phase-barrier coordinator (issue #162, Sprint 16 §3/§6).
//!
//! `run_phase_barrier` owns one job for the whole run (ADR-0007) and drives the
//! existing executor one phase at a time across every file in a policy input
//! set, phases acting as barriers across files. Each phase projects every
//! still-active file's current chain-tip snapshot through the shared durable
//! snapshot projector, plans that one phase, bridges its planned nodes to a
//! workflow, and runs them in the owned job; blocked files drop,
//! compliant/skipped files stay, committed files advance their chain tip
//! through the identity repository. It persists a durable per-phase /
//! per-`(file, phase)` workflow summary as it goes.
//!
//! Responsibility map of the child modules:
//! - [`planning`] — phase planning/policy projection and report/summary aggregation.
//! - [`promotion`] — terminal-artifact placement into the operator output dir.
//! - [`finalize`] — per-file/per-phase durable row writing and payload/sqlite helpers.
//! - [`resume`] — resume reconciliation and chain-tip/snapshot projection.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
#[cfg(test)]
use std::pin::Pin;

use voom_core::{FileAssetId, FileVersionId, JobId, PolicyInputSetId, PolicyVersionId, VoomError};
use voom_plan::{ExecutionPlan, PlanningContext, PlanningRequest};
use voom_policy::PolicyInputSetDraft;
use voom_store::repo::identity::MediaSnapshot;
use voom_store::repo::jobs::NewJob;
use voom_store::repo::tickets::TicketState;
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, NewFileRunHistory, NewFileRunStart, NewPhaseSummary,
    PhaseSummary, WorkflowSummary,
};

use crate::ControlPlane;
use crate::cases::policy::compliance::{ComplianceExecutionOptions, PromotionPlan};
use crate::cases::policy::plans::plan_compiled_policy_with_input;
use crate::cases::{begin_immediate_tx, begin_tx, commit_tx};

use super::execution::WorkerRuntimeRegistry;
use super::execution::executor::{
    PlannedLineageGuard, RunFailureMode, WORKFLOW_JOB_KIND, WorkflowExecutor,
    WorkflowExecutorOptions,
};
use super::plan::policy_bridge::{WorkflowExecutionShape, workflow_plan_from_compliance};

mod finalize;
mod planning;
mod promotion;
mod resume;

use finalize::{FailedPhaseFinalization, phase_ordinal};
use planning::{
    classify_phase, initial_phase_files, job_grain_summary, phase_draft, phase_outcome,
    regenerate_phase_report, reject_unpublished_on_error, resolved_phase_policy,
    zero_phase_summary,
};
use resume::{PreparedResumeSeed, ResumePreparation};

#[cfg(test)]
use finalize::{sqlite_i64, sqlite_u64};

/// A file the coordinator is advancing through phases. `version_id`/`snapshot`
/// track the file's current chain tip and are refreshed after each commit.
#[derive(Debug, Clone)]
struct PhaseFile {
    pub(super) asset_id: FileAssetId,
    pub(super) version_id: FileVersionId,
    pub(super) snapshot: MediaSnapshot,
    pub(super) branch_id: String,
    pub(super) ordinal: u32,
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

/// A phase that failed during dispatch. `run_summary` is `Some` once the
/// executor actually ran the workflow (and so some files may have committed
/// inline before draining), `None` for a pre-dispatch bridge failure.
struct PhaseDispatchFailure {
    pub(super) source: VoomError,
    pub(super) run_summary: Option<crate::workflow::WorkflowRunSummary>,
    pub(super) job_failed: bool,
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
    promotion_job_ids: Vec<JobId>,
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
    gate_admission: Vec<bool>,
    error_strategy: voom_policy::ErrorStrategy,
}

#[cfg(test)]
type CoordinatorFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CoordinatorOutcome, CoordinatorError>> + Send + 'a>>;

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
    phases: Vec<PhaseSummary>,
    file_phases: Vec<FilePhaseSummary>,
    last_run: Option<crate::workflow::WorkflowRunSummary>,
    continued_error: Option<VoomError>,
    promotable_branches: BTreeSet<String>,
}

impl<'a> PhaseLoop<'a> {
    fn new(control_plane: &'a ControlPlane, inputs: PhaseLoopInputs) -> Self {
        // Derive promotion pairs from the operator output dirs before the options
        // are converted (the conversion repoints commit targets to working dirs).
        let promotion = inputs.options.promotion_plan();
        let executor = WorkflowExecutor::with_options(
            control_plane.clone(),
            inputs.runtimes,
            WorkflowExecutorOptions::from(inputs.options),
        );
        let promotable_branches = inputs
            .files
            .iter()
            .map(|file| file.branch_id.clone())
            .collect();
        Self {
            control_plane,
            job_id: inputs.job_id,
            policy: inputs.policy,
            context: inputs.context,
            base_draft: inputs.base_draft,
            executor,
            files: inputs.files,
            promotion,
            phases: Vec::new(),
            file_phases: inputs.seed_file_phases,
            last_run: None,
            continued_error: None,
            promotable_branches,
        }
    }

    async fn run(self) -> Result<CoordinatorOutcome, CoordinatorError> {
        self.run_after_phase_plan(|_| std::future::ready(Ok(())))
            .await
    }

    async fn run_after_phase_plan<F, Fut>(
        mut self,
        mut after_phase_plan: F,
    ) -> Result<CoordinatorOutcome, CoordinatorError>
    where
        F: FnMut(u32) -> Fut,
        Fut: Future<Output = Result<(), VoomError>>,
    {
        let phase_order = self.policy.phase_order.clone();
        for (index, phase_name) in phase_order.iter().enumerate() {
            if self.files.is_empty() {
                break;
            }
            let phase_ordinal = phase_ordinal(index)?;
            let Some(mut entry) = self.enter_phase(phase_ordinal) else {
                continue;
            };
            let mut planned = self.plan_phase_for_files(phase_name, &entry.entering)?;
            after_phase_plan(phase_ordinal).await?;
            if let Err(failure) = self
                .dispatch_phase_work(phase_ordinal, &entry.entering, &planned)
                .await
            {
                if !failure.job_failed {
                    let before = planned.dispositions.clone();
                    planned.dispositions = self
                        .control_plane
                        .continued_dispositions(self.job_id, phase_ordinal, &planned.dispositions)
                        .await?;
                    for ((file, before), after) in entry
                        .entering
                        .iter()
                        .zip(&before)
                        .zip(&planned.dispositions)
                    {
                        if let (Disposition::Planned { .. }, Disposition::Blocked) = (before, after)
                        {
                            self.promotable_branches.remove(&file.branch_id);
                        }
                    }
                    if let Some(summary) = failure.run_summary {
                        self.record_run(summary);
                    }
                    if self.continued_error.is_none() {
                        self.continued_error = Some(failure.source);
                    }
                    self.persist_phase_outcome(phase_ordinal, phase_name, &planned, &mut entry)
                        .await?;
                    self.recombine_survivors(entry);
                    continue;
                }
                return self
                    .persist_failed_phase(
                        phase_ordinal,
                        &entry.entering,
                        &planned.dispositions,
                        failure,
                    )
                    .await;
            }
            self.persist_phase_outcome(phase_ordinal, phase_name, &planned, &mut entry)
                .await?;
            self.recombine_survivors(entry);
        }

        self.finish().await
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
            gate_admission,
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

    async fn persist_phase_outcome(
        &mut self,
        phase_ordinal: u32,
        phase_name: &str,
        planned: &PlannedPhase,
        entry: &mut PhaseEntry,
    ) -> Result<(), VoomError> {
        let (rows, refreshed) = self
            .control_plane
            .finalize_phase(
                self.job_id,
                phase_ordinal,
                &mut entry.entering,
                &planned.dispositions,
            )
            .await?;
        let outcome = phase_outcome(&rows.iter().map(|row| row.outcome).collect::<Vec<_>>());
        self.file_phases.extend(rows);
        let report = regenerate_phase_report(
            &self.policy,
            &self.context,
            &self.base_draft,
            phase_name,
            &refreshed,
            &planned.gate_admission,
        )?;
        let phase_row = self
            .control_plane
            .workflow_summaries
            .upsert_phase_summary(
                NewPhaseSummary {
                    job_id: self.job_id,
                    phase_ordinal,
                    phase_name: phase_name.to_owned(),
                    report: Some(report),
                    outcome,
                },
                self.control_plane.clock().now(),
            )
            .await?;
        self.phases.push(phase_row);
        Ok(())
    }

    async fn persist_failed_phase(
        &mut self,
        phase_ordinal: u32,
        entering: &[PhaseFile],
        dispositions: &[Disposition],
        mut failure: PhaseDispatchFailure,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let phase_dispatched = failure.run_summary.is_some();
        if let Some(run) = failure.run_summary.take() {
            self.record_run(run);
        }
        let phases = std::mem::take(&mut self.phases);
        let file_phases = std::mem::take(&mut self.file_phases);
        self.control_plane
            .finalize_failed_phase(FailedPhaseFinalization {
                job_id: self.job_id,
                phase_ordinal,
                files: entering,
                dispositions,
                phase_dispatched,
                run_summary: self.last_run.as_ref(),
                source: failure.source,
                phases,
                file_phases,
            })
            .await
    }

    fn recombine_survivors(&mut self, entry: PhaseEntry) {
        self.files = entry.entering;
        self.files.extend(entry.passthrough);
    }

    async fn finish(self) -> Result<CoordinatorOutcome, CoordinatorError> {
        let Self {
            control_plane,
            job_id,
            promotion,
            phases,
            file_phases,
            last_run,
            continued_error,
            promotable_branches,
            ..
        } = self;
        let survivor_branches = promotable_branches.into_iter().collect::<Vec<_>>();
        // Promote each file's terminal artifact into --output-dir before the job
        // succeeds: a promotion conflict must fail the run, not leave a job
        // marked succeeded with finals stranded in the working dir. A promotion
        // failure here happens after every phase already committed, so carry the
        // accumulated phase/file rows in the error's partial outcome rather than
        // discarding the operator's execution diagnostics.
        let promotion_result = match control_plane
            .promotion_location_ids_for_branches(&file_phases, &survivor_branches)
            .await
        {
            Ok(ids) => {
                control_plane
                    .promote_terminal_artifacts(&promotion, &ids)
                    .await
            }
            Err(source) => Err(source),
        };
        if let Err(source) = promotion_result {
            let summary = control_plane
                .workflow_summaries
                .insert_summary(
                    job_grain_summary(job_id, last_run.as_ref()),
                    control_plane.clock().now(),
                )
                .await
                .map_err(CoordinatorError::from)?;
            return Err(CoordinatorError {
                source,
                partial: Some(CoordinatorOutcome {
                    job_id,
                    summary,
                    phases,
                    file_phases,
                }),
            });
        }
        if let Some(source) = continued_error {
            let now = control_plane.clock().now();
            let summary = control_plane
                .workflow_summaries
                .insert_summary(job_grain_summary(job_id, last_run.as_ref()), now)
                .await
                .map_err(CoordinatorError::from)?;
            control_plane
                .fail_job(job_id, source.to_string(), now)
                .await
                .map_err(CoordinatorError::from)?;
            return Err(CoordinatorError {
                source,
                partial: Some(CoordinatorOutcome {
                    job_id,
                    summary,
                    phases,
                    file_phases,
                }),
            });
        }
        control_plane
            .finalize_succeeded_run(job_id, last_run.as_ref(), phases, file_phases)
            .await
            .map_err(CoordinatorError::from)
    }
}

impl ControlPlane {
    async fn continued_dispositions(
        &self,
        job_id: JobId,
        phase_ordinal: u32,
        dispositions: &[Disposition],
    ) -> Result<Vec<Disposition>, VoomError> {
        let mut resolved = Vec::with_capacity(dispositions.len());
        for disposition in dispositions {
            let Disposition::Planned { node_ids } = disposition else {
                resolved.push(disposition.clone());
                continue;
            };
            let mut states = Vec::with_capacity(node_ids.len());
            for node_id in node_ids {
                let workflow_node_id = super::plan::policy_bridge::policy_workflow_node_id(node_id);
                let ticket_ids = self
                    .ticket_ids_for_phase_node(job_id, phase_ordinal, &workflow_node_id)
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
            .prepare_phase_barrier_run_inputs(policy_version_id, input_set_id, &runtimes)
            .await?;
        Box::pin(self.run_prepared_phase_barrier(inputs, options, runtimes)).await
    }

    pub(crate) async fn run_prepared_phase_barrier(
        &self,
        inputs: PhaseBarrierRunInputs,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let starts = run_starts_for_files(&inputs.files);
        let (job, _) = self
            .open_phase_barrier_job(&starts, Vec::new(), Vec::new())
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
        self.resume_phase_barrier_with_runtimes(
            prior_job_id,
            policy_version_id,
            input_set_id,
            options,
            runtimes,
        )
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
            .prepare_phase_barrier_run_inputs(policy_version_id, input_set_id, &runtimes)
            .await?;
        let prepared = self
            .prepare_resume_phase_barrier_run_inputs(prior_job_id, inputs)
            .await?;
        self.run_prepared_resume_phase_barrier(prior_job_id, prepared, options, runtimes)
            .await
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
        prior_job_id: JobId,
        inputs: PreparedResumeRunInputs,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
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
                },
        } = inputs;
        let (job, seed_file_phases) = self
            .open_phase_barrier_job(&run_starts, history, seeds)
            .await?;
        let result = self
            .drive_phase_loop(PhaseLoopInputs {
                job_id: job.id,
                policy,
                context,
                base_draft,
                files,
                seed_file_phases,
                promotion_job_ids: vec![job.id, prior_job_id],
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
        runtimes: &WorkerRuntimeRegistry,
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
            self.preflight_policy_tools(&mut policy, runtimes).await?;
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
        let mut tx = begin_immediate_tx(&self.pool).await?;
        crate::artifact::bootstrap::ensure_builtin_verify_artifact_worker_in_tx(self, &mut tx)
            .await?;
        commit_tx(tx).await
    }

    /// Open the owned workflow job, run the supplied in-job phase-barrier work,
    /// and fail the job on every error that escapes after opening.
    #[cfg(test)]
    async fn with_phase_barrier_job<'a, F>(
        &'a self,
        run: F,
    ) -> Result<CoordinatorOutcome, CoordinatorError>
    where
        F: FnOnce(JobId) -> CoordinatorFuture<'a>,
    {
        let (job, _) = self
            .open_phase_barrier_job(&[], Vec::new(), Vec::new())
            .await?;
        let result = run(job.id).await;
        self.finish_phase_barrier_job(job.id, result).await
    }

    async fn open_phase_barrier_job(
        &self,
        run_starts: &[NewFileRunStart],
        history: Vec<NewFileRunHistory>,
        seeds: Vec<PreparedResumeSeed>,
    ) -> Result<(voom_store::repo::jobs::Job, Vec<FilePhaseSummary>), VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
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
            Err(err) => {
                let _ = self
                    .fail_job(job_id, err.source.to_string(), self.clock().now())
                    .await;
                Err(err)
            }
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
            promotion_job_ids: vec![job_id],
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
                promotion_job_ids,
                options,
                ..
            } = inputs;
            // No phase loop runs (e.g. a resume where every file already
            // completed). Files that committed in a prior, failed job were never
            // promoted, so promote any terminal artifacts still in a working dir
            // now, before the job succeeds.
            let promotion = options.promotion_plan();
            let promotion_result = match self
                .promotion_location_ids(&promotion_job_ids, &seed_file_phases)
                .await
            {
                Ok(ids) => self.promote_terminal_artifacts(&promotion, &ids).await,
                Err(source) => Err(source),
            };
            if let Err(source) = promotion_result {
                let summary = self
                    .workflow_summaries
                    .insert_summary(zero_phase_summary(job_id), self.clock().now())
                    .await
                    .map_err(CoordinatorError::from)?;
                return Err(CoordinatorError {
                    source,
                    partial: Some(CoordinatorOutcome {
                        job_id,
                        summary,
                        phases: Vec::new(),
                        file_phases: seed_file_phases,
                    }),
                });
            }
            return Ok(self
                .finalize_zero_phase_run(job_id, seed_file_phases)
                .await?);
        }
        PhaseLoop::new(self, inputs).run().await
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
                    job_failed: true,
                }
            })?;
        let Some(scope) = scope else {
            return Ok(None);
        };
        let shape = WorkflowExecutionShape::new(scope.planned_count, scope.planned_count).map_err(
            |source| PhaseDispatchFailure {
                source,
                run_summary: None,
                job_failed: true,
            },
        )?;
        let bridge =
            workflow_plan_from_compliance(&planned_phase.plan, &planned_phase.report, shape)
                .map_err(|source| PhaseDispatchFailure {
                    source,
                    run_summary: None,
                    job_failed: true,
                })?;
        let Some(workflow) = bridge.workflow else {
            return Ok(None);
        };
        // On a ticket failure the executor drains every in-flight dispatch to a
        // terminal state (so any inline commit has landed) and fails the job;
        // carry its run summary so the partial outcome reports the job-cumulative
        // counts including the failure.
        let run = Box::pin(executor.submit_and_run_guarded_invocation_in_job(
            job_id,
            &format!("phase-{phase_ordinal}"),
            workflow,
            match planned_phase.error_strategy {
                voom_policy::ErrorStrategy::Continue => RunFailureMode::ContinueIndependent,
                voom_policy::ErrorStrategy::Abort | voom_policy::ErrorStrategy::Skip => {
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
            }
        })?;
        Ok(Some(run))
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
