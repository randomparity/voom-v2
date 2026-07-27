//! The executor's dispatch seam: spawning ticket dispatches onto the join set,
//! processing joined dispatch outcomes, worker-candidate selection, and the
//! local reservation/capacity bookkeeping. Named `spawn` to avoid clashing with
//! the sibling `workflow::execution::dispatch` module.

use std::collections::HashMap;

use tokio::task::JoinSet;
use voom_core::OperationKind;
use voom_core::{JobId, TicketId, TicketOperation, VoomError, WorkerId};
use voom_scheduler::{SingleWorkerPerKindSelector, WorkerSelector, WorkerView};
use voom_store::repo::leases::NewLease;
use voom_store::repo::tickets::{Ticket, TicketState};

use crate::workflow::execution::dispatch::{DispatchOutcome, DispatchTerminal, dispatch_ticket};
use crate::workflow::execution::executor::RunFailureMode;
use crate::workflow::execution::executor::WorkflowExecutor;
use crate::workflow::execution::executor::errors::selector_failure_class;
use crate::workflow::execution::executor::tickets::parse_payload;
use crate::workflow::execution::leases::{
    acquire_lease_with_retry, failure_class_for_error, time_duration,
};
use crate::workflow::execution::operation_adapters::uses_bundled_policy_verification;
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
        let uses_bundled_verify = uses_bundled_policy_verification(
            workflow_payload.operation,
            &workflow_payload.rendered_payload,
        );
        let runtime = if uses_bundled_verify {
            None
        } else {
            Some(self.runtimes.get(worker_id)?)
        };
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
                worker_id,
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
        failure_mode: RunFailureMode,
        fatal_error: &mut Option<VoomError>,
        isolated_error: &mut Option<VoomError>,
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
                    *fatal_error = Some(source);
                }
            }
            DispatchTerminal::Failure { source } => {
                let class = match self.ticket_failure_class(outcome.ticket_id).await {
                    Ok(Some(class)) => class,
                    Ok(None) => failure_class_for_error(&source),
                    Err(err) => {
                        summary.record_failure(outcome.operation, failure_class_for_error(&source));
                        *fatal_error = Some(err);
                        return;
                    }
                };
                summary.record_failure(outcome.operation, class);
                match self.control_plane.tickets.get(outcome.ticket_id).await {
                    Ok(Some(ticket)) if ticket.state == TicketState::Failed => match failure_mode {
                        RunFailureMode::AbortJob => *fatal_error = Some(source),
                        RunFailureMode::ContinueIndependent => {
                            if isolated_error.is_none() {
                                *isolated_error = Some(source);
                            }
                        }
                    },
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        *fatal_error = Some(VoomError::NotFound(format!(
                            "ticket {} vanished after dispatch failure",
                            outcome.ticket_id
                        )));
                    }
                    Err(err) => {
                        *fatal_error = Some(err);
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
        let candidates = self
            .control_plane
            .workers
            .operation_candidates(&TicketOperation::from(operation))
            .await?;
        Ok(candidates
            .into_iter()
            .map(|candidate| WorkerView {
                worker_id: candidate.worker_id,
                supports: vec![operation],
                active_leases: candidate
                    .active_leases
                    .saturating_add(reservations.get(&candidate.worker_id).copied().unwrap_or(0)),
                max_parallel: candidate.max_parallel,
            })
            .collect())
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
