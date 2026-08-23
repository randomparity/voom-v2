#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tokio::task::{Id, JoinSet};
use voom_core::{
    FileAssetId, FileVersionId, JobId, LeaseId, OperationKind, TicketId, VoomError, WorkerId,
};
use voom_store::repo::execution::jobs::JobState;
#[cfg(test)]
use voom_store::repo::execution::jobs::NewJob;

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
pub(crate) use errors::{WorkflowFailureDisposition, WorkflowRunError};
use spawn::SpawnOutcome;

#[derive(Debug, Clone)]
pub struct WorkflowExecutor {
    control_plane: ControlPlane,
    runtimes: WorkerRuntimeRegistry,
    options: WorkflowExecutorOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunFailureMode {
    AbortJob,
    ContinueIndependent,
}

#[derive(Debug, Clone)]
pub(crate) struct PlannedLineageGuard {
    expectations: Vec<(FileAssetId, FileVersionId)>,
}

impl PlannedLineageGuard {
    pub(crate) fn new(
        planned_file_count: usize,
        expectations: Vec<(FileAssetId, FileVersionId)>,
    ) -> Result<Self, VoomError> {
        if planned_file_count == 0 {
            return Err(VoomError::Config(
                "lineage guard requires at least one planned file".to_owned(),
            ));
        }
        if expectations.len() != planned_file_count {
            return Err(VoomError::Config(format!(
                "lineage guard has {} expectations for {planned_file_count} planned files",
                expectations.len()
            )));
        }
        let unique_assets: HashSet<_> =
            expectations.iter().map(|(asset_id, _)| *asset_id).collect();
        if unique_assets.len() != expectations.len() {
            return Err(VoomError::Config(
                "lineage guard contains a duplicate file asset".to_owned(),
            ));
        }
        Ok(Self { expectations })
    }

    fn expectations(&self) -> &[(FileAssetId, FileVersionId)] {
        &self.expectations
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowExecutorOptions {
    pub timing: WorkflowTimingOptions,
    pub queue: WorkflowQueueOptions,
    pub artifact_roots: WorkflowArtifactRoots,
    #[cfg(test)]
    pub chaos: WorkflowChaosOptions,
    #[cfg(test)]
    pub(crate) capacity_deferred_sync: Option<CapacityDeferredTestSync>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct CapacityDeferredTestSync {
    pub(crate) observed: std::sync::Arc<tokio::sync::Notify>,
    pub(crate) resume: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct PostDispatchTestSync {
    pub(crate) operation: OperationKind,
    pub(crate) worker_result_observed: std::sync::Arc<tokio::sync::Notify>,
    pub(crate) resume_post_dispatch: std::sync::Arc<tokio::sync::Semaphore>,
    pub(crate) held: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl WorkflowExecutorOptions {
    #[must_use]
    pub(crate) fn dispatch_options(&self) -> WorkflowDispatchOptions {
        WorkflowDispatchOptions {
            timing: self.timing.clone(),
            artifact_roots: self.artifact_roots.clone(),
            #[cfg(test)]
            chaos: self.chaos.clone(),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            timing: WorkflowTimingOptions::for_tests(),
            queue: WorkflowQueueOptions::for_tests(),
            artifact_roots: WorkflowArtifactRoots::for_tests(),
            chaos: WorkflowChaosOptions::default(),
            capacity_deferred_sync: None,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct WorkflowChaosOptions {
    pub disable_heartbeat_ticks: bool,
    pub suppress_heartbeat_operation: Option<OperationKind>,
    pub payload_modes: BTreeMap<OperationKind, String>,
    pub(crate) panic_after_lease_operation: Option<OperationKind>,
    #[cfg(test)]
    pub(crate) post_dispatch_sync: Option<PostDispatchTestSync>,
    #[cfg(test)]
    pub(crate) fail_heartbeat_operation: Option<OperationKind>,
}

#[cfg(test)]
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

    #[cfg(test)]
    pub(crate) async fn hold_after_worker_result(&self, operation: OperationKind) {
        let Some(sync) = self
            .post_dispatch_sync
            .as_ref()
            .filter(|sync| sync.operation == operation)
        else {
            return;
        };
        if sync.held.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        sync.worker_result_observed.notify_one();
        let Ok(permit) = sync.resume_post_dispatch.acquire().await else {
            panic!("post-dispatch test semaphore must remain open");
        };
        permit.forget();
    }

    #[cfg(test)]
    pub(crate) fn fails_heartbeat_for(&self, operation: OperationKind) -> bool {
        self.fail_heartbeat_operation == Some(operation)
    }
}

struct RunLoopState {
    reservations: HashMap<WorkerId, u32>,
    active: JoinSet<DispatchOutcome>,
    active_identities: HashMap<Id, DispatchIdentity>,
    summary: WorkflowRunSummary,
    fatal_error: Option<VoomError>,
    isolated_error: Option<VoomError>,
    dispatch_started: bool,
    capacity_wait_started: Option<Instant>,
    accelerator_wait_started: HashMap<String, Instant>,
    accelerator_history: HashMap<String, Vec<String>>,
    /// Node-locally dispatched media tickets (ADR 0075) awaiting an agent's
    /// remote completion. The executor owns no lease for these; it polls their
    /// durable states and runs expansion/failure handling as they settle.
    node_local_outstanding: HashMap<TicketId, OperationKind>,
}

struct RunInvocation<'a> {
    job_id: JobId,
    workflow_id: &'a str,
    plan: &'a WorkflowPlan,
    failure_mode: RunFailureMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DispatchIdentity {
    ticket_id: TicketId,
    worker_id: WorkerId,
    lease_id: LeaseId,
    operation: OperationKind,
}

#[derive(Debug, Default)]
struct DispatchReadyOutcome {
    made_progress: bool,
    capacity_deferred: bool,
    accelerator_unavailable: HashSet<String>,
    recovered_accelerators: HashSet<String>,
}

fn no_dispatchable_work(job_id: JobId) -> VoomError {
    VoomError::Internal(format!(
        "workflow {job_id} has no dispatchable work but is not finished"
    ))
}

enum CapacityWaitOutcome {
    RetryAfter(Duration),
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowIdleState {
    Finished,
    Ready,
    Leased,
    Blocked,
}

impl RunLoopState {
    fn new(job_id: JobId, elapsed: Duration) -> Self {
        Self {
            reservations: HashMap::new(),
            active: JoinSet::new(),
            active_identities: HashMap::new(),
            summary: WorkflowRunSummary::empty(job_id, elapsed),
            fatal_error: None,
            isolated_error: None,
            dispatch_started: false,
            capacity_wait_started: None,
            accelerator_wait_started: HashMap::new(),
            accelerator_history: HashMap::new(),
            node_local_outstanding: HashMap::new(),
        }
    }

    fn active_is_empty(&self) -> bool {
        self.active.is_empty()
    }

    fn has_dispatch_capacity(&self, max_in_flight: usize) -> bool {
        self.active.len() < max_in_flight
    }

    fn record_fatal_error(&mut self, source: VoomError) {
        self.fatal_error = Some(source);
    }

    fn record_ticket_failure(&mut self, mode: RunFailureMode, source: VoomError) {
        match mode {
            RunFailureMode::AbortJob => self.record_fatal_error(source),
            RunFailureMode::ContinueIndependent => {
                if self.isolated_error.is_none() {
                    self.isolated_error = Some(source);
                }
            }
        }
    }

    fn has_fatal_error(&self) -> bool {
        self.fatal_error.is_some()
    }

    fn take_fatal_error(&mut self) -> Option<VoomError> {
        self.fatal_error.take()
    }

    fn take_isolated_error(&mut self) -> Option<VoomError> {
        self.isolated_error.take()
    }

    fn reset_capacity_wait(&mut self) {
        self.capacity_wait_started = None;
    }

    fn capacity_wait_elapsed(&mut self) -> Duration {
        self.capacity_wait_started
            .get_or_insert_with(Instant::now)
            .elapsed()
    }

    fn update_accelerator_waits(&mut self, dispatch: &DispatchReadyOutcome) {
        for token in &dispatch.accelerator_unavailable {
            self.accelerator_wait_started
                .entry(token.clone())
                .or_insert_with(Instant::now);
        }
        for token in &dispatch.recovered_accelerators {
            self.accelerator_wait_started.remove(token);
        }
    }

    fn timed_out_accelerator(&self, timeout: Duration) -> Option<&str> {
        self.accelerator_wait_started
            .iter()
            .find(|(_, started)| started.elapsed() >= timeout)
            .map(|(token, _)| token.as_str())
    }

    fn accelerator_wait_delay(&self, interval: Duration, timeout: Duration) -> Option<Duration> {
        self.accelerator_wait_started
            .values()
            .map(|started| timeout.saturating_sub(started.elapsed()))
            .min()
            .map(|remaining| interval.min(remaining))
    }

    async fn refresh(
        &mut self,
        control: &ControlPlane,
        job_id: JobId,
        started: Instant,
    ) -> Result<(), VoomError> {
        self.summary
            .refresh_counts(&control.tickets, &control.leases, job_id, started.elapsed())
            .await
    }

    async fn finish_success(
        &mut self,
        control: &ControlPlane,
        job_id: JobId,
        started: Instant,
    ) -> Result<WorkflowRunSummary, VoomError> {
        self.refresh(control, job_id, started).await?;
        Ok(self.summary.clone())
    }

    async fn fail_job(
        &mut self,
        control: &ControlPlane,
        job_id: JobId,
        source: VoomError,
        started: Instant,
    ) -> WorkflowRunError {
        let transition_error = control
            .fail_job(job_id, source.to_string(), control.clock().now())
            .await
            .err();
        let job_failed = transition_error.is_none();
        let refresh_error = self.refresh(control, job_id, started).await.err();
        let source = workflow_failure_source(job_id, source, transition_error, refresh_error);
        WorkflowRunError {
            summary: self.summary.clone(),
            source,
            job_failed,
            disposition: WorkflowFailureDisposition::Fatal,
            dispatch_started: self.dispatch_started,
        }
    }

    async fn finish_isolated_failure(
        &mut self,
        control: &ControlPlane,
        job_id: JobId,
        source: VoomError,
        started: Instant,
    ) -> WorkflowRunError {
        let Some(refresh_error) = self.refresh(control, job_id, started).await.err() else {
            return WorkflowRunError {
                summary: self.summary.clone(),
                source,
                job_failed: false,
                disposition: WorkflowFailureDisposition::IsolatedTicket,
                dispatch_started: self.dispatch_started,
            };
        };
        let source = workflow_failure_source(job_id, source, None, Some(refresh_error));
        WorkflowRunError {
            summary: self.summary.clone(),
            source,
            job_failed: false,
            disposition: WorkflowFailureDisposition::Fatal,
            dispatch_started: self.dispatch_started,
        }
    }

    async fn fail_after_drain(
        &mut self,
        executor: &WorkflowExecutor,
        invocation: &RunInvocation<'_>,
        source: VoomError,
        started: Instant,
    ) -> WorkflowRunError {
        let drain_invocation = RunInvocation {
            failure_mode: RunFailureMode::ContinueIndependent,
            ..*invocation
        };
        self.drain_active(executor, &drain_invocation).await;
        let source = self.take_fatal_error().unwrap_or(source);
        self.fail_job(&executor.control_plane, invocation.job_id, source, started)
            .await
    }

    async fn process_completed_dispatches(
        &mut self,
        executor: &WorkflowExecutor,
        invocation: &RunInvocation<'_>,
    ) {
        while let Some(joined) = self.active.try_join_next_with_id() {
            self.process_joined_dispatch(executor, joined, invocation)
                .await;
        }
    }

    async fn drain_active(&mut self, executor: &WorkflowExecutor, invocation: &RunInvocation<'_>) {
        while let Some(joined) = self.active.join_next_with_id().await {
            self.process_joined_dispatch(executor, joined, invocation)
                .await;
        }
    }

    async fn wait_for_one(&mut self, executor: &WorkflowExecutor, invocation: &RunInvocation<'_>) {
        if let Some(joined) = self.active.join_next_with_id().await {
            self.process_joined_dispatch(executor, joined, invocation)
                .await;
        }
    }

    async fn process_joined_dispatch(
        &mut self,
        executor: &WorkflowExecutor,
        joined: Result<(Id, DispatchOutcome), tokio::task::JoinError>,
        invocation: &RunInvocation<'_>,
    ) {
        let task_id = joined_task_id(&joined);
        let Some(identity) = self.active_identities.remove(&task_id) else {
            self.record_fatal_error(VoomError::Internal(format!(
                "workflow dispatch task {task_id} completed without a dispatch identity"
            )));
            return;
        };
        spawn::decrement_reservation(&mut self.reservations, identity.worker_id);
        executor
            .process_joined_dispatch(
                joined.map(|(_, outcome)| outcome),
                identity,
                invocation,
                self,
            )
            .await;
    }
}

fn joined_task_id(joined: &Result<(Id, DispatchOutcome), tokio::task::JoinError>) -> Id {
    match joined {
        Ok((task_id, _)) => *task_id,
        Err(error) => error.id(),
    }
}

fn workflow_failure_source(
    job_id: JobId,
    source: VoomError,
    transition_error: Option<VoomError>,
    refresh_error: Option<VoomError>,
) -> VoomError {
    if transition_error.is_none() && refresh_error.is_none() {
        return source;
    }
    let diagnostic = workflow_failure_diagnostic(
        job_id,
        &source,
        transition_error.as_ref(),
        refresh_error.as_ref(),
    );
    match transition_error {
        Some(VoomError::Database {
            source: database_source,
            ..
        }) => VoomError::Database {
            message: diagnostic,
            source: database_source,
        },
        _ => match refresh_error {
            Some(VoomError::Database {
                source: database_source,
                ..
            }) => VoomError::Database {
                message: diagnostic,
                source: database_source,
            },
            _ => match source {
                VoomError::Database {
                    source: database_source,
                    ..
                } => VoomError::Database {
                    message: diagnostic,
                    source: database_source,
                },
                _ => VoomError::Internal(diagnostic),
            },
        },
    }
}

fn workflow_failure_diagnostic(
    job_id: JobId,
    source: &VoomError,
    transition_error: Option<&VoomError>,
    refresh_error: Option<&VoomError>,
) -> String {
    match (transition_error, refresh_error) {
        (None, None) => format!("workflow failed for job {job_id}: {source}"),
        (Some(transition), None) => format!(
            "workflow failed for job {job_id}: {source}; \
             marking the job failed also failed: {transition}"
        ),
        (None, Some(refresh)) => format!(
            "workflow failed for job {job_id}: {source}; \
             refreshing the workflow summary also failed: {refresh}"
        ),
        (Some(transition), Some(refresh)) => format!(
            "workflow failed for job {job_id}: {source}; \
             marking the job failed also failed: {transition}; \
             refreshing the workflow summary also failed: {refresh}"
        ),
    }
}

impl WorkflowExecutor {
    async fn dispatch_ready_tickets(
        &self,
        state: &mut RunLoopState,
        invocation: &RunInvocation<'_>,
    ) -> DispatchReadyOutcome {
        let mut outcome = DispatchReadyOutcome::default();
        let mut accelerator_runtimes = None;
        let max_in_flight = invocation.plan.concurrency.max_in_flight_dispatches;
        while state.has_dispatch_capacity(max_in_flight) {
            let tickets = match self
                .ready_workflow_tickets(invocation.job_id, invocation.workflow_id)
                .await
            {
                Ok(tickets) if tickets.is_empty() => break,
                Ok(tickets) => tickets,
                Err(source) => {
                    state.record_fatal_error(source);
                    outcome.made_progress = true;
                    return outcome;
                }
            };
            let mut batch_made_progress = false;
            for ticket in tickets {
                if !state.has_dispatch_capacity(max_in_flight) {
                    break;
                }
                match self
                    .try_spawn_dispatch(state, ticket, &mut accelerator_runtimes)
                    .await
                {
                    Ok(SpawnOutcome::PreLeaseTerminal(source)) => {
                        state.record_ticket_failure(invocation.failure_mode, source);
                        outcome.made_progress = true;
                        batch_made_progress = true;
                        if invocation.failure_mode == RunFailureMode::AbortJob {
                            break;
                        }
                    }
                    Err(source) => {
                        state.record_fatal_error(source);
                        outcome.made_progress = true;
                        return outcome;
                    }
                    Ok(SpawnOutcome::Spawned(hardware_tokens)) => {
                        outcome.made_progress = true;
                        batch_made_progress = true;
                        outcome.recovered_accelerators.extend(hardware_tokens);
                    }
                    Ok(SpawnOutcome::PreLeaseRetriable) => {
                        outcome.made_progress = true;
                        batch_made_progress = true;
                    }
                    Ok(SpawnOutcome::CapacityDeferred) => {
                        outcome.capacity_deferred = true;
                    }
                    Ok(SpawnOutcome::AcceleratorUnavailable(hardware_tokens)) => {
                        outcome.accelerator_unavailable.extend(hardware_tokens);
                    }
                    Ok(SpawnOutcome::NodeLocalDispatched) => {
                        // Externally owned execution (ADR 0075): the ticket
                        // stays `ready` until its storage owner's agent takes
                        // it, so this is deliberately not dispatch progress —
                        // counting it would hot-spin the ready query. The run
                        // loop settles the ticket through
                        // `settle_node_local_completions`; `try_spawn_dispatch`
                        // has already recorded it as outstanding.
                    }
                }
            }
            if state.has_fatal_error() || !batch_made_progress {
                break;
            }
        }
        outcome
    }

    /// One run-loop pass of node-local completion settlement (ADR 0075),
    /// failing the job when the durable poll itself errors.
    async fn settle_node_local_step(
        &self,
        state: &mut RunLoopState,
        invocation: &RunInvocation<'_>,
        started: Instant,
    ) -> Result<(), WorkflowRunError> {
        if let Err(source) = self.settle_node_local_completions(state, invocation).await {
            return Err(state
                .fail_job(&self.control_plane, invocation.job_id, source, started)
                .await);
        }
        Ok(())
    }

    async fn capacity_wait_outcome(
        &self,
        state: &mut RunLoopState,
        job_id: JobId,
    ) -> Result<CapacityWaitOutcome, VoomError> {
        let job = self
            .control_plane
            .jobs
            .get(job_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("job {job_id}")))?;
        match job.state {
            JobState::Cancelled => return Ok(CapacityWaitOutcome::Cancelled),
            JobState::Open => {}
            JobState::Succeeded | JobState::Failed => {
                return Err(VoomError::Conflict(format!(
                    "workflow capacity wait rejected: job {job_id} is {}",
                    job.state.as_str()
                )));
            }
        }

        let interval = self.options.queue.capacity_retry_interval;
        let timeout = self.options.queue.capacity_retry_timeout;
        if interval.is_zero() || timeout.is_zero() {
            return Err(VoomError::Config(
                "worker capacity retry interval and timeout must be positive".to_owned(),
            ));
        }
        let elapsed = state.capacity_wait_elapsed();
        if elapsed >= timeout {
            return Ok(CapacityWaitOutcome::TimedOut);
        }
        Ok(CapacityWaitOutcome::RetryAfter(
            interval.min(timeout.saturating_sub(elapsed)),
        ))
    }

    async fn wait_for_external_capacity(
        &self,
        state: &mut RunLoopState,
        job_id: JobId,
        started: Instant,
    ) -> Result<(), WorkflowRunError> {
        #[cfg(test)]
        if let Some(sync) = &self.options.capacity_deferred_sync {
            sync.observed.notify_one();
            sync.resume.notified().await;
        }
        let timeout_source = VoomError::NoEligibleWorker(format!(
            "workflow {job_id} worker capacity remained full for {:?}",
            self.options.queue.capacity_retry_timeout
        ));
        self.wait_for_external_progress(state, job_id, started, timeout_source)
            .await
    }

    async fn wait_for_externally_leased_work(
        &self,
        state: &mut RunLoopState,
        job_id: JobId,
        started: Instant,
    ) -> Result<(), WorkflowRunError> {
        let job = match self.control_plane.jobs.get(job_id).await {
            Ok(Some(job)) => job,
            Ok(None) => {
                let source = VoomError::NotFound(format!("job {job_id}"));
                return Err(state
                    .fail_job(&self.control_plane, job_id, source, started)
                    .await);
            }
            Err(source) => {
                return Err(state
                    .fail_job(&self.control_plane, job_id, source, started)
                    .await);
            }
        };
        match job.state {
            JobState::Open => {}
            JobState::Cancelled => {
                let source = VoomError::UserCancellation(format!(
                    "workflow {job_id} cancelled while waiting for externally leased work"
                ));
                return Err(state
                    .finish_isolated_failure(&self.control_plane, job_id, source, started)
                    .await);
            }
            JobState::Succeeded | JobState::Failed => {
                let source = VoomError::Conflict(format!(
                    "workflow external lease wait rejected: job {job_id} is {}",
                    job.state.as_str()
                ));
                return Err(state
                    .fail_job(&self.control_plane, job_id, source, started)
                    .await);
            }
        }
        let interval = self.options.queue.capacity_retry_interval;
        if interval.is_zero() {
            let source =
                VoomError::Config("worker capacity retry interval must be positive".to_owned());
            return Err(state
                .fail_job(&self.control_plane, job_id, source, started)
                .await);
        }
        let expiry = match self
            .control_plane
            .expire_due(self.control_plane.clock().now())
            .await
        {
            Ok(expiry) => expiry,
            Err(source) => {
                return Err(state
                    .fail_job(&self.control_plane, job_id, source, started)
                    .await);
            }
        };
        if expiry.expired_leases.is_empty() {
            tokio::time::sleep(interval).await;
        }
        Ok(())
    }

    async fn wait_for_external_progress(
        &self,
        state: &mut RunLoopState,
        job_id: JobId,
        started: Instant,
        timeout_source: VoomError,
    ) -> Result<(), WorkflowRunError> {
        let control = &self.control_plane;
        match self.capacity_wait_outcome(state, job_id).await {
            Ok(CapacityWaitOutcome::RetryAfter(delay)) => {
                tokio::time::sleep(delay).await;
                Ok(())
            }
            Ok(CapacityWaitOutcome::TimedOut) => Err(state
                .fail_job(control, job_id, timeout_source, started)
                .await),
            Ok(CapacityWaitOutcome::Cancelled) => {
                let source = VoomError::UserCancellation(format!(
                    "workflow {job_id} cancelled while waiting for worker capacity"
                ));
                Err(state
                    .finish_isolated_failure(control, job_id, source, started)
                    .await)
            }
            Err(source) => Err(state.fail_job(control, job_id, source, started).await),
        }
    }

    async fn wait_for_accelerator_recovery(
        &self,
        state: &mut RunLoopState,
        job_id: JobId,
        started: Instant,
    ) -> Result<(), WorkflowRunError> {
        let job = match self.control_plane.jobs.get(job_id).await {
            Ok(Some(job)) => job,
            Ok(None) => {
                return Err(state
                    .fail_job(
                        &self.control_plane,
                        job_id,
                        VoomError::NotFound(format!("job {job_id}")),
                        started,
                    )
                    .await);
            }
            Err(source) => {
                return Err(state
                    .fail_job(&self.control_plane, job_id, source, started)
                    .await);
            }
        };
        match job.state {
            JobState::Open => {}
            JobState::Cancelled => {
                let source = VoomError::UserCancellation(format!(
                    "workflow {job_id} cancelled while waiting for accelerator recovery"
                ));
                return Err(state
                    .finish_isolated_failure(&self.control_plane, job_id, source, started)
                    .await);
            }
            JobState::Succeeded | JobState::Failed => {
                let source = VoomError::Conflict(format!(
                    "accelerator recovery wait rejected: job {job_id} is {}",
                    job.state.as_str()
                ));
                return Err(state
                    .fail_job(&self.control_plane, job_id, source, started)
                    .await);
            }
        }
        let interval = self.options.queue.capacity_retry_interval;
        let timeout = self.options.queue.accelerator_unavailable_timeout;
        if interval.is_zero() || timeout.is_zero() {
            let source = VoomError::Config(
                "accelerator retry interval and unavailable timeout must be positive".to_owned(),
            );
            return Err(state
                .fail_job(&self.control_plane, job_id, source, started)
                .await);
        }
        if let Some(delay) = state.accelerator_wait_delay(interval, timeout) {
            tokio::time::sleep(delay).await;
        }
        Ok(())
    }

    async fn wait_or_fail_idle(
        &self,
        state: &mut RunLoopState,
        invocation: &RunInvocation<'_>,
        dispatch: DispatchReadyOutcome,
        started: Instant,
    ) -> Result<(), WorkflowRunError> {
        if !state.accelerator_wait_started.is_empty() {
            return self
                .wait_for_accelerator_recovery(state, invocation.job_id, started)
                .await;
        }
        if dispatch.capacity_deferred {
            return self
                .wait_for_external_capacity(state, invocation.job_id, started)
                .await;
        }
        let idle_state = match self
            .workflow_idle_state(invocation.job_id, invocation.workflow_id)
            .await
        {
            Ok(idle_state) => idle_state,
            Err(source) => {
                return Err(state
                    .fail_job(&self.control_plane, invocation.job_id, source, started)
                    .await);
            }
        };
        match idle_state {
            WorkflowIdleState::Finished => return Ok(()),
            WorkflowIdleState::Leased => {
                return self
                    .wait_for_externally_leased_work(state, invocation.job_id, started)
                    .await;
            }
            WorkflowIdleState::Ready | WorkflowIdleState::Blocked => {}
        }
        match self
            .retry_delay(
                invocation.job_id,
                invocation.workflow_id,
                self.control_plane.clock().now(),
            )
            .await
        {
            Ok(Some(delay)) => {
                tokio::time::sleep(delay).await;
                return Ok(());
            }
            Ok(None) => {}
            Err(source) => {
                return Err(state
                    .fail_job(&self.control_plane, invocation.job_id, source, started)
                    .await);
            }
        }
        if idle_state == WorkflowIdleState::Ready {
            return Ok(());
        }
        if let Some(source) = state.take_isolated_error() {
            return Err(state
                .finish_isolated_failure(&self.control_plane, invocation.job_id, source, started)
                .await);
        }
        let source = no_dispatchable_work(invocation.job_id);
        Err(state
            .fail_job(&self.control_plane, invocation.job_id, source, started)
            .await)
    }

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
            return Err(WorkflowRunError {
                summary,
                source,
                job_failed: false,
                disposition: WorkflowFailureDisposition::Fatal,
                dispatch_started: false,
            });
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
                return Err(WorkflowRunError {
                    summary,
                    source,
                    job_failed: false,
                    disposition: WorkflowFailureDisposition::Fatal,
                    dispatch_started: false,
                });
            }
        };

        let workflow_id = format!("workflow-{}", job.id.0);
        let summary = self
            .run_plan_in_job(
                job.id,
                &workflow_id,
                plan,
                started,
                RunFailureMode::AbortJob,
                None,
            )
            .await?;
        let _ = self
            .control_plane
            .succeed_job(job.id, self.control_plane.clock().now())
            .await;
        Ok(summary)
    }

    /// Run one phase invocation inside a caller-owned, already-open job.
    #[cfg(test)]
    pub(crate) async fn submit_and_run_invocation_in_job(
        &self,
        job_id: JobId,
        invocation_id: &str,
        plan: WorkflowPlan,
        failure_mode: RunFailureMode,
    ) -> Result<WorkflowRunSummary, WorkflowRunError> {
        let started = Instant::now();
        if let Err(source) = plan
            .validate()
            .map_err(|e| VoomError::Config(format!("workflow plan invalid: {e}")))
        {
            let mut state = RunLoopState::new(job_id, started.elapsed());
            return Err(state
                .fail_job(&self.control_plane, job_id, source, started)
                .await);
        }
        let workflow_id = format!("workflow-{}-{invocation_id}", job_id.0);
        self.run_plan_in_job(job_id, &workflow_id, plan, started, failure_mode, None)
            .await
    }

    /// Run one phase invocation after atomically validating its planned file
    /// lineage and creating every root ticket.
    pub(crate) async fn submit_and_run_guarded_invocation_in_job(
        &self,
        job_id: JobId,
        invocation_id: &str,
        plan: WorkflowPlan,
        failure_mode: RunFailureMode,
        lineage_guard: PlannedLineageGuard,
    ) -> Result<WorkflowRunSummary, WorkflowRunError> {
        let started = Instant::now();
        if let Err(source) = plan
            .validate()
            .map_err(|e| VoomError::Config(format!("workflow plan invalid: {e}")))
        {
            let mut state = RunLoopState::new(job_id, started.elapsed());
            return Err(state
                .fail_job(&self.control_plane, job_id, source, started)
                .await);
        }
        let workflow_id = format!("workflow-{}-{invocation_id}", job_id.0);
        self.run_plan_in_job(
            job_id,
            &workflow_id,
            plan,
            started,
            failure_mode,
            Some(lineage_guard),
        )
        .await
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
        workflow_id: &str,
        plan: WorkflowPlan,
        started: Instant,
        failure_mode: RunFailureMode,
        lineage_guard: Option<PlannedLineageGuard>,
    ) -> Result<WorkflowRunSummary, WorkflowRunError> {
        let now = self.control_plane.clock().now();
        let mut state = RunLoopState::new(job_id, started.elapsed());
        let control = &self.control_plane;
        let invocation = RunInvocation {
            job_id,
            workflow_id,
            plan: &plan,
            failure_mode,
        };

        let root_result = match lineage_guard {
            Some(guard) => {
                self.create_guarded_root_tickets(&plan, workflow_id, job_id, now, &guard)
                    .await
            }
            None => {
                self.create_root_tickets(&plan, workflow_id, job_id, now)
                    .await
            }
        };
        if let Err(source) = root_result {
            return Err(state.fail_job(control, job_id, source, started).await);
        }
        state.dispatch_started = true;

        loop {
            state.process_completed_dispatches(self, &invocation).await;
            self.settle_node_local_step(&mut state, &invocation, started)
                .await?;

            if let Err(source) = state.refresh(control, job_id, started).await {
                return Err(state.fail_job(control, job_id, source, started).await);
            }
            if let Some(source) = state.take_fatal_error() {
                return Err(state
                    .fail_after_drain(self, &invocation, source, started)
                    .await);
            }
            let finished = match self.workflow_finished(job_id, workflow_id).await {
                Ok(finished) => finished,
                Err(source) => {
                    return Err(state.fail_job(control, job_id, source, started).await);
                }
            };
            if state.active_is_empty() && finished {
                match self.first_failed_ticket_error(job_id, workflow_id).await {
                    Ok(None) => match state.finish_success(control, job_id, started).await {
                        Ok(summary) => return Ok(summary),
                        Err(source) => {
                            return Err(state.fail_job(control, job_id, source, started).await);
                        }
                    },
                    Ok(Some(source)) => {
                        let source = state.take_isolated_error().unwrap_or(source);
                        return match failure_mode {
                            RunFailureMode::AbortJob => {
                                Err(state.fail_job(control, job_id, source, started).await)
                            }
                            RunFailureMode::ContinueIndependent => Err(state
                                .finish_isolated_failure(control, job_id, source, started)
                                .await),
                        };
                    }
                    Err(source) => {
                        return Err(state.fail_job(control, job_id, source, started).await);
                    }
                }
            }

            let dispatch = self.dispatch_ready_tickets(&mut state, &invocation).await;
            state.update_accelerator_waits(&dispatch);
            if let Some(hardware_token) = state
                .timed_out_accelerator(self.options.queue.accelerator_unavailable_timeout)
                .map(str::to_owned)
            {
                let source = VoomError::NoEligibleWorker(format!(
                    "accelerator {hardware_token} remained unavailable for {:?}",
                    self.options.queue.accelerator_unavailable_timeout
                ));
                return Err(state
                    .fail_after_drain(self, &invocation, source, started)
                    .await);
            }
            if dispatch.made_progress {
                state.reset_capacity_wait();
                continue;
            }

            if state.active_is_empty() {
                self.wait_or_fail_idle(&mut state, &invocation, dispatch, started)
                    .await?;
                continue;
            }
            state.reset_capacity_wait();
            if !state.accelerator_wait_started.is_empty() {
                self.wait_for_accelerator_recovery(&mut state, job_id, started)
                    .await?;
                continue;
            }
            state.wait_for_one(self, &invocation).await;
        }
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
