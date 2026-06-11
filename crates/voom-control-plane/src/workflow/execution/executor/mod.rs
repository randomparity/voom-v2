use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use tokio::task::JoinSet;
use voom_core::OperationKind;
use voom_core::{JobId, VoomError, WorkerId};
#[cfg(test)]
use voom_store::repo::jobs::NewJob;
use voom_store::repo::tickets::Ticket;

use super::dispatch::DispatchOutcome;
use super::runtime::WorkerRuntimeRegistry;
use crate::ControlPlane;
use crate::workflow::plan::model::WorkflowPlan;
use crate::workflow::summary::WorkflowRunSummary;

mod config;
mod errors;
mod expansion;
mod spawn;
mod tickets;

pub(crate) use config::{
    OperationArtifactRoots, WORKFLOW_JOB_KIND, WorkflowArtifactRoots, WorkflowDispatchOptions,
    WorkflowQueueOptions, WorkflowStreamOptions, WorkflowTimingOptions,
};
pub(crate) use errors::WorkflowRunError;
use spawn::SpawnOutcome;

#[derive(Debug, Clone)]
pub struct WorkflowExecutor {
    control_plane: ControlPlane,
    runtimes: WorkerRuntimeRegistry,
    options: WorkflowExecutorOptions,
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowExecutorOptions {
    pub timing: WorkflowTimingOptions,
    pub queue: WorkflowQueueOptions,
    pub artifact_roots: WorkflowArtifactRoots,
    pub chaos: WorkflowChaosOptions,
}

impl WorkflowExecutorOptions {
    #[must_use]
    pub(crate) fn dispatch_options(&self) -> WorkflowDispatchOptions {
        WorkflowDispatchOptions {
            timing: self.timing.clone(),
            artifact_roots: self.artifact_roots.clone(),
            chaos: self.chaos.clone(),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            timing: WorkflowTimingOptions::for_tests(),
            queue: WorkflowQueueOptions::default(),
            artifact_roots: WorkflowArtifactRoots::for_tests(),
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
    #[cfg(test)]
    #[must_use]
    pub fn suppress_heartbeats_for_operation(operation: OperationKind) -> Self {
        Self {
            suppress_heartbeat_operation: Some(operation),
            ..Self::default()
        }
    }

    #[cfg(test)]
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

struct RunLoopState {
    reservations: HashMap<WorkerId, u32>,
    active: JoinSet<DispatchOutcome>,
    summary: WorkflowRunSummary,
    terminal_error: Option<VoomError>,
}

impl RunLoopState {
    fn new(job_id: JobId, elapsed: Duration) -> Self {
        Self {
            reservations: HashMap::new(),
            active: JoinSet::new(),
            summary: WorkflowRunSummary::empty(job_id, elapsed),
            terminal_error: None,
        }
    }

    fn active_is_empty(&self) -> bool {
        self.active.is_empty()
    }

    fn has_dispatch_capacity(&self, max_in_flight: usize) -> bool {
        self.active.len() < max_in_flight
    }

    fn record_terminal_error(&mut self, source: VoomError) {
        self.terminal_error = Some(source);
    }

    fn has_terminal_error(&self) -> bool {
        self.terminal_error.is_some()
    }

    fn take_terminal_error(&mut self) -> Option<VoomError> {
        self.terminal_error.take()
    }

    async fn refresh(&mut self, control: &ControlPlane, job_id: JobId, started: Instant) {
        self.summary
            .refresh_counts(control, job_id, started.elapsed())
            .await;
    }

    async fn finish_success(
        &mut self,
        control: &ControlPlane,
        job_id: JobId,
        started: Instant,
    ) -> WorkflowRunSummary {
        self.refresh(control, job_id, started).await;
        self.summary.clone()
    }

    async fn fail_job(
        &mut self,
        control: &ControlPlane,
        job_id: JobId,
        source: VoomError,
        started: Instant,
    ) -> WorkflowRunError {
        let _ = control
            .fail_job(job_id, source.to_string(), control.clock().now())
            .await;
        self.refresh(control, job_id, started).await;
        WorkflowRunError {
            summary: self.summary.clone(),
            source,
        }
    }

    async fn fail_after_drain(
        &mut self,
        executor: &WorkflowExecutor,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        source: VoomError,
        started: Instant,
    ) -> WorkflowRunError {
        self.drain_active(executor, plan, workflow_id, job_id).await;
        self.fail_job(&executor.control_plane, job_id, source, started)
            .await
    }

    async fn process_completed_dispatches(
        &mut self,
        executor: &WorkflowExecutor,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
    ) {
        while let Some(joined) = self.active.try_join_next() {
            self.process_joined_dispatch(executor, joined, plan, workflow_id, job_id)
                .await;
        }
    }

    async fn drain_active(
        &mut self,
        executor: &WorkflowExecutor,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
    ) {
        while let Some(joined) = self.active.join_next().await {
            self.process_joined_dispatch(executor, joined, plan, workflow_id, job_id)
                .await;
        }
    }

    async fn wait_for_one(
        &mut self,
        executor: &WorkflowExecutor,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
    ) {
        if let Some(joined) = self.active.join_next().await {
            self.process_joined_dispatch(executor, joined, plan, workflow_id, job_id)
                .await;
        }
    }

    async fn try_spawn_dispatch(
        &mut self,
        executor: &WorkflowExecutor,
        ticket: Ticket,
    ) -> Result<SpawnOutcome, VoomError> {
        executor
            .try_spawn_dispatch(
                &mut self.active,
                &mut self.reservations,
                &mut self.summary,
                ticket,
            )
            .await
    }

    async fn process_joined_dispatch(
        &mut self,
        executor: &WorkflowExecutor,
        joined: Result<DispatchOutcome, tokio::task::JoinError>,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
    ) {
        executor
            .process_joined_dispatch(
                joined,
                plan,
                workflow_id,
                job_id,
                &mut self.reservations,
                &mut self.summary,
                &mut self.terminal_error,
            )
            .await;
    }
}

impl WorkflowExecutor {
    #[must_use]
    pub fn with_options(
        control_plane: ControlPlane,
        runtimes: WorkerRuntimeRegistry,
        options: WorkflowExecutorOptions,
    ) -> Self {
        Self {
            control_plane,
            runtimes,
            options,
        }
    }

    #[cfg(test)]
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
    /// Unlike the test-only `submit_and_run` helper, this does not open or succeed the job:
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
    async fn run_plan_in_job(
        &self,
        job_id: JobId,
        plan: WorkflowPlan,
        started: Instant,
    ) -> Result<WorkflowRunSummary, WorkflowRunError> {
        let now = self.control_plane.clock().now();
        let workflow_id = format!("workflow-{}", job_id.0);
        let mut state = RunLoopState::new(job_id, started.elapsed());
        let control = &self.control_plane;

        if let Err(source) = self
            .create_root_tickets(&plan, &workflow_id, job_id, now)
            .await
        {
            return Err(state.fail_job(control, job_id, source, started).await);
        }

        loop {
            state
                .process_completed_dispatches(self, &plan, &workflow_id, job_id)
                .await;

            state.refresh(control, job_id, started).await;
            if let Some(source) = state.take_terminal_error() {
                return Err(state
                    .fail_after_drain(self, &plan, &workflow_id, job_id, source, started)
                    .await);
            }
            let finished = match self.workflow_finished(job_id).await {
                Ok(finished) => finished,
                Err(source) => {
                    return Err(state.fail_job(control, job_id, source, started).await);
                }
            };
            if state.active_is_empty() && finished {
                match self.first_failed_ticket_error(job_id).await {
                    Ok(None) => {
                        return Ok(state.finish_success(control, job_id, started).await);
                    }
                    Ok(Some(source)) | Err(source) => {
                        return Err(state.fail_job(control, job_id, source, started).await);
                    }
                }
            }

            let mut dispatched_or_failed = false;
            let max_in_flight = plan.concurrency.max_in_flight_dispatches;
            while state.has_dispatch_capacity(max_in_flight) {
                let tickets = match self.ready_workflow_tickets(job_id, &workflow_id).await {
                    Ok(tickets) if tickets.is_empty() => break,
                    Ok(tickets) => tickets,
                    Err(source) => {
                        state.record_terminal_error(source);
                        dispatched_or_failed = true;
                        break;
                    }
                };
                let mut batch_made_progress = false;
                for ticket in tickets {
                    if !state.has_dispatch_capacity(max_in_flight) {
                        break;
                    }
                    match state.try_spawn_dispatch(self, ticket).await {
                        Ok(SpawnOutcome::PreLeaseTerminal(source)) | Err(source) => {
                            state.record_terminal_error(source);
                            dispatched_or_failed = true;
                            batch_made_progress = true;
                            break;
                        }
                        Ok(SpawnOutcome::Spawned | SpawnOutcome::PreLeaseRetriable) => {
                            dispatched_or_failed = true;
                            batch_made_progress = true;
                        }
                        Ok(SpawnOutcome::CapacityDeferred) => {}
                    }
                }
                if state.has_terminal_error() || !batch_made_progress {
                    break;
                }
            }
            if dispatched_or_failed {
                continue;
            }

            if state.active_is_empty() {
                match self
                    .retry_delay(job_id, &workflow_id, self.control_plane.clock().now())
                    .await
                {
                    Ok(Some(delay)) => {
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    Ok(None) => {}
                    Err(source) => {
                        return Err(state.fail_job(control, job_id, source, started).await);
                    }
                }
                let source = VoomError::Internal(format!(
                    "workflow {job_id} has no dispatchable work but is not finished"
                ));
                return Err(state.fail_job(control, job_id, source, started).await);
            }
            state.wait_for_one(self, &plan, &workflow_id, job_id).await;
        }
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
