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

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use sqlx::Row;
use voom_core::{
    FileAssetId, FileLocationId, FileVersionId, JobId, MediaSnapshotId, PolicyInputSetId,
    PolicyVersionId, TicketId, VoomError,
};
use voom_plan::{ExecutionPlan, NodeStatus, PlanningContext, PlanningRequest};
use voom_policy::{MediaSnapshotInput, PolicyInputSetDraft, TargetRef};
use voom_store::repo::identity::{FileLocationKind, FileVersion, IdentityRepo, MediaSnapshot};
use voom_store::repo::jobs::NewJob;
use voom_store::repo::policy_inputs::PolicyInputTargetRef;
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, NewFilePhaseSummary, NewPhaseSummary, NewWorkflowSummary,
    PhaseOutcome, PhaseReport, PhaseSummary, WorkflowSummary, WorkflowSummaryRepo,
};

use crate::ControlPlane;
use crate::cases::policy::compliance::ComplianceExecutionOptions;
use crate::cases::policy::plans::input_set_to_draft;
use crate::cases::policy::policy_inputs::stream_summary_from_snapshot_payload;

use super::execution::WorkerRuntimeRegistry;
use super::execution::executor::{WORKFLOW_JOB_KIND, WorkflowExecutor, WorkflowExecutorOptions};
use super::plan::expansion::branch_id_from_path;
use super::plan::policy_bridge::{WorkflowExecutionShape, workflow_plan_from_compliance};

/// Bridge node ids carry this prefix; the per-file ticket lookup reconstructs the
/// workflow node id from a plan node id (`policy_bridge.rs`).
const POLICY_NODE_ID_PREFIX: &str = "policy-node_";

/// A file the coordinator is advancing through phases. `version_id`/`snapshot`
/// track the file's current chain tip and are refreshed after each commit.
struct PhaseFile {
    asset_id: FileAssetId,
    version_id: FileVersionId,
    /// The input-set starting version (chain root for this run). The resume
    /// backfill consistency guard compares the current tip against this when no
    /// committed row is visible (#165).
    start_version_id: FileVersionId,
    snapshot: MediaSnapshot,
    branch_id: String,
    ordinal: u32,
    /// First phase ordinal this file participates in (`0` for a fresh run; set by
    /// resume reconciliation). The loop passes a file through phases below this
    /// untouched (#165).
    resume_ordinal: u32,
}

/// How a single file's phase node resolved (ADR-0005: at most one node status
/// per target when the phase runs).
enum Disposition {
    Blocked,
    Skipped,
    Planned { node_id: String },
}

/// Classify each active file's node for a phase by `NodeStatus`. A file with no
/// node (its target was skipped via `run_if`/`skip_if`) is `Skipped`.
fn classify_phase(files: &[PhaseFile], plan: &ExecutionPlan) -> Vec<Disposition> {
    files
        .iter()
        .map(|file| {
            let node = plan.nodes.iter().find(|node| {
                matches!(node.target, TargetRef::FileVersion { id } if id == file.version_id)
            });
            match node {
                Some(node) => match node.status {
                    NodeStatus::Blocked => Disposition::Blocked,
                    NodeStatus::NoOp => Disposition::Skipped,
                    NodeStatus::Planned => Disposition::Planned {
                        node_id: node.node_id.clone(),
                    },
                },
                None => Disposition::Skipped,
            }
        })
        .collect()
}

/// Roll the per-file outcomes up to the phase grain (plan §3 step 6).
fn phase_outcome(file_outcomes: &[FilePhaseOutcome]) -> PhaseOutcome {
    if file_outcomes.is_empty() {
        return PhaseOutcome::Skipped;
    }
    let any_committed = file_outcomes.contains(&FilePhaseOutcome::Committed);
    let any_blocked = file_outcomes.contains(&FilePhaseOutcome::Blocked);
    if file_outcomes
        .iter()
        .all(|outcome| *outcome == FilePhaseOutcome::Committed)
    {
        PhaseOutcome::Completed
    } else if any_committed {
        PhaseOutcome::PartiallyCommitted
    } else if any_blocked {
        PhaseOutcome::Blocked
    } else {
        PhaseOutcome::Skipped
    }
}

/// Reject a policy whose any `phase_order` phase declares a non-default
/// `on_error` strategy. `continue`/`skip` are deferred this sprint (Sprint 16
/// §11); honoring them partially would be indistinguishable at runtime from real
/// handling, so they are rejected at resolve time before any job opens (#165).
fn reject_unhandled_on_error(policy: &voom_policy::CompiledPolicy) -> Result<(), VoomError> {
    for phase_name in &policy.phase_order {
        let Some(phase) = policy.phases.iter().find(|phase| phase.name == *phase_name) else {
            continue;
        };
        let label = match phase.on_error {
            None | Some(voom_policy::ErrorStrategy::Abort) => continue,
            Some(voom_policy::ErrorStrategy::Continue) => "continue",
            Some(voom_policy::ErrorStrategy::Skip) => "skip",
        };
        return Err(VoomError::PolicyValidationError(format!(
            "phase `{phase_name}` declares on_error `{label}`, which is not supported this sprint \
             (only the default abort); see Sprint 16 §11"
        )));
    }
    Ok(())
}

/// Build a phase's planning input: the input set's identity with each still-active
/// file's current snapshot projected in place of the original snapshots.
fn phase_draft(base: &PolicyInputSetDraft, files: &[PhaseFile]) -> PolicyInputSetDraft {
    let mut draft = base.clone();
    draft.media_snapshots = files
        .iter()
        .map(|file| project_media_snapshot_input(file.ordinal, &file.snapshot))
        .collect();
    draft
}

/// Regenerate the per-phase compliance report against the phase's refreshed facts
/// (ADR-0008): re-project every file that *entered* the phase at its refreshed
/// chain tip (committed files at their produced version + re-probe snapshot,
/// others unchanged), re-plan the same phase, and generate the report. Pure: the
/// `refreshed` snapshots are supplied by `finalize_phase`, so this does no
/// database reads, dispatches no tickets, advances no version, and adds no phase.
fn regenerate_phase_report(
    policy: &voom_policy::CompiledPolicy,
    context: &PlanningContext,
    base_draft: &PolicyInputSetDraft,
    phase_name: &str,
    refreshed: &[(u32, MediaSnapshot)],
) -> Result<PhaseReport, VoomError> {
    let mut draft = base_draft.clone();
    draft.media_snapshots = refreshed
        .iter()
        .map(|(ordinal, snapshot)| project_media_snapshot_input(*ordinal, snapshot))
        .collect();
    let plan = voom_plan::plan_phase(
        PlanningRequest {
            policy: policy.clone(),
            input: draft,
            context: context.clone(),
        },
        phase_name,
    )
    .map_err(voom_plan::PlanGenerationError::into_voom_error)?;
    let report = voom_plan::generate_compliance_report(&plan)
        .map_err(voom_plan::ComplianceReportError::into_voom_error)?;
    Ok(PhaseReport {
        report_id: report.report_id.clone(),
        report: serde_json::to_value(&report)
            .map_err(|e| VoomError::Internal(format!("phase report encode: {e}")))?,
    })
}

/// Job-grain summary counters from the last phase that dispatched work (counts
/// are job-cumulative, so the final run reflects the whole job), or zeros when
/// no phase dispatched.
fn job_grain_summary(
    job_id: JobId,
    run: Option<&crate::workflow::WorkflowRunSummary>,
) -> NewWorkflowSummary {
    match run {
        Some(run) => NewWorkflowSummary {
            job_id,
            branch_count: run.branch_count,
            ticket_count: run.ticket_count,
            dispatch_count: run.dispatch_count,
            retry_count: run.retry_count,
            failure_count: run.failure_count,
            peak_active_workflow_leases: run.peak_active_workflow_leases,
            elapsed: run.elapsed,
            per_operation: per_operation_json(run),
        },
        None => NewWorkflowSummary {
            job_id,
            branch_count: 0,
            ticket_count: 0,
            dispatch_count: 0,
            retry_count: 0,
            failure_count: 0,
            peak_active_workflow_leases: 0,
            elapsed: Duration::ZERO,
            per_operation: json!({}),
        },
    }
}

/// Per-operation counters as an opaque JSON object keyed by operation name (the
/// store keeps `per_operation` decoupled from the executor's summary type).
fn per_operation_json(run: &crate::workflow::WorkflowRunSummary) -> Value {
    let map = run
        .per_operation
        .iter()
        .map(|(kind, summary)| {
            (
                kind.as_str().to_owned(),
                json!({
                    "ticket_count": summary.ticket_count,
                    "dispatch_count": summary.dispatch_count,
                    "success_count": summary.success_count,
                    "retry_count": summary.retry_count,
                    "failure_count": summary.failure_count,
                }),
            )
        })
        .collect::<serde_json::Map<String, Value>>();
    Value::Object(map)
}

/// The durable references a committed file-phase row requires (NOT NULL by DB
/// CHECK): the produced version, its live location, and its reprobe snapshot.
#[derive(Default)]
#[expect(
    clippy::struct_field_names,
    reason = "fields mirror the NewFilePhaseSummary produced_*/reprobe_* id columns"
)]
struct ProducedRefs {
    file_version_id: Option<FileVersionId>,
    file_location_id: Option<FileLocationId>,
    reprobe_snapshot_id: Option<MediaSnapshotId>,
}

impl ProducedRefs {
    async fn resolve(
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
    source: VoomError,
    run_summary: Option<crate::workflow::WorkflowRunSummary>,
}

/// Shared inputs for a fresh or resumed phase-barrier run. Everything here is
/// prepared before a new job opens, so validation failures do not create a job
/// that immediately needs cleanup.
struct PhaseBarrierRunInputs {
    policy: voom_policy::CompiledPolicy,
    context: PlanningContext,
    base_draft: PolicyInputSetDraft,
    branch_ids: Vec<(FileVersionId, String)>,
}

type CoordinatorFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CoordinatorOutcome, CoordinatorError>> + Send + 'a>>;

impl ControlPlane {
    /// Drive the existing workflow executor one phase at a time across every
    /// file in a policy input set, phases acting as barriers across files
    /// (issue #162, Sprint 16 §3/§6). The coordinator owns one job for the whole
    /// run (ADR-0007) and persists a durable per-phase / per-`(file, phase)`
    /// summary.
    ///
    /// # Errors
    /// Returns [`crate::workflow::CoordinatorError`] when durable inputs are
    /// missing, the policy fails to compile, or a phase's tickets fail. Any
    /// error after the job opens finalizes the job as `failed`.
    pub async fn run_phase_barrier(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let runtimes = self.policy_runtime_registry().await?;
        self.run_phase_barrier_with_runtimes(policy_version_id, input_set_id, options, runtimes)
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
            .phase_barrier_run_inputs(policy_version_id, input_set_id)
            .await?;
        let inputs = Box::new(inputs);
        self.with_phase_barrier_job(|job_id| {
            Box::pin(async move {
                self.run_phase_barrier_in_job(job_id, *inputs, options, runtimes)
                    .await
            })
        })
        .await
    }

    /// Resume a crashed or failed phase-barrier run (issue #165, spec §3/§8).
    /// Opens a **new** job and reconciles each file against `prior_job_id`'s
    /// per-`(file, phase)` rows (ADR-0009). Pass the **most-recently-failed**
    /// run's job id (the latest
    /// [`crate::workflow::CoordinatorError`].`partial.job_id`).
    ///
    /// # Errors
    /// Returns [`crate::workflow::CoordinatorError`] when `prior_job_id` does
    /// not exist, durable inputs are missing, the policy declares an unsupported
    /// `on_error`, or a phase's tickets fail.
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
        use voom_store::repo::jobs::JobRepo;
        if self.jobs.get(prior_job_id).await?.is_none() {
            return Err(VoomError::NotFound(format!(
                "resume: prior job {prior_job_id} does not exist"
            ))
            .into());
        }
        let inputs = self
            .phase_barrier_run_inputs(policy_version_id, input_set_id)
            .await?;
        let inputs = Box::new(inputs);

        self.with_phase_barrier_job(|job_id| {
            Box::pin(async move {
                self.resume_phase_barrier_in_job(job_id, prior_job_id, *inputs, options, runtimes)
                    .await
            })
        })
        .await
    }

    async fn resume_phase_barrier_in_job(
        &self,
        job_id: JobId,
        prior_job_id: JobId,
        inputs: PhaseBarrierRunInputs,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let PhaseBarrierRunInputs {
            policy,
            context,
            base_draft,
            branch_ids,
        } = inputs;
        if branch_ids.is_empty() || policy.phase_order.is_empty() {
            return Ok(self.finalize_zero_phase_run(job_id, Vec::new()).await?);
        }
        let files = self.initial_phase_files(&branch_ids).await?;
        let phase_count = u32::try_from(policy.phase_order.len())
            .map_err(|e| VoomError::Internal(format!("phase count overflow: {e}")))?;
        let (files, backfilled) = self
            .reconcile_resume(prior_job_id, job_id, files, phase_count)
            .await?;
        self.drive_phase_loop(
            job_id, &policy, &context, base_draft, files, backfilled, options, runtimes,
        )
        .await
    }

    /// Prepare all shared phase-barrier inputs that are independent of the new
    /// job. Both fresh and resume runs use the same policy/input identity,
    /// projected planning context, base draft, and active branch-id set.
    async fn phase_barrier_run_inputs(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<PhaseBarrierRunInputs, VoomError> {
        let inputs = self
            .load_current_accepted_policy_and_input(policy_version_id, input_set_id)
            .await?;
        let policy = self.compiled_policy_for_version(&inputs.version).await?;
        reject_unhandled_on_error(&policy)?;
        let active: Vec<FileVersionId> = inputs
            .input
            .media_snapshots
            .iter()
            .filter_map(|snapshot| match snapshot.target {
                PolicyInputTargetRef::FileVersion { id } => Some(id),
                _ => None,
            })
            .collect();

        // Carry the input set's non-snapshot identity forward; each phase only
        // swaps in the projected snapshots of the still-active files.
        let base_draft = input_set_to_draft(inputs.input);
        let context = PlanningContext {
            policy_version_id: Some(policy_version_id),
            policy_input_set_id: Some(input_set_id),
            ..PlanningContext::default()
        };

        // Derive each active file's branch id and fail fast on a collision
        // *before* opening the job: the per-`(file, phase)` upsert is
        // `ON CONFLICT DO NOTHING` and would silently drop a colliding file's
        // row, losing it from the durable summary.
        let branch_ids = self.active_branch_ids(&active).await?;
        Ok(PhaseBarrierRunInputs {
            policy,
            context,
            base_draft,
            branch_ids,
        })
    }

    /// Open the owned workflow job, run the supplied in-job phase-barrier work,
    /// and fail the job on every error that escapes after opening.
    async fn with_phase_barrier_job<'a, F>(
        &'a self,
        run: F,
    ) -> Result<CoordinatorOutcome, CoordinatorError>
    where
        F: FnOnce(JobId) -> CoordinatorFuture<'a>,
    {
        let job = self
            .open_job(NewJob {
                kind: WORKFLOW_JOB_KIND.to_owned(),
                priority: 0,
                created_at: self.clock().now(),
            })
            .await?;

        // Job-cleanup contract: once the job is open, every error path finalizes
        // it as `failed` rather than orphaning it in `open`. A dispatch failure
        // already failed the job inside `run_plan_in_job` (and `fail_job` is a
        // no-op on an already-failed job), so this `fail_job` only matters for
        // pre-dispatch errors that leave the job open. Committed per-`(file,
        // phase)` rows are durable before the error returns (queryable via
        // `file_phases_for_job` and carried in `partial`), satisfying ADR-0007.
        match run(job.id).await {
            Ok(outcome) => Ok(outcome),
            Err(err) => {
                let _ = self
                    .fail_job(job.id, err.source.to_string(), self.clock().now())
                    .await;
                Err(err)
            }
        }
    }

    /// Derive a stable branch id (the file's location path stem) for every
    /// active file, rejecting a stem collision across the set.
    async fn active_branch_ids(
        &self,
        active: &[FileVersionId],
    ) -> Result<Vec<(FileVersionId, String)>, VoomError> {
        let mut branch_ids = Vec::with_capacity(active.len());
        let mut seen: HashMap<String, FileVersionId> = HashMap::with_capacity(active.len());
        for &file_version_id in active {
            let branch_id = self.file_branch_id(file_version_id).await?;
            if let Some(previous) = seen.insert(branch_id.clone(), file_version_id) {
                return Err(VoomError::Config(format!(
                    "active files {previous} and {file_version_id} both derive branch id \
                     `{branch_id}`; phase-barrier summaries require a unique branch id per file"
                )));
            }
            branch_ids.push((file_version_id, branch_id));
        }
        Ok(branch_ids)
    }

    /// A file's branch id is the stem of its live location path, matching the
    /// scanner-completion binding (`expansion::branch_id_from_path`).
    async fn file_branch_id(&self, file_version_id: FileVersionId) -> Result<String, VoomError> {
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
        branch_id_from_path(&path)
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
            branch_ids,
        } = inputs;
        if branch_ids.is_empty() || policy.phase_order.is_empty() {
            return Ok(self.finalize_zero_phase_run(job_id, Vec::new()).await?);
        }
        let files = self.initial_phase_files(&branch_ids).await?;
        self.drive_phase_loop(
            job_id,
            &policy,
            &context,
            base_draft,
            files,
            Vec::new(),
            options,
            runtimes,
        )
        .await
    }

    /// Run the phase loop across `files`, each file participating only in phases
    /// at or above its `resume_ordinal` (`0` for a fresh run). `seed_file_phases`
    /// pre-loads rows a resume backfilled before the loop. Files below their
    /// `resume_ordinal` pass through a phase untouched and rejoin at their own
    /// resume phase (#165).
    #[expect(
        clippy::too_many_arguments,
        reason = "one owned job's run state plus the pre-seeded resume rows"
    )]
    async fn drive_phase_loop(
        &self,
        job_id: JobId,
        policy: &voom_policy::CompiledPolicy,
        context: &PlanningContext,
        base_draft: PolicyInputSetDraft,
        mut files: Vec<PhaseFile>,
        seed_file_phases: Vec<FilePhaseSummary>,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        if files.is_empty() || policy.phase_order.is_empty() {
            return Ok(self
                .finalize_zero_phase_run(job_id, seed_file_phases)
                .await?);
        }
        let executor = WorkflowExecutor::with_options(
            self.clone(),
            runtimes,
            WorkflowExecutorOptions::from(options),
        );

        let mut phases = Vec::new();
        let mut file_phases = seed_file_phases;
        let mut last_run = None;
        for (index, phase_name) in policy.phase_order.iter().enumerate() {
            if files.is_empty() {
                break;
            }
            let phase_ordinal = u32::try_from(index)
                .map_err(|e| VoomError::Internal(format!("phase ordinal overflow: {e}")))?;
            // Only files that have reached their resume phase enter this phase;
            // the rest pass through untouched and rejoin at their own ordinal.
            let (mut entering, passthrough): (Vec<PhaseFile>, Vec<PhaseFile>) =
                std::mem::take(&mut files)
                    .into_iter()
                    .partition(|file| file.resume_ordinal <= phase_ordinal);
            if entering.is_empty() {
                files = passthrough;
                continue;
            }
            let draft = phase_draft(&base_draft, &entering);
            let plan = voom_plan::plan_phase(
                PlanningRequest {
                    policy: policy.clone(),
                    input: draft,
                    context: context.clone(),
                },
                phase_name,
            )
            .map_err(voom_plan::PlanGenerationError::into_voom_error)?;
            let report = voom_plan::generate_compliance_report(&plan)
                .map_err(voom_plan::ComplianceReportError::into_voom_error)?;
            let dispositions = classify_phase(&entering, &plan);

            let run = match self
                .dispatch_phase(&executor, job_id, &plan, &report, &dispositions)
                .await
            {
                Ok(run) => run,
                Err(failure) => {
                    return self
                        .finalize_failed_phase(
                            job_id,
                            phase_ordinal,
                            &entering,
                            &dispositions,
                            failure,
                            phases,
                            file_phases,
                        )
                        .await;
                }
            };
            if run.is_some() {
                last_run = run;
            }
            let (rows, refreshed) = self
                .finalize_phase(job_id, phase_ordinal, &mut entering, &dispositions)
                .await?;
            let outcome = phase_outcome(&rows.iter().map(|row| row.outcome).collect::<Vec<_>>());
            file_phases.extend(rows);
            let report =
                regenerate_phase_report(policy, context, &base_draft, phase_name, &refreshed)?;
            let phase_row = self
                .workflow_summaries
                .upsert_phase_summary(
                    NewPhaseSummary {
                        job_id,
                        phase_ordinal,
                        phase_name: phase_name.clone(),
                        report: Some(report),
                        outcome,
                    },
                    self.clock().now(),
                )
                .await?;
            phases.push(phase_row);

            // Recombine the phase's survivors with the files still waiting for
            // their resume phase.
            files = entering;
            files.extend(passthrough);
        }

        self.finalize_succeeded_run(job_id, last_run.as_ref(), phases, file_phases)
            .await
            .map_err(CoordinatorError::from)
    }

    /// Succeed the owned job and write its job-grain summary, returning the
    /// completed [`CoordinatorOutcome`].
    async fn finalize_succeeded_run(
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

    /// Resolve every active file's current chain tip (and its latest snapshot)
    /// into the per-phase working set.
    async fn initial_phase_files(
        &self,
        branch_ids: &[(FileVersionId, String)],
    ) -> Result<Vec<PhaseFile>, VoomError> {
        let mut files = Vec::with_capacity(branch_ids.len());
        for (index, (version_id, branch_id)) in branch_ids.iter().enumerate() {
            let version = self
                .identity
                .get_file_version(*version_id)
                .await?
                .ok_or_else(|| {
                    VoomError::NotFound(format!("file version {version_id} not found"))
                })?;
            let (tip, snapshot) =
                active_version_with_snapshot(&self.identity, version.file_asset_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::NotFound(format!(
                            "file version {version_id} has no active snapshot to project"
                        ))
                    })?;
            files.push(PhaseFile {
                asset_id: version.file_asset_id,
                version_id: tip.id,
                start_version_id: *version_id,
                snapshot,
                branch_id: branch_id.clone(),
                ordinal: u32::try_from(index + 1)
                    .map_err(|e| VoomError::Internal(format!("file ordinal overflow: {e}")))?,
                resume_ordinal: 0,
            });
        }
        Ok(files)
    }

    /// Compute each active file's `resume_ordinal` from the most-recent failed
    /// job's per-`(file, phase)` rows (spec §3.1). Drops files that are terminal
    /// (`Blocked` at their highest recorded phase) or complete
    /// (`resume_ordinal >= phase_count`). Backfills a `Committed` row for any file
    /// whose chain tip advanced past its highest recorded committed version
    /// (a crash between the inline commit and the row write, or a stale prior id).
    /// Returns the surviving files (with `resume_ordinal` set) and the rows it
    /// backfilled (#165).
    async fn reconcile_resume(
        &self,
        prior_job_id: JobId,
        job_id: JobId,
        files: Vec<PhaseFile>,
        phase_count: u32,
    ) -> Result<(Vec<PhaseFile>, Vec<FilePhaseSummary>), VoomError> {
        let prior = self
            .workflow_summaries
            .file_phases_for_job(prior_job_id)
            .await?;
        let mut survivors = Vec::with_capacity(files.len());
        let mut backfilled = Vec::new();
        for mut file in files {
            let rows: Vec<&FilePhaseSummary> = prior
                .iter()
                .filter(|row| row.branch_id == file.branch_id)
                .collect();
            let highest = rows.iter().max_by_key(|row| row.phase_ordinal);
            if highest.is_some_and(|top| top.outcome == FilePhaseOutcome::Blocked) {
                continue; // terminal: aborted-for-file under the prior run
            }
            let mut resume_ordinal = highest.map_or(0, |top| top.phase_ordinal + 1);

            // Consistency backfill: default the recorded tip to the input-set
            // starting version when no committed row is visible.
            let recorded_tip = rows
                .iter()
                .filter(|row| row.outcome == FilePhaseOutcome::Committed)
                .max_by_key(|row| row.phase_ordinal)
                .and_then(|row| row.produced_file_version_id)
                .unwrap_or(file.start_version_id);
            if file.version_id != recorded_tip {
                let tip = self
                    .identity
                    .get_file_version(file.version_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::Internal(format!(
                            "resume: chain tip {} vanished for {}",
                            file.version_id, file.branch_id
                        ))
                    })?;
                let produced = ProducedRefs::resolve(self, &tip, &file.snapshot).await?;
                let row = self
                    .write_file_row(
                        job_id,
                        resume_ordinal,
                        &file,
                        FilePhaseOutcome::Committed,
                        &[],
                        Some(produced),
                    )
                    .await?;
                backfilled.push(row);
                resume_ordinal += 1;
            }

            if resume_ordinal >= phase_count {
                continue; // complete: nothing left to run
            }
            file.resume_ordinal = resume_ordinal;
            survivors.push(file);
        }
        Ok((survivors, backfilled))
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

    /// Finalize a run whose phase failed during dispatch: record every file that
    /// committed inline before the failure (the executor drained in-flight
    /// dispatches, so their commits have landed), then return the partial
    /// outcome inside the error. No phase-grain row is written for the failed
    /// phase, and the job is already `failed`.
    #[expect(
        clippy::too_many_arguments,
        reason = "threads the in-progress run's accumulated phase/file rows into the partial"
    )]
    async fn finalize_failed_phase(
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
            let workflow_node_id = format!("{POLICY_NODE_ID_PREFIX}{node_id}");
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
    async fn finalize_phase(
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
                let workflow_node_id = format!("{POLICY_NODE_ID_PREFIX}{node_id}");
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

    async fn write_file_row(
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
    async fn ticket_ids_for_node(
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
        .map_err(|e| VoomError::Database(format!("phase ticket ids: {e}")))?;
        rows.into_iter()
            .map(|row| {
                let id: i64 = row
                    .try_get("id")
                    .map_err(|e| VoomError::Database(format!("phase ticket id: {e}")))?;
                u64::try_from(id)
                    .map(TicketId)
                    .map_err(|e| VoomError::Database(format!("phase ticket id negative: {e}")))
            })
            .collect()
    }

    /// Succeed the job and write a zero-count job-grain summary for a run with no
    /// active files or no declared phases (no work, no phase or file rows).
    async fn finalize_zero_phase_run(
        &self,
        job_id: JobId,
        seed_file_phases: Vec<FilePhaseSummary>,
    ) -> Result<CoordinatorOutcome, VoomError> {
        let now = self.clock().now();
        self.succeed_job(job_id, now).await?;
        let summary = self
            .workflow_summaries
            .insert_summary(
                NewWorkflowSummary {
                    job_id,
                    branch_count: 0,
                    ticket_count: 0,
                    dispatch_count: 0,
                    retry_count: 0,
                    failure_count: 0,
                    peak_active_workflow_leases: 0,
                    elapsed: Duration::ZERO,
                    per_operation: json!({}),
                },
                now,
            )
            .await?;
        Ok(CoordinatorOutcome {
            job_id,
            summary,
            phases: Vec::new(),
            file_phases: seed_file_phases,
        })
    }
}

fn first_stream_of_kind<'a>(payload: &'a Value, kind: &str) -> Option<&'a Value> {
    payload
        .get("streams")
        .and_then(Value::as_array)?
        .iter()
        .find(|stream| stream.get("kind").and_then(Value::as_str) == Some(kind))
}

fn payload_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Snapshot dimensions arrive as JSON `u64`, but planner dimensions are `u32`.
fn payload_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

/// Project a committed file version's reprobe [`MediaSnapshot`] into the planner
/// input the next phase plans against.
///
/// The reprobe payload (`scan::persist::snapshot_with_stream_ids` output) carries
/// `container.format_name` plus a `streams` array whose entries are tagged with a
/// `kind` (`video`/`audio`/`subtitle`). Top-level `container`, `video_codec`,
/// `width`, and `height` are lifted from the container object and the first video
/// stream; the full `streams` array is forwarded verbatim as `stream_summary` so
/// the planner's per-stream readers see refreshed facts.
pub(crate) fn project_media_snapshot_input(
    ordinal: u32,
    snapshot: &MediaSnapshot,
) -> MediaSnapshotInput {
    let payload = &snapshot.payload;
    let container = payload
        .get("container")
        .and_then(|container| payload_str(container, "format_name"));
    let video = first_stream_of_kind(payload, "video");
    let video_codec = video.and_then(|stream| payload_str(stream, "codec_name"));
    let width = video.and_then(|stream| payload_u32(stream, "width"));
    let height = video.and_then(|stream| payload_u32(stream, "height"));
    MediaSnapshotInput {
        ordinal,
        target: TargetRef::FileVersion {
            id: snapshot.file_version_id,
        },
        container,
        stream_summary: stream_summary_from_snapshot_payload(payload),
        video_codec,
        width,
        height,
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: Some(snapshot.id),
    }
}

/// Read a file asset's active version (chain tip = latest non-retired
/// `file_versions` row) and its latest [`MediaSnapshot`].
///
/// Returns `Ok(None)` when the asset has no live version, or when the live tip
/// has no recorded snapshot yet. The coordinator resolves `file_asset_id` from a
/// starting `FileVersionId` via `IdentityRepo::get_file_version`.
///
/// # Errors
/// Propagates repository read errors.
pub(crate) async fn active_version_with_snapshot(
    repo: &impl IdentityRepo,
    file_asset_id: FileAssetId,
) -> Result<Option<(FileVersion, MediaSnapshot)>, VoomError> {
    let versions = repo.list_file_versions_by_asset(file_asset_id).await?;
    let Some(tip) = versions
        .into_iter()
        .filter(|version| version.retired_at.is_none())
        .max_by_key(|version| version.id.0)
    else {
        return Ok(None);
    };
    let snapshots = repo.list_media_snapshots_by_version(tip.id).await?;
    let Some(snapshot) = snapshots.into_iter().max_by_key(|snapshot| snapshot.id.0) else {
        return Ok(None);
    };
    Ok(Some((tip, snapshot)))
}

#[cfg(test)]
#[path = "coordinator_test.rs"]
mod tests;
