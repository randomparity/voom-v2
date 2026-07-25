//! Multi-file phase-barrier coordinator (issue #162, Sprint 16 §3/§6).
//!
//! `run_phase_barrier` owns one job for the whole run (ADR-0007) and drives the
//! existing executor one phase at a time across every file in a policy input
//! set, phases acting as barriers across files. Each phase projects every
//! still-active file's current chain-tip snapshot into the planner
//! (`project_media_snapshot_input`), plans that one phase, bridges its planned
//! nodes to a workflow, and runs them in the owned job; blocked files drop,
//! compliant/skipped files stay, committed files advance their chain tip
//! (`active_version_with_snapshot`). It persists a durable per-phase /
//! per-`(file, phase)` workflow summary as it goes.
//!
//! Responsibility map of the child modules:
//! - [`planning`] — phase planning/policy projection and report/summary aggregation.
//! - [`promotion`] — terminal-artifact placement into the operator output dir.
//! - [`finalize`] — per-file/per-phase durable row writing and payload/sqlite helpers.
//! - [`resume`] — resume reconciliation and chain-tip/snapshot projection.

#[cfg(test)]
use std::future::Future;
#[cfg(test)]
use std::pin::Pin;

use voom_core::{FileAssetId, FileVersionId, JobId, PolicyInputSetId, PolicyVersionId, VoomError};
use voom_plan::{ExecutionPlan, PlanningContext, PlanningRequest};
use voom_policy::PolicyInputSetDraft;
use voom_store::repo::identity::MediaSnapshot;
use voom_store::repo::jobs::NewJob;
use voom_store::repo::workflow_summaries::{
    FilePhaseSummary, NewFileRunStart, NewPhaseSummary, PhaseSummary, WorkflowSummary,
};

use crate::ControlPlane;
use crate::cases::policy::compliance::{ComplianceExecutionOptions, PromotionPlan};
use crate::cases::policy::plans::plan_compiled_policy_with_input;
use crate::cases::{begin_tx, commit_tx};

use super::execution::WorkerRuntimeRegistry;
use super::execution::executor::{WORKFLOW_JOB_KIND, WorkflowExecutor, WorkflowExecutorOptions};
use super::plan::policy_bridge::{WorkflowExecutionShape, workflow_plan_from_compliance};

mod finalize;
mod planning;
mod promotion;
mod resume;

use finalize::phase_ordinal;
use planning::{
    classify_phase, initial_phase_files, job_grain_summary, phase_draft, phase_outcome,
    regenerate_phase_report, reject_unhandled_on_error, zero_phase_summary,
};
use resume::{PreparedResumeSeed, ResumePreparation};

#[cfg(test)]
use finalize::{sqlite_i64, sqlite_u64};
#[cfg(test)]
pub(crate) use resume::active_version_with_snapshot;
pub(crate) use resume::project_media_snapshot_input;

/// A file the coordinator is advancing through phases. `version_id`/`snapshot`
/// track the file's current chain tip and are refreshed after each commit.
#[derive(Debug)]
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

/// How a single file's phase node resolved (ADR-0005: at most one node status
/// per target when the phase runs).
enum Disposition {
    Blocked,
    Skipped,
    Planned { node_id: String },
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
}

/// Shared inputs for a fresh or resumed phase-barrier run. Everything here is
/// prepared before a new job opens, so validation failures do not create a job
/// that immediately needs cleanup.
pub(crate) struct PhaseBarrierRunInputs {
    policy: voom_policy::CompiledPolicy,
    context: PlanningContext,
    base_draft: PolicyInputSetDraft,
    pub(crate) initial_plan: ExecutionPlan,
    files: Vec<PhaseFile>,
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
    promotion_job_ids: Vec<JobId>,
    phases: Vec<PhaseSummary>,
    file_phases: Vec<FilePhaseSummary>,
    last_run: Option<crate::workflow::WorkflowRunSummary>,
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
        Self {
            control_plane,
            job_id: inputs.job_id,
            policy: inputs.policy,
            context: inputs.context,
            base_draft: inputs.base_draft,
            executor,
            files: inputs.files,
            promotion,
            promotion_job_ids: inputs.promotion_job_ids,
            phases: Vec::new(),
            file_phases: inputs.seed_file_phases,
            last_run: None,
        }
    }

    async fn run(mut self) -> Result<CoordinatorOutcome, CoordinatorError> {
        let phase_order = self.policy.phase_order.clone();
        for (index, phase_name) in phase_order.iter().enumerate() {
            if self.files.is_empty() {
                break;
            }
            let phase_ordinal = phase_ordinal(index)?;
            let Some(mut entry) = self.enter_phase(phase_ordinal) else {
                continue;
            };
            let planned = self.plan_phase_for_files(phase_name, &entry.entering)?;
            if let Err(failure) = self.dispatch_phase_work(&planned).await {
                return self
                    .persist_failed_phase(
                        phase_ordinal,
                        &entry.entering,
                        &planned.dispositions,
                        failure,
                    )
                    .await;
            }
            self.persist_phase_outcome(
                phase_ordinal,
                phase_name,
                &planned.dispositions,
                &mut entry,
            )
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
        let draft = phase_draft(&self.base_draft, entering);
        let plan = voom_plan::plan_phase(
            PlanningRequest {
                policy: self.policy.clone(),
                input: draft,
                context: self.context.clone(),
            },
            phase_name,
        )
        .map_err(voom_plan::PlanGenerationError::into_voom_error)?;
        let report = voom_plan::generate_compliance_report(&plan)
            .map_err(voom_plan::ComplianceReportError::into_voom_error)?;
        let dispositions = classify_phase(entering, &plan);
        Ok(PlannedPhase {
            plan,
            report,
            dispositions,
        })
    }

    async fn dispatch_phase_work(
        &mut self,
        planned: &PlannedPhase,
    ) -> Result<(), PhaseDispatchFailure> {
        let run = self
            .control_plane
            .dispatch_phase(
                &self.executor,
                self.job_id,
                &planned.plan,
                &planned.report,
                &planned.dispositions,
            )
            .await?;
        if let Some(run) = run {
            self.last_run = Some(run);
        }
        Ok(())
    }

    async fn persist_phase_outcome(
        &mut self,
        phase_ordinal: u32,
        phase_name: &str,
        dispositions: &[Disposition],
        entry: &mut PhaseEntry,
    ) -> Result<(), VoomError> {
        let (rows, refreshed) = self
            .control_plane
            .finalize_phase(
                self.job_id,
                phase_ordinal,
                &mut entry.entering,
                dispositions,
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
        failure: PhaseDispatchFailure,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let phases = std::mem::take(&mut self.phases);
        let file_phases = std::mem::take(&mut self.file_phases);
        self.control_plane
            .finalize_failed_phase(
                self.job_id,
                phase_ordinal,
                entering,
                dispositions,
                failure,
                phases,
                file_phases,
            )
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
            promotion_job_ids,
            phases,
            file_phases,
            last_run,
            ..
        } = self;
        // Promote each file's terminal artifact into --output-dir before the job
        // succeeds: a promotion conflict must fail the run, not leave a job
        // marked succeeded with finals stranded in the working dir. A promotion
        // failure here happens after every phase already committed, so carry the
        // accumulated phase/file rows in the error's partial outcome rather than
        // discarding the operator's execution diagnostics.
        let promotion_result = match control_plane
            .promotion_location_ids(&promotion_job_ids, &file_phases)
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
        control_plane
            .finalize_succeeded_run(job_id, last_run.as_ref(), phases, file_phases)
            .await
            .map_err(CoordinatorError::from)
    }
}

impl ControlPlane {
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
        let inputs = self
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
        let (job, _) = self.open_phase_barrier_job(&starts, Vec::new()).await?;
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
        let inputs = self
            .prepare_phase_barrier_run_inputs(policy_version_id, input_set_id, &runtimes)
            .await?;
        let phase_count = u32::try_from(inputs.policy.phase_order.len())
            .map_err(|e| VoomError::Internal(format!("phase count overflow: {e}")))?;
        let ResumePreparation {
            files,
            run_starts,
            seeds,
        } = self
            .prepare_resume(prior_job_id, inputs.files, phase_count)
            .await?;
        let (job, seed_file_phases) = self.open_phase_barrier_job(&run_starts, seeds).await?;
        let result = self
            .drive_phase_loop(PhaseLoopInputs {
                job_id: job.id,
                policy: inputs.policy,
                context: inputs.context,
                base_draft: inputs.base_draft,
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
    ) -> Result<PhaseBarrierRunInputs, VoomError> {
        let inputs = self
            .load_current_accepted_policy_and_input(policy_version_id, input_set_id)
            .await?;
        let mut policy = self.compiled_policy_for_version(&inputs.version).await?;
        reject_unhandled_on_error(&policy)?;
        self.preflight_policy_tools(&mut policy, runtimes).await?;
        let stored = self
            .resolve_stored_planning_input(&policy, inputs.input)
            .await?;
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
        Ok(PhaseBarrierRunInputs {
            policy,
            context,
            base_draft: stored.draft,
            initial_plan,
            files,
        })
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
        let (job, _) = self.open_phase_barrier_job(&[], Vec::new()).await?;
        let result = run(job.id).await;
        self.finish_phase_barrier_job(job.id, result).await
    }

    async fn open_phase_barrier_job(
        &self,
        run_starts: &[NewFileRunStart],
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
        let mut rows = Vec::with_capacity(seeds.len());
        for seed in seeds {
            rows.push(
                self.workflow_summaries
                    .upsert_file_phase_summary_in_tx(
                        &mut tx,
                        seed.produced
                            .committed_seed(job.id, seed.phase_ordinal, seed.branch_id),
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
            initial_plan: _,
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
        plan: &ExecutionPlan,
        report: &voom_plan::ComplianceReport,
        dispositions: &[Disposition],
    ) -> Result<Option<crate::workflow::WorkflowRunSummary>, PhaseDispatchFailure> {
        let planned = dispositions
            .iter()
            .filter(|d| matches!(d, Disposition::Planned { .. }))
            .count();
        if planned == 0 {
            return Ok(None);
        }
        let shape = WorkflowExecutionShape::new(planned, planned).map_err(|source| {
            PhaseDispatchFailure {
                source,
                run_summary: None,
            }
        })?;
        let bridge = workflow_plan_from_compliance(plan, report, shape).map_err(|source| {
            PhaseDispatchFailure {
                source,
                run_summary: None,
            }
        })?;
        let Some(workflow) = bridge.workflow else {
            return Ok(None);
        };
        // On a ticket failure the executor drains every in-flight dispatch to a
        // terminal state (so any inline commit has landed) and fails the job;
        // carry its run summary so the partial outcome reports the job-cumulative
        // counts including the failure.
        let run = executor
            .submit_and_run_in_job(job_id, workflow)
            .await
            .map_err(|err| PhaseDispatchFailure {
                source: err.source,
                run_summary: Some(err.summary),
            })?;
        Ok(Some(run))
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
