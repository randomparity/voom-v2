use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;
use tokio::task::JoinSet;
use voom_core::{FailureClass, JobId, TicketId, VoomError, WorkerId};
use voom_scheduler::{SingleWorkerPerKindSelector, WorkerSelector, WorkerView};
use voom_store::repo::identity::IdentityRepo;
use voom_store::repo::jobs::NewJob;
use voom_store::repo::leases::NewLease;
use voom_store::repo::tickets::{NewTicket, Ticket, TicketRepo, TicketState};
use voom_worker_protocol::OperationKind;

use super::binding::{
    BranchContext, PolicyFileSource, render_default_payload, render_default_payload_with_fan_out,
    render_policy_extract_audio_payload, render_policy_remux_payload,
    render_policy_transcode_audio_payload, render_policy_transcode_payload,
};
use super::dispatch::{DispatchOutcome, DispatchTerminal, dispatch_ticket};
use super::expansion::{
    ExpansionContext, expand_backup_completion, expand_probe_completion, expand_quality_completion,
    expand_scanner_completion, expand_transform_completion,
};
use super::leases::{acquire_lease_with_retry, failure_class_for_error, time_duration};
use super::model::{WorkflowNode, WorkflowPlan};
use super::runtime::WorkerRuntimeRegistry;
pub use super::summary::{OperationSummary, WorkflowRunSummary};
use super::ticket_payload::{WorkflowTicketPayload, operation_name};
use super::timing::{EffectiveTiming, seeded_timing};
use crate::ControlPlane;

pub(crate) const WORKFLOW_JOB_KIND: &str = "synthetic.workflow";
const POLICY_NODE_ID_PREFIX: &str = "policy-node_";
const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_PROGRESS_IDLE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct WorkflowExecutor<S = SingleWorkerPerKindSelector> {
    control_plane: ControlPlane,
    selector: S,
    runtimes: WorkerRuntimeRegistry,
    options: WorkflowExecutorOptions,
}

#[derive(Debug, Clone)]
pub struct WorkflowExecutorOptions {
    pub lease_ttl: Duration,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
    pub progress_idle_timeout: Duration,
    pub ready_batch_size: u32,
    pub max_attempts: u32,
    pub transcode_staging_root: PathBuf,
    pub transcode_target_dir: PathBuf,
    pub remux_staging_root: PathBuf,
    pub remux_target_dir: PathBuf,
    pub audio_staging_root: PathBuf,
    pub audio_target_dir: PathBuf,
    pub chaos: WorkflowChaosOptions,
}

impl Default for WorkflowExecutorOptions {
    fn default() -> Self {
        Self {
            lease_ttl: DEFAULT_LEASE_TTL,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            heartbeat_timeout: DEFAULT_HEARTBEAT_TIMEOUT,
            progress_idle_timeout: DEFAULT_PROGRESS_IDLE_TIMEOUT,
            ready_batch_size: 64,
            max_attempts: 1,
            transcode_staging_root: PathBuf::from("/tmp/voom/transcode/staging"),
            transcode_target_dir: PathBuf::from("/tmp/voom/transcode/output"),
            remux_staging_root: PathBuf::from("/tmp/voom/remux/staging"),
            remux_target_dir: PathBuf::from("/tmp/voom/remux/output"),
            audio_staging_root: PathBuf::from("/tmp/voom/audio/staging"),
            audio_target_dir: PathBuf::from("/tmp/voom/audio/output"),
            chaos: WorkflowChaosOptions::default(),
        }
    }
}

impl WorkflowExecutorOptions {
    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            lease_ttl: Duration::from_secs(5),
            heartbeat_interval: Duration::from_millis(10),
            heartbeat_timeout: Duration::from_secs(5),
            progress_idle_timeout: Duration::from_secs(5),
            ready_batch_size: 64,
            max_attempts: 1,
            transcode_staging_root: PathBuf::from("/tmp/voom-test/transcode/staging"),
            transcode_target_dir: PathBuf::from("/tmp/voom-test/transcode/output"),
            remux_staging_root: PathBuf::from("/tmp/voom-test/remux/staging"),
            remux_target_dir: PathBuf::from("/tmp/voom-test/remux/output"),
            audio_staging_root: PathBuf::from("/tmp/voom-test/audio/staging"),
            audio_target_dir: PathBuf::from("/tmp/voom-test/audio/output"),
            chaos: WorkflowChaosOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowChaosOptions {
    pub disable_heartbeat_ticks: bool,
    pub suppress_heartbeat_operation: Option<OperationKind>,
    pub payload_modes: BTreeMap<OperationKind, String>,
}

impl WorkflowChaosOptions {
    #[must_use]
    pub fn suppress_heartbeats_for_operation(operation: OperationKind) -> Self {
        Self {
            suppress_heartbeat_operation: Some(operation),
            ..Self::default()
        }
    }

    pub fn set_payload_mode_for_operation(
        &mut self,
        operation: OperationKind,
        mode: impl Into<String>,
    ) {
        self.payload_modes.insert(operation, mode.into());
    }

    pub(super) fn suppresses_heartbeats_for(&self, operation: OperationKind) -> bool {
        self.disable_heartbeat_ticks || self.suppress_heartbeat_operation == Some(operation)
    }

    pub(super) fn payload_mode_for(&self, operation: OperationKind) -> Option<&str> {
        self.payload_modes.get(&operation).map(String::as_str)
    }
}

#[derive(Debug)]
pub struct WorkflowRunError {
    pub summary: WorkflowRunSummary,
    pub source: VoomError,
}

impl<S> WorkflowExecutor<S>
where
    S: WorkerSelector + Clone + Send + Sync + 'static,
{
    #[must_use]
    pub fn new(control_plane: ControlPlane, selector: S, runtimes: WorkerRuntimeRegistry) -> Self {
        Self::with_options(
            control_plane,
            selector,
            runtimes,
            WorkflowExecutorOptions::default(),
        )
    }

    #[must_use]
    pub fn with_options(
        control_plane: ControlPlane,
        selector: S,
        runtimes: WorkerRuntimeRegistry,
        options: WorkflowExecutorOptions,
    ) -> Self {
        Self {
            control_plane,
            selector,
            runtimes,
            options,
        }
    }

    pub async fn submit_and_run(
        &self,
        plan: WorkflowPlan,
    ) -> Result<WorkflowRunSummary, WorkflowRunError> {
        let started = Instant::now();
        if let Err(source) = plan
            .validate()
            .map_err(|e| VoomError::Config(format!("workflow plan invalid: {e}")))
        {
            let summary = WorkflowRunSummary::empty(JobId(0), started.elapsed());
            return Err(WorkflowRunError { summary, source });
        }

        let now = self.control_plane.clock().now();
        let job = match self
            .control_plane
            .open_job(NewJob {
                kind: WORKFLOW_JOB_KIND.to_owned(),
                priority: 0,
                created_at: now,
            })
            .await
        {
            Ok(job) => job,
            Err(source) => {
                let summary = WorkflowRunSummary::empty(JobId(0), started.elapsed());
                return Err(WorkflowRunError { summary, source });
            }
        };

        let summary = self.run_plan_in_job(job.id, plan, started).await?;
        let _ = self
            .control_plane
            .succeed_job(job.id, self.control_plane.clock().now())
            .await;
        Ok(summary)
    }

    /// Run one plan inside a caller-owned, already-open job.
    ///
    /// Unlike [`Self::submit_and_run`], this does not open or succeed the job:
    /// the caller owns the job lifecycle. The phase-barrier coordinator (#162)
    /// calls this once per phase against a single job and calls `succeed_job`
    /// itself after the last phase. On an in-phase ticket failure the job is
    /// failed here (whole job fails); on a plan-validation error the existing
    /// job is also failed since the caller cannot otherwise observe the cause.
    /// First caller is the phase-barrier coordinator (#162 Phase 3); shipped as
    /// a crate surface ahead of that caller, like `submit_and_run`.
    pub async fn submit_and_run_in_job(
        &self,
        job_id: JobId,
        plan: WorkflowPlan,
    ) -> Result<WorkflowRunSummary, WorkflowRunError> {
        let started = Instant::now();
        if let Err(source) = plan
            .validate()
            .map_err(|e| VoomError::Config(format!("workflow plan invalid: {e}")))
        {
            let _ = self
                .control_plane
                .fail_job(job_id, source.to_string(), self.control_plane.clock().now())
                .await;
            let summary = WorkflowRunSummary::empty(job_id, started.elapsed());
            return Err(WorkflowRunError { summary, source });
        }
        self.run_plan_in_job(job_id, plan, started).await
    }

    /// Drive a validated plan to completion within an open job.
    ///
    /// Creates the plan's root tickets and runs the dispatch loop until every
    /// ticket reaches a terminal state. **Never calls `succeed_job`** — on
    /// success it returns `Ok(summary)` leaving the job open for the caller to
    /// finalize. On an in-phase ticket failure it fails the job and returns the
    /// error. On terminal failure it first drains every in-flight dispatch to a
    /// terminal state (so any inline commit has landed) before failing the job,
    /// keeping a caller's post-run inspection race-free.
    #[expect(
        clippy::too_many_lines,
        reason = "workflow run loop keeps scheduler state and terminal handling together"
    )]
    async fn run_plan_in_job(
        &self,
        job_id: JobId,
        plan: WorkflowPlan,
        started: Instant,
    ) -> Result<WorkflowRunSummary, WorkflowRunError> {
        let now = self.control_plane.clock().now();
        let workflow_id = format!("workflow-{}", job_id.0);
        let mut summary = WorkflowRunSummary::empty(job_id, started.elapsed());

        if let Err(source) = self
            .create_root_tickets(&plan, &workflow_id, job_id, now)
            .await
        {
            let _ = self
                .control_plane
                .fail_job(job_id, source.to_string(), self.control_plane.clock().now())
                .await;
            summary
                .refresh_counts(&self.control_plane, job_id, started.elapsed())
                .await;
            return Err(WorkflowRunError { summary, source });
        }

        let mut reservations: HashMap<WorkerId, u32> = HashMap::new();
        let mut active = JoinSet::new();
        let mut terminal_error: Option<VoomError> = None;

        loop {
            while let Some(joined) = active.try_join_next() {
                self.process_joined_dispatch(
                    joined,
                    &plan,
                    &workflow_id,
                    job_id,
                    &mut reservations,
                    &mut summary,
                    &mut terminal_error,
                )
                .await;
            }

            summary
                .refresh_counts(&self.control_plane, job_id, started.elapsed())
                .await;
            if let Some(source) = terminal_error.take() {
                // Drain every still-running dispatch to a terminal state before
                // failing the job, so any inline commit has landed (or released
                // its lease) rather than being aborted when `active` is dropped.
                // Keeps a caller's post-run inspection race-free.
                while let Some(joined) = active.join_next().await {
                    self.process_joined_dispatch(
                        joined,
                        &plan,
                        &workflow_id,
                        job_id,
                        &mut reservations,
                        &mut summary,
                        &mut terminal_error,
                    )
                    .await;
                }
                let _ = self
                    .control_plane
                    .fail_job(job_id, source.to_string(), self.control_plane.clock().now())
                    .await;
                summary
                    .refresh_counts(&self.control_plane, job_id, started.elapsed())
                    .await;
                return Err(WorkflowRunError { summary, source });
            }
            if active.is_empty() && self.workflow_finished(job_id).await {
                if let Some(source) = self.first_failed_ticket_error(job_id).await {
                    let _ = self
                        .control_plane
                        .fail_job(job_id, source.to_string(), self.control_plane.clock().now())
                        .await;
                    summary
                        .refresh_counts(&self.control_plane, job_id, started.elapsed())
                        .await;
                    return Err(WorkflowRunError { summary, source });
                }
                summary
                    .refresh_counts(&self.control_plane, job_id, started.elapsed())
                    .await;
                return Ok(summary);
            }

            let mut dispatched_or_failed = false;
            let max_in_flight = plan.concurrency.max_in_flight_dispatches;
            while active.len() < max_in_flight {
                let Some(ticket) = self.next_ready_workflow_ticket(job_id, &workflow_id).await
                else {
                    break;
                };
                match self
                    .try_spawn_dispatch(&mut active, &mut reservations, &mut summary, ticket)
                    .await
                {
                    Ok(SpawnOutcome::PreLeaseTerminal(source)) | Err(source) => {
                        terminal_error = Some(source);
                        dispatched_or_failed = true;
                        break;
                    }
                    Ok(SpawnOutcome::Spawned | SpawnOutcome::PreLeaseRetriable) => {
                        dispatched_or_failed = true;
                    }
                    Ok(SpawnOutcome::CapacityDeferred) => {
                        break;
                    }
                }
            }
            if dispatched_or_failed {
                continue;
            }

            if active.is_empty() {
                if let Some(delay) = self
                    .retry_delay(job_id, &workflow_id, self.control_plane.clock().now())
                    .await
                {
                    tokio::time::sleep(delay).await;
                    continue;
                }
                let source = VoomError::Internal(format!(
                    "workflow {job_id} has no dispatchable work but is not finished"
                ));
                let _ = self
                    .control_plane
                    .fail_job(job_id, source.to_string(), self.control_plane.clock().now())
                    .await;
                summary
                    .refresh_counts(&self.control_plane, job_id, started.elapsed())
                    .await;
                return Err(WorkflowRunError { summary, source });
            }
            if let Some(joined) = active.join_next().await {
                self.process_joined_dispatch(
                    joined,
                    &plan,
                    &workflow_id,
                    job_id,
                    &mut reservations,
                    &mut summary,
                    &mut terminal_error,
                )
                .await;
            }
        }
    }

    async fn create_root_tickets(
        &self,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        for node in &plan.nodes {
            if !node.depends_on().is_empty() || !node.depends_on_selected().is_empty() {
                continue;
            }
            self.create_node_ticket(plan, node, workflow_id, job_id, now)
                .await?;
        }
        Ok(())
    }

    async fn create_node_ticket(
        &self,
        plan: &WorkflowPlan,
        node: &WorkflowNode,
        workflow_id: &str,
        job_id: JobId,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let operation = node.operation();
        let branch = BranchContext {
            branch_id: "root".to_owned(),
            path: "/library/root.mkv".to_owned(),
            probe_codec: Some("h264".to_owned()),
            source_file: None,
        };
        let timing = seeded_timing(
            plan.seed,
            node.id(),
            &branch.branch_id,
            plan.timing.base_duration_ms,
            plan.timing.jitter_ms,
        );
        let rendered_payload = self
            .render_root_payload(plan, node, &branch, timing)
            .await?;
        let payload = WorkflowTicketPayload {
            workflow_id: workflow_id.to_owned(),
            plan_id: plan.id.clone(),
            node_id: node.id().to_owned(),
            branch_id: branch.branch_id.clone(),
            operation,
            rendered_payload,
            timing,
            source_file: None,
        }
        .to_ticket_payload()
        .map_err(|e| VoomError::Config(format!("workflow ticket payload encode: {e}")))?;
        let ticket = self
            .control_plane
            .create_ticket(NewTicket {
                job_id: Some(job_id),
                kind: ticket_kind(operation),
                priority: 0,
                payload,
                max_attempts: self.options.max_attempts,
                created_at: now,
            })
            .await?;
        self.control_plane
            .mark_ready_if_unblocked(ticket.id, now)
            .await?;
        Ok(())
    }

    async fn render_root_payload(
        &self,
        plan: &WorkflowPlan,
        node: &WorkflowNode,
        branch: &BranchContext,
        timing: EffectiveTiming,
    ) -> Result<Value, VoomError> {
        let operation = node.operation();
        match operation {
            OperationKind::ScanLibrary => root_payload_result(render_default_payload_with_fan_out(
                operation,
                branch,
                timing,
                plan.fan_out.max_files,
            )),
            OperationKind::TranscodeVideo => match node.policy_target() {
                Some(target) => root_payload_result(render_policy_transcode_payload(
                    self.resolve_policy_file_source(target, "transcode_video")
                        .await?,
                    node.operation_payload(),
                    &self.options.transcode_staging_root,
                    &self.options.transcode_target_dir,
                    timing,
                )),
                None => root_payload_result(render_default_payload(operation, branch, timing)),
            },
            OperationKind::Remux => self.render_root_remux_payload(node, branch, timing).await,
            OperationKind::TranscodeAudio => match node.policy_target() {
                Some(target) => root_payload_result(render_policy_transcode_audio_payload(
                    self.resolve_policy_file_source(target, "transcode_audio")
                        .await?,
                    node.operation_payload(),
                    &self.options.audio_staging_root,
                    &self.options.audio_target_dir,
                    timing,
                )),
                None => root_payload_result(render_default_payload(operation, branch, timing)),
            },
            OperationKind::ExtractAudio => match node.policy_target() {
                Some(target) => root_payload_result(render_policy_extract_audio_payload(
                    self.resolve_policy_file_source(target, "extract_audio")
                        .await?,
                    node.operation_payload(),
                    &self.options.audio_staging_root,
                    &self.options.audio_target_dir,
                    timing,
                )),
                None => root_payload_result(render_default_payload(operation, branch, timing)),
            },
            _ => root_payload_result(render_default_payload(operation, branch, timing)),
        }
    }

    async fn render_root_remux_payload(
        &self,
        node: &WorkflowNode,
        branch: &BranchContext,
        timing: EffectiveTiming,
    ) -> Result<Value, VoomError> {
        match node.policy_target() {
            Some(
                target @ (voom_plan::TargetRef::FileVersion { .. }
                | voom_plan::TargetRef::FileLocation { .. }),
            ) => {
                let rendered = render_policy_remux_payload(
                    self.resolve_policy_file_source(target, "remux").await?,
                    node.operation_payload(),
                    &self.options.remux_staging_root,
                    &self.options.remux_target_dir,
                    timing,
                );
                root_payload_result(rendered)
            }
            Some(target) => Err(root_payload_error(&super::binding::BindingError::new(
                format!("remux requires file_version or file_location target, got {target:?}"),
            ))),
            None => {
                root_payload_result(render_default_payload(OperationKind::Remux, branch, timing))
            }
        }
    }

    async fn resolve_policy_file_source(
        &self,
        target: &voom_plan::TargetRef,
        operation_name: &str,
    ) -> Result<PolicyFileSource, VoomError> {
        match target {
            voom_plan::TargetRef::FileVersion { id } => Ok(PolicyFileSource {
                file_version_id: *id,
                location_id: None,
            }),
            voom_plan::TargetRef::FileLocation { id } => {
                let location = self
                    .control_plane
                    .identity
                    .get_file_location(*id)
                    .await?
                    .ok_or_else(|| VoomError::NotFound(format!("file_location {id}")))?;
                if location.retired_at.is_some() {
                    return Err(VoomError::Config(format!("file_location {id} is retired")));
                }
                Ok(PolicyFileSource {
                    file_version_id: location.file_version_id,
                    location_id: Some(*id),
                })
            }
            other => Err(VoomError::Config(format!(
                "{operation_name} requires file_version or file_location target, got {other:?}"
            ))),
        }
    }

    async fn try_spawn_dispatch(
        &self,
        active: &mut JoinSet<DispatchOutcome>,
        reservations: &mut HashMap<WorkerId, u32>,
        summary: &mut WorkflowRunSummary,
        ticket: Ticket,
    ) -> Result<SpawnOutcome, VoomError> {
        let workflow_payload = parse_payload(&ticket)?;
        let candidates = self
            .candidate_workers(workflow_payload.operation, reservations)
            .await?;
        let worker_id = match self
            .selector
            .select(workflow_payload.operation, &candidates)
        {
            Ok(worker_id) => worker_id,
            Err(source) => {
                if matches!(source, VoomError::NoEligibleWorker(_))
                    && local_reservation_blocks(&candidates, reservations)
                {
                    return Ok(SpawnOutcome::CapacityDeferred);
                }
                let class = selector_failure_class(&source)?;
                let outcome = self
                    .control_plane
                    .record_pre_lease_ticket_failure(
                        ticket.id,
                        class,
                        self.control_plane.clock().now(),
                    )
                    .await?;
                summary.failure_count += u64::from(outcome.terminal);
                if outcome.terminal {
                    return Ok(SpawnOutcome::PreLeaseTerminal(source));
                }
                return Ok(SpawnOutcome::PreLeaseRetriable);
            }
        };
        let runtime = self.runtimes.get(worker_id)?;
        let lease = acquire_lease_with_retry(
            &self.control_plane,
            NewLease {
                ticket_id: ticket.id,
                worker_id,
                ttl: time_duration(self.options.lease_ttl)?,
                now: self.control_plane.clock().now(),
            },
        )
        .await?;
        increment_reservation(reservations, worker_id);
        summary.dispatch_count += 1;
        summary.record_dispatch(workflow_payload.operation, worker_id, reservations);

        let control = self.control_plane.clone();
        let options = self.options.clone();
        active.spawn(async move {
            dispatch_ticket(
                control,
                runtime,
                ticket,
                workflow_payload,
                lease.id,
                options,
            )
            .await
        });
        Ok(SpawnOutcome::Spawned)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "completion handling needs shared scheduler state plus immutable workflow context"
    )]
    async fn process_joined_dispatch(
        &self,
        joined: Result<DispatchOutcome, tokio::task::JoinError>,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        reservations: &mut HashMap<WorkerId, u32>,
        summary: &mut WorkflowRunSummary,
        terminal_error: &mut Option<VoomError>,
    ) {
        let outcome = match joined {
            Ok(outcome) => outcome,
            Err(err) => DispatchOutcome {
                ticket_id: TicketId(0),
                worker_id: WorkerId(0),
                operation: OperationKind::HashFile,
                terminal: DispatchTerminal::Failure {
                    source: VoomError::WorkerCrash(format!(
                        "workflow dispatch task crashed: {err}"
                    )),
                },
            },
        };
        decrement_reservation(reservations, outcome.worker_id);
        match outcome.terminal {
            DispatchTerminal::Success => {
                summary.record_success(outcome.operation);
                if let Err(source) = self
                    .expand_successful_ticket(plan, workflow_id, job_id, outcome.ticket_id)
                    .await
                {
                    *terminal_error = Some(source);
                }
            }
            DispatchTerminal::Failure { source } => {
                let class = self
                    .ticket_failure_class(outcome.ticket_id)
                    .await
                    .unwrap_or_else(|| failure_class_for_error(&source));
                summary.record_failure(outcome.operation, class);
                match self.control_plane.tickets.get(outcome.ticket_id).await {
                    Ok(Some(ticket)) if ticket.state == TicketState::Failed => {
                        *terminal_error = Some(source);
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        *terminal_error = Some(VoomError::NotFound(format!(
                            "ticket {} vanished after dispatch failure",
                            outcome.ticket_id
                        )));
                    }
                    Err(err) => {
                        *terminal_error = Some(err);
                    }
                }
            }
        }
    }

    async fn expand_successful_ticket(
        &self,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        ticket_id: TicketId,
    ) -> Result<(), VoomError> {
        let ticket = self
            .control_plane
            .tickets
            .get(ticket_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("ticket {ticket_id}")))?;
        let payload = parse_payload(&ticket)?;
        let ctx = ExpansionContext::new(
            &self.control_plane,
            plan,
            workflow_id,
            &plan.id,
            job_id,
            self.control_plane.clock().now(),
        );
        match payload.node_id.as_str() {
            "scan" => {
                expand_scanner_completion(&ctx, &ticket).await?;
            }
            "probe" => {
                expand_probe_completion(&ctx, &payload.branch_id, &ticket).await?;
            }
            "quality" => {
                expand_quality_completion(&ctx, &payload.branch_id, &ticket).await?;
            }
            "remux" | "transcode" => {
                expand_transform_completion(&ctx, &payload.branch_id, &ticket).await?;
            }
            "backup" => {
                expand_backup_completion(&ctx, &payload.branch_id, &ticket).await?;
            }
            node_id if node_id.starts_with(POLICY_NODE_ID_PREFIX) => {
                self.expand_policy_node_completion(plan, workflow_id, job_id, node_id)
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Dynamically expands the dependents of a just-succeeded policy-bridge node.
    ///
    /// Policy plans (node ids prefixed `policy-node_`) can be arbitrary DAGs whose
    /// edges are declared via [`WorkflowNode::depends_on`]. Workflow tickets do not
    /// use the store's declarative dependency table, so each downstream node's
    /// ticket must be created here once all of its parents have succeeded.
    async fn expand_policy_node_completion(
        &self,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        completed_node_id: &str,
    ) -> Result<(), VoomError> {
        let succeeded = self.succeeded_node_ids(job_id, workflow_id).await?;
        let now = self.control_plane.clock().now();
        for node in &plan.nodes {
            if !depends_on_node(node, completed_node_id) {
                continue;
            }
            if self
                .node_ticket_exists(job_id, workflow_id, node.id())
                .await?
            {
                continue;
            }
            if !all_dependencies_succeeded(node, &succeeded) {
                continue;
            }
            self.create_node_ticket(plan, node, workflow_id, job_id, now)
                .await?;
        }
        Ok(())
    }

    /// Returns the set of node ids whose tickets are in the `succeeded` state for
    /// this workflow. Used to decide whether a join node's parents have all
    /// completed.
    async fn succeeded_node_ids(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<HashSet<String>, VoomError> {
        let rows = sqlx::query(
            "SELECT json_extract(payload, '$.node_id') AS node_id FROM tickets \
             WHERE job_id = ? \
               AND state = 'succeeded' \
               AND json_extract(payload, '$.workflow_id') = ?",
        )
        .bind(sqlite_i64(job_id.0))
        .bind(workflow_id)
        .fetch_all(&self.control_plane.pool)
        .await
        .map_err(|e| VoomError::Database(format!("workflow succeeded node ids: {e}")))?;
        let mut node_ids = HashSet::new();
        for row in rows {
            let node_id: Option<String> = row
                .try_get("node_id")
                .map_err(|e| VoomError::Database(format!("succeeded node id row: {e}")))?;
            if let Some(node_id) = node_id {
                node_ids.insert(node_id);
            }
        }
        Ok(node_ids)
    }

    /// Reports whether a ticket already exists for the given node id in this
    /// workflow, in any state. Guards against creating duplicate tickets for a
    /// join node when more than one parent succeeds.
    async fn node_ticket_exists(
        &self,
        job_id: JobId,
        workflow_id: &str,
        node_id: &str,
    ) -> Result<bool, VoomError> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM tickets \
             WHERE job_id = ? \
               AND json_extract(payload, '$.workflow_id') = ? \
               AND json_extract(payload, '$.node_id') = ?",
        )
        .bind(sqlite_i64(job_id.0))
        .bind(workflow_id)
        .bind(node_id)
        .fetch_one(&self.control_plane.pool)
        .await
        .map_err(|e| VoomError::Database(format!("workflow node ticket exists: {e}")))?;
        Ok(count > 0)
    }

    async fn next_ready_workflow_ticket(&self, job_id: JobId, workflow_id: &str) -> Option<Ticket> {
        let now = format_time(self.control_plane.clock().now()).ok()?;
        let rows = sqlx::query(
            "SELECT id FROM tickets \
             WHERE job_id = ? \
               AND state = 'ready' \
               AND next_eligible_at <= ? \
               AND json_extract(payload, '$.workflow_id') = ? \
             ORDER BY priority DESC, next_eligible_at ASC, id ASC \
             LIMIT ?",
        )
        .bind(sqlite_i64(job_id.0))
        .bind(now)
        .bind(workflow_id)
        .bind(i64::from(self.options.ready_batch_size))
        .fetch_all(&self.control_plane.pool)
        .await
        .ok()?;
        for row in rows {
            let id: i64 = row.try_get("id").ok()?;
            let ticket = self
                .control_plane
                .tickets
                .get(TicketId(sqlite_u64(id)))
                .await
                .ok()??;
            if WorkflowTicketPayload::parse_ticket(&ticket.kind, ticket.payload.clone()).is_ok() {
                return Some(ticket);
            }
        }
        None
    }

    async fn workflow_finished(&self, job_id: JobId) -> bool {
        let Ok((unfinished,)): Result<(i64,), _> = sqlx::query_as(
            "SELECT COUNT(*) FROM tickets \
             WHERE job_id = ? AND state IN ('pending', 'ready', 'leased')",
        )
        .bind(sqlite_i64(job_id.0))
        .fetch_one(&self.control_plane.pool)
        .await
        else {
            return false;
        };
        unfinished == 0
    }

    async fn first_failed_ticket_error(&self, job_id: JobId) -> Option<VoomError> {
        let row = sqlx::query(
            "SELECT kind, payload FROM tickets \
             WHERE job_id = ? AND state = 'failed' ORDER BY id ASC LIMIT 1",
        )
        .bind(sqlite_i64(job_id.0))
        .fetch_optional(&self.control_plane.pool)
        .await
        .ok()??;
        let kind: String = row.try_get("kind").ok()?;
        let payload: String = row.try_get("payload").ok()?;
        let payload: Value = serde_json::from_str(&payload).ok()?;
        let workflow_payload = WorkflowTicketPayload::parse_ticket(&kind, payload).ok()?;
        Some(VoomError::Internal(format!(
            "workflow ticket {} failed",
            workflow_payload.node_id
        )))
    }

    async fn ticket_failure_class(&self, ticket_id: TicketId) -> Option<FailureClass> {
        let row = sqlx::query(
            "SELECT payload FROM events \
             WHERE kind IN ('ticket.failed_terminal', 'ticket.failed_retriable') \
               AND subject_type = 'ticket' \
               AND subject_id = ? \
             ORDER BY event_id DESC LIMIT 1",
        )
        .bind(sqlite_i64(ticket_id.0))
        .fetch_optional(&self.control_plane.pool)
        .await
        .ok()??;
        let payload: String = row.try_get("payload").ok()?;
        let payload: Value = serde_json::from_str(&payload).ok()?;
        serde_json::from_value(payload.get("class")?.clone()).ok()
    }

    async fn retry_delay(
        &self,
        job_id: JobId,
        workflow_id: &str,
        now: OffsetDateTime,
    ) -> Option<Duration> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT MIN(next_eligible_at) FROM tickets \
             WHERE job_id = ? \
               AND state = 'ready' \
               AND next_eligible_at > ? \
               AND json_extract(payload, '$.workflow_id') = ?",
        )
        .bind(sqlite_i64(job_id.0))
        .bind(format_time(now).ok()?)
        .bind(workflow_id)
        .fetch_optional(&self.control_plane.pool)
        .await
        .ok()?;
        let (next_eligible,) = row?;
        let next_eligible = next_eligible?;
        let next_eligible = OffsetDateTime::parse(
            &next_eligible,
            &time::format_description::well_known::Iso8601::DEFAULT,
        )
        .ok()?;
        let wait = next_eligible - now;
        Duration::try_from(wait).ok()
    }

    async fn candidate_workers(
        &self,
        operation: OperationKind,
        reservations: &HashMap<WorkerId, u32>,
    ) -> Result<Vec<WorkerView>, VoomError> {
        let operation_name = operation_name(operation);
        let rows = sqlx::query(
            "SELECT w.id AS worker_id, wg.can_execute, wg.denies, wg.max_parallel, \
                    COALESCE(held.active_leases, 0) AS active_leases \
             FROM workers w \
             JOIN worker_capabilities wc ON wc.worker_id = w.id \
             JOIN worker_grants wg ON wg.worker_id = w.id \
             LEFT JOIN ( \
                 SELECT worker_id, COUNT(*) AS active_leases \
                 FROM leases WHERE state = 'held' GROUP BY worker_id \
             ) held ON held.worker_id = w.id \
             WHERE w.status IN ('registered', 'active') AND wc.operation = ? \
             ORDER BY w.id ASC",
        )
        .bind(operation_name)
        .fetch_all(&self.control_plane.pool)
        .await
        .map_err(|e| VoomError::Database(format!("workflow worker candidates: {e}")))?;

        let mut views = Vec::new();
        for row in rows {
            let worker_id: i64 = row
                .try_get("worker_id")
                .map_err(|e| VoomError::Database(format!("worker candidate row: {e}")))?;
            let can_execute: String = row
                .try_get("can_execute")
                .map_err(|e| VoomError::Database(format!("worker grant can_execute: {e}")))?;
            let denies: String = row
                .try_get("denies")
                .map_err(|e| VoomError::Database(format!("worker grant denies: {e}")))?;
            let max_parallel: String = row
                .try_get("max_parallel")
                .map_err(|e| VoomError::Database(format!("worker grant max_parallel: {e}")))?;
            if !json_string_array_contains(&can_execute, operation_name)?
                || json_string_array_contains(&denies, operation_name)?
            {
                continue;
            }
            let worker_id = WorkerId(sqlite_u64(worker_id));
            let active_leases: i64 = row
                .try_get("active_leases")
                .map_err(|e| VoomError::Database(format!("worker active lease count: {e}")))?;
            let reserved = reservations.get(&worker_id).copied().unwrap_or(0);
            views.push(WorkerView {
                worker_id,
                supports: vec![operation],
                active_leases: sqlite_u32(active_leases).saturating_add(reserved),
                max_parallel: max_parallel_for_operation(&max_parallel, operation_name)?,
            });
        }
        Ok(views)
    }
}

fn root_payload_result(
    result: Result<Value, super::binding::BindingError>,
) -> Result<Value, VoomError> {
    result.map_err(|error| root_payload_error(&error))
}

fn root_payload_error(error: &super::binding::BindingError) -> VoomError {
    VoomError::Config(format!("workflow root payload binding: {error}"))
}

#[derive(Debug)]
enum SpawnOutcome {
    Spawned,
    PreLeaseRetriable,
    PreLeaseTerminal(VoomError),
    CapacityDeferred,
}

fn parse_payload(ticket: &Ticket) -> Result<WorkflowTicketPayload, VoomError> {
    WorkflowTicketPayload::parse_ticket(&ticket.kind, ticket.payload.clone())
        .map_err(|e| VoomError::Config(format!("workflow ticket payload decode: {e}")))
}

fn ticket_kind(operation: OperationKind) -> String {
    format!("synthetic.workflow.operation.{}", operation_name(operation))
}

/// Reports whether `node` lists `parent_id` among its direct dependencies.
///
/// Only `depends_on` (node ids) is consulted. `depends_on_selected` holds
/// dependency-*group* names resolved through [`WorkflowNode::provides_selected`],
/// not node ids, and no policy plan currently emits selected dependencies; their
/// completion gating is therefore left undefined here rather than guessed.
fn depends_on_node(node: &WorkflowNode, parent_id: &str) -> bool {
    node.depends_on().iter().any(|id| id == parent_id)
}

/// Reports whether every direct dependency of `node` has a succeeded ticket. A
/// join node is created only once all of its parents are present in `succeeded`,
/// so the last parent to finish triggers creation exactly once.
fn all_dependencies_succeeded(node: &WorkflowNode, succeeded: &HashSet<String>) -> bool {
    node.depends_on()
        .iter()
        .all(|dependency| succeeded.contains(dependency))
}

fn selector_failure_class(source: &VoomError) -> Result<FailureClass, VoomError> {
    match source {
        VoomError::NoEligibleWorker(_) => Ok(FailureClass::NoEligibleWorker),
        VoomError::AmbiguousWorkerSelection(_) => Ok(FailureClass::AmbiguousWorkerSelection),
        other => Err(VoomError::Internal(format!(
            "selector returned unsupported workflow error: {other}"
        ))),
    }
}

fn increment_reservation(reservations: &mut HashMap<WorkerId, u32>, worker_id: WorkerId) {
    *reservations.entry(worker_id).or_default() += 1;
}

fn decrement_reservation(reservations: &mut HashMap<WorkerId, u32>, worker_id: WorkerId) {
    if let Some(count) = reservations.get_mut(&worker_id) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            reservations.remove(&worker_id);
        }
    }
}

fn local_reservation_blocks(
    candidates: &[WorkerView],
    reservations: &HashMap<WorkerId, u32>,
) -> bool {
    candidates.iter().any(|candidate| {
        reservations.get(&candidate.worker_id).copied().unwrap_or(0) > 0
            && candidate.active_leases >= candidate.max_parallel
    })
}

fn json_string_array_contains(raw: &str, needle: &str) -> Result<bool, VoomError> {
    let values: Vec<String> = serde_json::from_str(raw)
        .map_err(|e| VoomError::Database(format!("parse worker grant array: {e}")))?;
    Ok(values.iter().any(|value| value == needle))
}

fn max_parallel_for_operation(raw: &str, operation: &str) -> Result<u32, VoomError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| VoomError::Database(format!("parse worker max_parallel: {e}")))?;
    let max = value
        .get(operation)
        .or_else(|| value.get("*"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    Ok(u32::try_from(max).unwrap_or(u32::MAX).max(1))
}

fn format_time(t: OffsetDateTime) -> Result<String, VoomError> {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::Internal(format!("format iso8601: {e}")))
}

fn sqlite_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn sqlite_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn sqlite_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

#[cfg(test)]
#[path = "executor_test.rs"]
mod tests;
