//! The executor's dispatch seam: spawning ticket dispatches onto the join set,
//! processing joined dispatch outcomes, worker-candidate selection, and the
//! local reservation/capacity bookkeeping. Named `spawn` to avoid clashing with
//! the sibling `workflow::execution::dispatch` module.

use std::collections::HashMap;

use serde_json::Value;
use sqlx::Row;
use tokio::task::JoinSet;
use voom_core::OperationKind;
use voom_core::{JobId, TicketId, VoomError, WorkerId};
use voom_scheduler::{SingleWorkerPerKindSelector, WorkerSelector, WorkerView};
use voom_store::repo::leases::NewLease;
use voom_store::repo::tickets::{Ticket, TicketState};

use crate::workflow::execution::dispatch::{DispatchOutcome, DispatchTerminal, dispatch_ticket};
use crate::workflow::execution::executor::WorkflowExecutor;
use crate::workflow::execution::executor::errors::{
    selector_failure_class, sqlite_u32, sqlite_u64,
};
use crate::workflow::execution::executor::tickets::parse_payload;
use crate::workflow::execution::leases::{
    acquire_lease_with_retry, failure_class_for_error, time_duration,
};
use crate::workflow::plan::model::WorkflowPlan;
use crate::workflow::summary::WorkflowRunSummary;

#[derive(Debug)]
pub(super) enum SpawnOutcome {
    Spawned,
    PreLeaseRetriable,
    PreLeaseTerminal(VoomError),
    CapacityDeferred,
}

impl WorkflowExecutor {
    pub(super) async fn try_spawn_dispatch(
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
        let selector = SingleWorkerPerKindSelector;
        let worker_id = match selector.select(workflow_payload.operation, &candidates) {
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
                ttl: time_duration(self.options.timing.lease_ttl)?,
                now: self.control_plane.clock().now(),
            },
        )
        .await?;
        increment_reservation(reservations, worker_id);
        summary.dispatch_count += 1;
        summary.record_dispatch(workflow_payload.operation, worker_id, reservations);

        let control = self.control_plane.clone();
        let options = self.options.dispatch_options();
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
    pub(super) async fn process_joined_dispatch(
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
                let class = match self.ticket_failure_class(outcome.ticket_id).await {
                    Ok(Some(class)) => class,
                    Ok(None) => failure_class_for_error(&source),
                    Err(err) => {
                        summary.record_failure(outcome.operation, failure_class_for_error(&source));
                        *terminal_error = Some(err);
                        return;
                    }
                };
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

    async fn candidate_workers(
        &self,
        operation: OperationKind,
        reservations: &HashMap<WorkerId, u32>,
    ) -> Result<Vec<WorkerView>, VoomError> {
        let operation_name = operation.as_str();
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
        .map_err(|e| VoomError::database_context("workflow worker candidates", e))?;

        let mut views = Vec::new();
        for row in rows {
            let worker_id: i64 = row
                .try_get("worker_id")
                .map_err(|e| VoomError::database_context("worker candidate row", e))?;
            let can_execute: String = row
                .try_get("can_execute")
                .map_err(|e| VoomError::database_context("worker grant can_execute", e))?;
            let denies: String = row
                .try_get("denies")
                .map_err(|e| VoomError::database_context("worker grant denies", e))?;
            let max_parallel: String = row
                .try_get("max_parallel")
                .map_err(|e| VoomError::database_context("worker grant max_parallel", e))?;
            if !json_string_array_contains(&can_execute, operation_name)?
                || json_string_array_contains(&denies, operation_name)?
            {
                continue;
            }
            let worker_id = WorkerId(sqlite_u64(worker_id));
            let active_leases: i64 = row
                .try_get("active_leases")
                .map_err(|e| VoomError::database_context("worker active lease count", e))?;
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
        .map_err(|e| VoomError::database_context("parse worker grant array", e))?;
    Ok(values.iter().any(|value| value == needle))
}

fn max_parallel_for_operation(raw: &str, operation: &str) -> Result<u32, VoomError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| VoomError::database_context("parse worker max_parallel", e))?;
    let max = value
        .get(operation)
        .or_else(|| value.get("*"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    Ok(u32::try_from(max).unwrap_or(u32::MAX).max(1))
}
