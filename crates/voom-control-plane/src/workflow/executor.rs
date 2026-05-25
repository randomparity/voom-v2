use std::collections::{BTreeMap, HashMap, HashSet};
use std::pin::Pin;
use std::time::{Duration, Instant};

use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;
use tokio::task::JoinSet;
use voom_core::{ErrorCode, FailureClass, JobId, LeaseId, TicketId, VoomError, WorkerId};
use voom_scheduler::{SingleWorkerPerKindSelector, WorkerSelector, WorkerView};
use voom_store::repo::identity::IdentityRepo;
use voom_store::repo::jobs::NewJob;
use voom_store::repo::leases::NewLease;
use voom_store::repo::tickets::{NewTicket, Ticket, TicketRepo, TicketState};
use voom_worker_protocol::{
    DispatchStream, NdjsonOutcome, OperationKind, OperationRequest, ProgressFrame, ProtocolError,
};

use super::binding::{
    BranchContext, PolicyTranscodeSource, render_default_payload,
    render_default_payload_with_fan_out, render_policy_transcode_payload,
};
use super::expansion::{
    ExpansionContext, expand_backup_completion, expand_probe_completion, expand_quality_completion,
    expand_scanner_completion, expand_transform_completion,
};
use super::model::WorkflowPlan;
use super::runtime::WorkerRuntimeRegistry;
use super::ticket_payload::{WorkflowTicketPayload, operation_name};
use super::timing::seeded_timing;
use crate::ControlPlane;

const WORKFLOW_JOB_KIND: &str = "synthetic.workflow";
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

    fn suppresses_heartbeats_for(&self, operation: OperationKind) -> bool {
        self.disable_heartbeat_ticks || self.suppress_heartbeat_operation == Some(operation)
    }

    fn payload_mode_for(&self, operation: OperationKind) -> Option<&str> {
        self.payload_modes.get(&operation).map(String::as_str)
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowRunSummary {
    pub job_id: JobId,
    pub branch_count: u32,
    pub ticket_count: u32,
    pub dispatch_count: u64,
    pub retry_count: u64,
    pub failure_count: u64,
    pub peak_active_workflow_leases: u32,
    pub elapsed: Duration,
    /// Total dispatch throughput across the workflow run.
    pub throughput_per_second: f64,
    pub per_operation: BTreeMap<OperationKind, OperationSummary>,
    max_active_by_worker: BTreeMap<WorkerId, u32>,
}

impl WorkflowRunSummary {
    #[must_use]
    pub fn operation_count(&self, operation: OperationKind) -> u64 {
        self.per_operation
            .get(&operation)
            .map_or(0, |summary| summary.success_count)
    }

    #[must_use]
    pub fn max_active_for_worker(&self, worker_id: WorkerId) -> u32 {
        self.max_active_by_worker
            .get(&worker_id)
            .copied()
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct OperationSummary {
    pub ticket_count: u64,
    pub dispatch_count: u64,
    pub success_count: u64,
    pub retry_count: u64,
    pub failure_count: u64,
    pub last_failure_class: Option<FailureClass>,
    /// Workflow run duration used as the measurement window for this operation summary.
    pub elapsed: Duration,
    /// Dispatch throughput for this operation over the full workflow run window.
    pub throughput_per_second: f64,
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

    #[expect(
        clippy::too_many_lines,
        reason = "workflow run loop keeps scheduler state and terminal handling together"
    )]
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
        let workflow_id = format!("workflow-{}", job.id.0);
        let mut summary = WorkflowRunSummary::empty(job.id, started.elapsed());

        if let Err(source) = self
            .create_root_tickets(&plan, &workflow_id, job.id, now)
            .await
        {
            let _ = self
                .control_plane
                .fail_job(job.id, source.to_string(), self.control_plane.clock().now())
                .await;
            summary
                .refresh_counts(&self.control_plane, job.id, started.elapsed())
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
                    job.id,
                    &mut reservations,
                    &mut summary,
                    &mut terminal_error,
                )
                .await;
            }

            summary
                .refresh_counts(&self.control_plane, job.id, started.elapsed())
                .await;
            if let Some(source) = terminal_error.take() {
                let _ = self
                    .control_plane
                    .fail_job(job.id, source.to_string(), self.control_plane.clock().now())
                    .await;
                summary
                    .refresh_counts(&self.control_plane, job.id, started.elapsed())
                    .await;
                return Err(WorkflowRunError { summary, source });
            }
            if active.is_empty() && self.workflow_finished(job.id).await {
                if let Some(source) = self.first_failed_ticket_error(job.id).await {
                    let _ = self
                        .control_plane
                        .fail_job(job.id, source.to_string(), self.control_plane.clock().now())
                        .await;
                    summary
                        .refresh_counts(&self.control_plane, job.id, started.elapsed())
                        .await;
                    return Err(WorkflowRunError { summary, source });
                }
                let _ = self
                    .control_plane
                    .succeed_job(job.id, self.control_plane.clock().now())
                    .await;
                summary
                    .refresh_counts(&self.control_plane, job.id, started.elapsed())
                    .await;
                return Ok(summary);
            }

            let mut dispatched_or_failed = false;
            let max_in_flight = plan.concurrency.max_in_flight_dispatches;
            while active.len() < max_in_flight {
                let Some(ticket) = self.next_ready_workflow_ticket(job.id, &workflow_id).await
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
                    .retry_delay(job.id, &workflow_id, self.control_plane.clock().now())
                    .await
                {
                    tokio::time::sleep(delay).await;
                    continue;
                }
                let source = VoomError::Internal(format!(
                    "workflow {} has no dispatchable work but is not finished",
                    job.id
                ));
                let _ = self
                    .control_plane
                    .fail_job(job.id, source.to_string(), self.control_plane.clock().now())
                    .await;
                summary
                    .refresh_counts(&self.control_plane, job.id, started.elapsed())
                    .await;
                return Err(WorkflowRunError { summary, source });
            }
            if let Some(joined) = active.join_next().await {
                self.process_joined_dispatch(
                    joined,
                    &plan,
                    &workflow_id,
                    job.id,
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
            let rendered_payload = match operation {
                OperationKind::ScanLibrary => render_default_payload_with_fan_out(
                    operation,
                    &branch,
                    timing,
                    plan.fan_out.max_files,
                ),
                OperationKind::TranscodeVideo => match node.policy_target() {
                    Some(target) => render_policy_transcode_payload(
                        self.resolve_policy_transcode_source(target).await?,
                        node.operation_payload(),
                        timing,
                    ),
                    None => render_default_payload(operation, &branch, timing),
                },
                _ => render_default_payload(operation, &branch, timing),
            }
            .map_err(|e| VoomError::Config(format!("workflow root payload binding: {e}")))?;
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
        }
        Ok(())
    }

    async fn resolve_policy_transcode_source(
        &self,
        target: &voom_plan::TargetRef,
    ) -> Result<PolicyTranscodeSource, VoomError> {
        match target {
            voom_plan::TargetRef::FileVersion { id } => Ok(PolicyTranscodeSource {
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
                Ok(PolicyTranscodeSource {
                    file_version_id: location.file_version_id,
                    location_id: Some(*id),
                })
            }
            other => Err(VoomError::Config(format!(
                "transcode_video requires file_version or file_location target, got {other:?}"
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
            _ => {}
        }
        Ok(())
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

impl WorkflowRunSummary {
    fn empty(job_id: JobId, elapsed: Duration) -> Self {
        Self {
            job_id,
            branch_count: 0,
            ticket_count: 0,
            dispatch_count: 0,
            retry_count: 0,
            failure_count: 0,
            peak_active_workflow_leases: 0,
            elapsed,
            throughput_per_second: 0.0,
            per_operation: BTreeMap::new(),
            max_active_by_worker: BTreeMap::new(),
        }
    }

    fn record_dispatch(
        &mut self,
        operation: OperationKind,
        worker_id: WorkerId,
        reservations: &HashMap<WorkerId, u32>,
    ) {
        self.per_operation
            .entry(operation)
            .or_default()
            .dispatch_count += 1;
        let active_total: u32 = reservations.values().copied().sum();
        self.peak_active_workflow_leases = self.peak_active_workflow_leases.max(active_total);
        let active_for_worker = reservations.get(&worker_id).copied().unwrap_or(0);
        let max_for_worker = self.max_active_by_worker.entry(worker_id).or_default();
        *max_for_worker = (*max_for_worker).max(active_for_worker);
    }

    fn record_success(&mut self, operation: OperationKind) {
        self.per_operation
            .entry(operation)
            .or_default()
            .success_count += 1;
    }

    fn record_failure(&mut self, operation: OperationKind, class: FailureClass) {
        let summary = self.per_operation.entry(operation).or_default();
        summary.failure_count += 1;
        summary.last_failure_class = Some(class);
    }

    async fn refresh_counts(&mut self, control: &ControlPlane, job_id: JobId, elapsed: Duration) {
        self.elapsed = elapsed;
        self.throughput_per_second = throughput(self.dispatch_count, elapsed);
        if let Ok((ticket_count, retry_count, failure_count)) = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN attempt > 1 THEN attempt - 1 ELSE 0 END), 0), \
                    SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END) \
             FROM tickets WHERE job_id = ?",
        )
        .bind(sqlite_i64(job_id.0))
        .fetch_one(&control.pool)
        .await
        {
            self.ticket_count = sqlite_u32(ticket_count);
            self.retry_count = sqlite_u64(retry_count);
            self.failure_count = self.failure_count.max(sqlite_u64(failure_count));
        }
        if let Ok(rows) = sqlx::query("SELECT kind, payload, state FROM tickets WHERE job_id = ?")
            .bind(sqlite_i64(job_id.0))
            .fetch_all(&control.pool)
            .await
        {
            let mut branches = HashSet::new();
            let mut ticket_counts: BTreeMap<OperationKind, u64> = BTreeMap::new();
            for row in rows {
                let Ok(kind) = row.try_get::<String, _>("kind") else {
                    continue;
                };
                let Ok(payload_json) = row.try_get::<String, _>("payload") else {
                    continue;
                };
                let Ok(payload) = serde_json::from_str::<Value>(&payload_json) else {
                    continue;
                };
                let Ok(workflow_payload) = WorkflowTicketPayload::parse_ticket(&kind, payload)
                else {
                    continue;
                };
                if !is_synthetic_root_ticket(&workflow_payload) {
                    branches.insert(workflow_payload.branch_id);
                }
                *ticket_counts.entry(workflow_payload.operation).or_default() += 1;
            }
            self.branch_count = u32::try_from(branches.len()).unwrap_or(u32::MAX);
            for (operation, count) in ticket_counts {
                let operation_summary = self.per_operation.entry(operation).or_default();
                operation_summary.ticket_count = count;
                operation_summary.elapsed = elapsed;
                operation_summary.throughput_per_second =
                    throughput(operation_summary.dispatch_count, elapsed);
            }
        }
    }
}

pub(crate) fn is_synthetic_root_ticket(payload: &WorkflowTicketPayload) -> bool {
    payload.branch_id == "root"
        && payload.node_id == "scan"
        && payload.operation == OperationKind::ScanLibrary
        && payload.source_file.is_none()
}

#[derive(Debug)]
enum SpawnOutcome {
    Spawned,
    PreLeaseRetriable,
    PreLeaseTerminal(VoomError),
    CapacityDeferred,
}

#[derive(Debug)]
struct DispatchOutcome {
    ticket_id: TicketId,
    worker_id: WorkerId,
    operation: OperationKind,
    terminal: DispatchTerminal,
}

#[derive(Debug)]
enum DispatchTerminal {
    Success,
    Failure { source: VoomError },
}

async fn dispatch_ticket(
    control: ControlPlane,
    runtime: super::runtime::WorkerRuntime,
    ticket: Ticket,
    workflow_payload: WorkflowTicketPayload,
    lease_id: LeaseId,
    options: WorkflowExecutorOptions,
) -> DispatchOutcome {
    let worker_id = runtime.credentials.worker_id;
    let operation = workflow_payload.operation;
    let terminal = match dispatch_ticket_inner(
        &control,
        &runtime,
        &ticket,
        &workflow_payload,
        lease_id,
        options,
    )
    .await
    {
        Ok(()) => DispatchTerminal::Success,
        Err(source) => DispatchTerminal::Failure { source },
    };
    DispatchOutcome {
        ticket_id: ticket.id,
        worker_id,
        operation,
        terminal,
    }
}

async fn dispatch_ticket_inner(
    control: &ControlPlane,
    runtime: &super::runtime::WorkerRuntime,
    ticket: &Ticket,
    workflow_payload: &WorkflowTicketPayload,
    lease_id: LeaseId,
    options: WorkflowExecutorOptions,
) -> Result<(), VoomError> {
    let mut payload = workflow_payload.rendered_payload.clone();
    apply_chaos_payload_override(&mut payload, workflow_payload.operation, &options.chaos)?;
    let request = OperationRequest {
        operation: workflow_payload.operation,
        lease_id,
        payload,
        heartbeat_deadline_ms: duration_millis_u32(options.heartbeat_timeout),
        progress_idle_deadline_ms: duration_millis_u32(options.progress_idle_timeout),
    };
    let idempotency_key = format!("ticket-{}-lease-{}", ticket.id.0, lease_id.0);
    let dispatch_timeout = no_response_timeout(&options);
    let dispatch = tokio::time::timeout(
        dispatch_timeout,
        runtime
            .client
            .dispatch(&runtime.credentials, &idempotency_key, request),
    )
    .await
    .map_err(|_| {
        VoomError::WorkerTimeout(format!(
            "dispatch response timeout for lease {lease_id} after {dispatch_timeout:?}"
        ))
    })
    .and_then(|result| result.map_err(|err| map_dispatch_setup_protocol_error(&err)));
    let dispatch = match dispatch {
        Ok(dispatch) => dispatch,
        Err(source) => {
            return fail_lease_and_return(
                control,
                lease_id,
                failure_class_for_error(&source),
                source,
            )
            .await;
        }
    };
    if dispatch.response.lease_id != lease_id {
        return fail_lease_and_return(
            control,
            lease_id,
            FailureClass::MalformedWorkerResult,
            VoomError::MalformedWorkerResult(format!(
                "worker accepted lease {:?} for expected {:?}",
                dispatch.response.lease_id, lease_id
            )),
        )
        .await;
    }
    consume_dispatch_stream(
        control,
        lease_id,
        workflow_payload.operation,
        dispatch,
        options,
    )
    .await
}

async fn consume_dispatch_stream(
    control: &ControlPlane,
    lease_id: LeaseId,
    operation: OperationKind,
    mut dispatch: DispatchStream,
    options: WorkflowExecutorOptions,
) -> Result<(), VoomError> {
    let mut last_progress = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut heartbeat = tokio::time::interval(options.heartbeat_interval);
    loop {
        let progress_deadline = sleep_until(last_progress + options.progress_idle_timeout);
        let heartbeat_deadline = sleep_until(last_heartbeat + options.heartbeat_timeout);
        tokio::pin!(progress_deadline);
        tokio::pin!(heartbeat_deadline);
        tokio::select! {
            biased;
            frame = dispatch.frames.next_frame() => {
                match frame {
                    Ok(NdjsonOutcome::Frame(frame)) => {
                        validate_frame_lease(&frame, lease_id)?;
                        fail_if_watchdog_elapsed(
                            control,
                            lease_id,
                            last_heartbeat,
                            last_progress,
                            &options,
                        )
                        .await?;
                        last_progress = Instant::now();
                        if !options.chaos.suppresses_heartbeats_for(operation) {
                            heartbeat_lease(control, lease_id, &mut last_heartbeat, &options).await?;
                        }
                    }
                    Ok(NdjsonOutcome::Terminated(frame)) => {
                        validate_frame_lease(&frame, lease_id)?;
                        fail_if_watchdog_elapsed(
                            control,
                            lease_id,
                            last_heartbeat,
                            last_progress,
                            &options,
                        )
                        .await?;
                        return handle_terminal_frame(control, lease_id, frame).await;
                    }
                    Ok(NdjsonOutcome::StreamEnd { .. } | NdjsonOutcome::Closed) => {
                        return fail_lease_and_return(
                            control,
                            lease_id,
                            FailureClass::WorkerCrash,
                            VoomError::WorkerCrash(format!("worker stream closed before terminal frame for lease {lease_id}")),
                        ).await;
                    }
                    Err(err) => {
                        return fail_lease_and_return(
                            control,
                            lease_id,
                            FailureClass::MalformedWorkerResult,
                            map_protocol_error(&err),
                        ).await;
                    }
                }
            }
            () = &mut heartbeat_deadline => {
                return fail_lease_and_return(
                    control,
                    lease_id,
                    FailureClass::WorkerTimeout,
                    VoomError::WorkerTimeout(format!("heartbeat timeout for lease {lease_id}")),
                ).await;
            }
            () = &mut progress_deadline => {
                return fail_lease_and_return(
                    control,
                    lease_id,
                    FailureClass::ProgressTimeout,
                    VoomError::WorkerTimeout(format!("progress timeout for lease {lease_id}")),
                ).await;
            }
            _ = heartbeat.tick(), if !options.chaos.suppresses_heartbeats_for(operation) => {
                heartbeat_lease(control, lease_id, &mut last_heartbeat, &options).await?;
            }
        }
    }
}

async fn fail_if_watchdog_elapsed(
    control: &ControlPlane,
    lease_id: LeaseId,
    last_heartbeat: Instant,
    last_progress: Instant,
    options: &WorkflowExecutorOptions,
) -> Result<(), VoomError> {
    let now = Instant::now();
    if now.duration_since(last_heartbeat) >= options.heartbeat_timeout {
        return fail_lease_and_return(
            control,
            lease_id,
            FailureClass::WorkerTimeout,
            VoomError::WorkerTimeout(format!("heartbeat timeout for lease {lease_id}")),
        )
        .await;
    }
    if now.duration_since(last_progress) >= options.progress_idle_timeout {
        return fail_lease_and_return(
            control,
            lease_id,
            FailureClass::ProgressTimeout,
            VoomError::WorkerTimeout(format!("progress timeout for lease {lease_id}")),
        )
        .await;
    }
    Ok(())
}

async fn handle_terminal_frame(
    control: &ControlPlane,
    lease_id: LeaseId,
    frame: ProgressFrame,
) -> Result<(), VoomError> {
    match frame {
        ProgressFrame::Result { payload, .. } => {
            if !payload.is_object() {
                return fail_lease_and_return(
                    control,
                    lease_id,
                    FailureClass::MalformedWorkerResult,
                    VoomError::MalformedWorkerResult(format!(
                        "result payload for lease {lease_id} must be an object"
                    )),
                )
                .await;
            }
            release_lease_with_retry(control, lease_id, payload).await?;
            Ok(())
        }
        ProgressFrame::Error { class, message, .. } => {
            let source = voom_error_for_failure_class(class, message);
            fail_lease_and_return(control, lease_id, class, source).await
        }
        ProgressFrame::Progress { .. } => Err(VoomError::Internal(
            "progress frame cannot be terminal".to_owned(),
        )),
    }
}

async fn heartbeat_lease(
    control: &ControlPlane,
    lease_id: LeaseId,
    last_heartbeat: &mut Instant,
    options: &WorkflowExecutorOptions,
) -> Result<(), VoomError> {
    heartbeat_lease_with_retry(control, lease_id, time_duration(options.lease_ttl)?).await?;
    *last_heartbeat = Instant::now();
    Ok(())
}

async fn fail_lease_and_return<T>(
    control: &ControlPlane,
    lease_id: LeaseId,
    class: FailureClass,
    source: VoomError,
) -> Result<T, VoomError> {
    fail_lease_with_retry(control, lease_id, source.to_string(), class).await?;
    Err(source)
}

fn parse_payload(ticket: &Ticket) -> Result<WorkflowTicketPayload, VoomError> {
    WorkflowTicketPayload::parse_ticket(&ticket.kind, ticket.payload.clone())
        .map_err(|e| VoomError::Config(format!("workflow ticket payload decode: {e}")))
}

fn ticket_kind(operation: OperationKind) -> String {
    format!("synthetic.workflow.operation.{}", operation_name(operation))
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

fn map_protocol_error(err: &ProtocolError) -> VoomError {
    match err {
        ProtocolError::MalformedFrame { detail } => {
            VoomError::MalformedWorkerResult(detail.clone())
        }
        ProtocolError::WrongLeaseId { .. }
        | ProtocolError::OutOfOrderFrame { .. }
        | ProtocolError::UnexpectedFrameAfterTerminal
        | ProtocolError::InvalidPayload { .. } => VoomError::MalformedWorkerResult(err.to_string()),
        _ => VoomError::WorkerCrash(err.to_string()),
    }
}

fn map_dispatch_setup_protocol_error(err: &ProtocolError) -> VoomError {
    match err {
        ProtocolError::MalformedFrame { detail }
            if detail.contains("missing response/body separator")
                || detail.contains("response read") =>
        {
            VoomError::WorkerCrash(err.to_string())
        }
        ProtocolError::InvalidPayload { detail }
            if detail.contains("request:") || detail.contains("body:") =>
        {
            VoomError::WorkerCrash(err.to_string())
        }
        _ => map_protocol_error(err),
    }
}

fn voom_error_for_failure_class(class: FailureClass, message: String) -> VoomError {
    match class.into_error_code() {
        ErrorCode::WorkerTimeout => VoomError::WorkerTimeout(message),
        ErrorCode::WorkerCrash => VoomError::WorkerCrash(message),
        ErrorCode::NoEligibleWorker => VoomError::NoEligibleWorker(message),
        ErrorCode::ArtifactUnavailable => VoomError::ArtifactUnavailable(message),
        ErrorCode::ArtifactChecksumMismatch => VoomError::ArtifactChecksumMismatch(message),
        ErrorCode::ExternalSystemUnavailable => VoomError::ExternalSystemUnavailable(message),
        ErrorCode::ExternalSystemRateLimited => VoomError::ExternalSystemRateLimited(message),
        ErrorCode::VerificationFailure => VoomError::VerificationFailure(message),
        ErrorCode::BackupFailure => VoomError::BackupFailure(message),
        ErrorCode::CommitFailure => VoomError::CommitFailure(message),
        ErrorCode::PolicyParseError => VoomError::PolicyParseError(message),
        ErrorCode::PolicyValidationError => VoomError::PolicyValidationError(message),
        ErrorCode::MissingCapability => VoomError::MissingCapability(message),
        ErrorCode::MalformedWorkerResult => VoomError::MalformedWorkerResult(message),
        ErrorCode::UserCancellation => VoomError::UserCancellation(message),
        ErrorCode::StaleIdentityEvidence => VoomError::StaleIdentityEvidence(message),
        ErrorCode::ClosureResolutionIncomplete => VoomError::ClosureResolutionIncomplete(message),
        ErrorCode::BlockedByUseLease => VoomError::BlockedByUseLease(message),
        ErrorCode::ApprovalRequired => VoomError::ApprovalRequired(message),
        ErrorCode::PriorityPolicyConflict => VoomError::PriorityPolicyConflict(message),
        ErrorCode::AmbiguousWorkerSelection => VoomError::AmbiguousWorkerSelection(message),
        other => VoomError::Internal(format!(
            "unsupported worker failure code {other:?}: {message}"
        )),
    }
}

fn failure_class_for_error(source: &VoomError) -> FailureClass {
    match source.error_code() {
        ErrorCode::WorkerTimeout => FailureClass::WorkerTimeout,
        ErrorCode::NoEligibleWorker => FailureClass::NoEligibleWorker,
        ErrorCode::ArtifactUnavailable => FailureClass::ArtifactUnavailable,
        ErrorCode::ArtifactChecksumMismatch => FailureClass::ArtifactChecksumMismatch,
        ErrorCode::ExternalSystemUnavailable => FailureClass::ExternalSystemUnavailable,
        ErrorCode::ExternalSystemRateLimited => FailureClass::ExternalSystemRateLimited,
        ErrorCode::VerificationFailure => FailureClass::VerificationFailure,
        ErrorCode::BackupFailure => FailureClass::BackupFailure,
        ErrorCode::CommitFailure => FailureClass::CommitFailure,
        ErrorCode::PolicyParseError => FailureClass::PolicyParseError,
        ErrorCode::PolicyValidationError => FailureClass::PolicyValidationError,
        ErrorCode::MissingCapability => FailureClass::MissingCapability,
        ErrorCode::MalformedWorkerResult => FailureClass::MalformedWorkerResult,
        ErrorCode::UserCancellation => FailureClass::UserCancellation,
        ErrorCode::StaleIdentityEvidence => FailureClass::StaleIdentityEvidence,
        ErrorCode::ClosureResolutionIncomplete => FailureClass::ClosureResolutionIncomplete,
        ErrorCode::BlockedByUseLease => FailureClass::BlockedByActiveUseLease,
        ErrorCode::ApprovalRequired => FailureClass::ApprovalRequired,
        ErrorCode::PriorityPolicyConflict => FailureClass::PriorityPolicyConflict,
        ErrorCode::AmbiguousWorkerSelection => FailureClass::AmbiguousWorkerSelection,
        _ => FailureClass::WorkerCrash,
    }
}

fn apply_chaos_payload_override(
    payload: &mut Value,
    operation: OperationKind,
    chaos: &WorkflowChaosOptions,
) -> Result<(), VoomError> {
    let Some(mode) = chaos.payload_mode_for(operation) else {
        return Ok(());
    };
    let Some(object) = payload.as_object_mut() else {
        return Err(VoomError::Config(format!(
            "workflow chaos payload for {operation:?} must be an object"
        )));
    };
    object.insert("mode".to_owned(), Value::String(mode.to_owned()));
    Ok(())
}

fn no_response_timeout(options: &WorkflowExecutorOptions) -> Duration {
    options
        .heartbeat_timeout
        .min(options.progress_idle_timeout)
        .max(Duration::from_millis(1))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "throughput is an approximate reporting metric, not an exact counter"
)]
fn throughput(count: u64, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds > 0.0 {
        count as f64 / seconds
    } else if count > 0 {
        f64::INFINITY
    } else {
        0.0
    }
}

fn validate_frame_lease(frame: &ProgressFrame, lease_id: LeaseId) -> Result<(), VoomError> {
    if frame.lease_id() == lease_id {
        Ok(())
    } else {
        Err(VoomError::MalformedWorkerResult(format!(
            "wrong lease id in frame: expected {lease_id}, got {}",
            frame.lease_id()
        )))
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

async fn acquire_lease_with_retry(
    control: &ControlPlane,
    input: NewLease,
) -> Result<voom_store::repo::leases::Lease, VoomError> {
    let mut last = None;
    for _ in 0..8 {
        match control.acquire_lease(input.clone()).await {
            Ok(lease) => return Ok(lease),
            Err(err) if is_database_locked(&err) => {
                last = Some(err);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(|| VoomError::Database("database is locked".to_owned())))
}

async fn release_lease_with_retry(
    control: &ControlPlane,
    lease_id: LeaseId,
    payload: Value,
) -> Result<(), VoomError> {
    let mut last = None;
    for _ in 0..8 {
        match control
            .release_lease(lease_id, payload.clone(), control.clock().now())
            .await
        {
            Ok(_) => return Ok(()),
            Err(err) if is_database_locked(&err) => {
                last = Some(err);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(|| VoomError::Database("database is locked".to_owned())))
}

async fn fail_lease_with_retry(
    control: &ControlPlane,
    lease_id: LeaseId,
    reason: String,
    class: FailureClass,
) -> Result<(), VoomError> {
    let mut last = None;
    for _ in 0..8 {
        match control
            .fail_lease(lease_id, reason.clone(), class, control.clock().now())
            .await
        {
            Ok(_) => return Ok(()),
            Err(err) if is_database_locked(&err) => {
                last = Some(err);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(|| VoomError::Database("database is locked".to_owned())))
}

async fn heartbeat_lease_with_retry(
    control: &ControlPlane,
    lease_id: LeaseId,
    ttl: time::Duration,
) -> Result<(), VoomError> {
    let mut last = None;
    for _ in 0..8 {
        match control
            .heartbeat_lease(lease_id, ttl, control.clock().now())
            .await
        {
            Ok(_) => return Ok(()),
            Err(err) if is_database_locked(&err) => {
                last = Some(err);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(|| VoomError::Database("database is locked".to_owned())))
}

fn is_database_locked(err: &VoomError) -> bool {
    matches!(err, VoomError::Database(message) if message.contains("database is locked"))
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

fn time_duration(duration: Duration) -> Result<time::Duration, VoomError> {
    time::Duration::try_from(duration)
        .map_err(|e| VoomError::Config(format!("duration out of range: {e}")))
}

fn duration_millis_u32(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

fn format_time(t: OffsetDateTime) -> Result<String, VoomError> {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::Internal(format!("format iso8601: {e}")))
}

fn sleep_until(deadline: Instant) -> Pin<Box<tokio::time::Sleep>> {
    Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(
        deadline,
    )))
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
