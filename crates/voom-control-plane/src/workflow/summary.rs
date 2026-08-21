use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use voom_core::OperationKind;
use voom_core::{FailureClass, JobId, VoomError, WorkerId};
use voom_store::repo::execution::leases::{LeaseInterval, SqliteLeaseRepo};
use voom_store::repo::execution::tickets::{SqliteTicketRepo, TicketState};

use super::plan::ticket_payload::WorkflowTicketPayload;

#[derive(Debug, Clone)]
pub struct WorkflowRunSummary {
    #[cfg(test)]
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
    /// Latch for the undecodable-payload warning, so it is emitted once per run
    /// rather than once per run-loop iteration. Not a durable summary field.
    warned_undecodable: bool,
}

impl WorkflowRunSummary {
    pub(crate) fn merge_invocation(&mut self, next: Self) {
        self.branch_count = self.branch_count.max(next.branch_count);
        self.ticket_count = self.ticket_count.max(next.ticket_count);
        self.dispatch_count += next.dispatch_count;
        self.retry_count = self.retry_count.max(next.retry_count);
        self.failure_count = self.failure_count.max(next.failure_count);
        self.peak_active_workflow_leases = self
            .peak_active_workflow_leases
            .max(next.peak_active_workflow_leases);
        self.elapsed = self.elapsed.max(next.elapsed);
        self.throughput_per_second = throughput(self.dispatch_count, self.elapsed);
        for (operation, next) in next.per_operation {
            let summary = self.per_operation.entry(operation).or_default();
            summary.ticket_count = summary.ticket_count.max(next.ticket_count);
            summary.dispatch_count += next.dispatch_count;
            summary.success_count += next.success_count;
            summary.retry_count += next.retry_count;
            summary.failure_count += next.failure_count;
            if next.last_failure_class.is_some() {
                summary.last_failure_class = next.last_failure_class;
            }
            summary.elapsed = summary.elapsed.max(next.elapsed);
            summary.throughput_per_second = throughput(summary.dispatch_count, summary.elapsed);
        }
        for (worker_id, active) in next.max_active_by_worker {
            let maximum = self.max_active_by_worker.entry(worker_id).or_default();
            *maximum = (*maximum).max(active);
        }
        // Merging a run that already warned keeps the latch closed, so a merged
        // summary does not re-announce a condition an operator has already seen.
        self.warned_undecodable |= next.warned_undecodable;
    }

    #[cfg(test)]
    #[must_use]
    pub fn operation_count(&self, operation: OperationKind) -> u64 {
        self.per_operation
            .get(&operation)
            .map_or(0, |summary| summary.success_count)
    }

    #[cfg(test)]
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

impl WorkflowRunSummary {
    pub(super) fn empty(
        #[cfg_attr(
            not(test),
            expect(
                unused_variables,
                reason = "job_id is retained in test summaries for durable workflow assertions"
            )
        )]
        job_id: JobId,
        elapsed: Duration,
    ) -> Self {
        Self {
            #[cfg(test)]
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
            warned_undecodable: false,
        }
    }

    pub(super) fn record_dispatch(
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

    pub(super) fn record_success(&mut self, operation: OperationKind) {
        self.per_operation
            .entry(operation)
            .or_default()
            .success_count += 1;
    }

    pub(super) fn record_failure(&mut self, operation: OperationKind, class: FailureClass) {
        let summary = self.per_operation.entry(operation).or_default();
        summary.failure_count += 1;
        summary.last_failure_class = Some(class);
    }

    pub(super) async fn refresh_counts(
        &mut self,
        tickets: &SqliteTicketRepo,
        leases: &SqliteLeaseRepo,
        job_id: JobId,
        elapsed: Duration,
    ) -> Result<(), VoomError> {
        self.elapsed = elapsed;
        self.throughput_per_second = throughput(self.dispatch_count, elapsed);
        let tickets = tickets.list_for_job(job_id).await?;
        self.ticket_count = u32::try_from(tickets.len()).unwrap_or(u32::MAX);
        let retry_count = tickets.iter().fold(0_u64, |total, ticket| {
            total.saturating_add(u64::from(ticket.attempt.saturating_sub(1)))
        });
        let failure_count = tickets
            .iter()
            .filter(|ticket| ticket.state == TicketState::Failed)
            .count();
        self.retry_count = retry_count;
        self.failure_count = self
            .failure_count
            .max(u64::try_from(failure_count).unwrap_or(u64::MAX));

        let mut branches = HashSet::new();
        let mut ticket_counts: BTreeMap<OperationKind, u64> = BTreeMap::new();
        let mut skipped = 0_u32;
        let mut first_skipped = None;
        for ticket in tickets {
            let ticket_id = ticket.id;
            let workflow_payload =
                match WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload) {
                    Ok(payload) => payload,
                    Err(error) => {
                        skipped += 1;
                        if first_skipped.is_none() {
                            first_skipped = Some((ticket_id.0, error.to_string()));
                        }
                        continue;
                    }
                };
            if !is_synthetic_root_ticket(&workflow_payload) {
                branches.insert(workflow_payload.branch_id);
            }
            *ticket_counts.entry(workflow_payload.operation).or_default() += 1;
        }
        // Skipping is right — a payload this code cannot read says nothing about
        // branches or per-operation counts. Saying so is also right: after the ADR
        // 0068 payload break an undrained pre-upgrade ticket lands here, and one
        // that never leases never reaches the terminal transition that would open
        // an ADR 0018 issue, so silence would leave an incomplete drain visible
        // nowhere while under-reporting the counts an operator checks against it.
        //
        // Once per run, not once per refresh and not once per ticket. Both
        // multipliers are real: `refresh_counts` runs on every run-loop iteration
        // (`executor/mod.rs`), over durable rows whose state cannot change until an
        // operator drains them. Left unlatched this repeats for the life of the run
        // and buries the rest of the run-loop output — during exactly the incident
        // an operator is reading these logs for. A later refresh that skips more
        // tickets stays silent; the drain, not the count, is the action.
        if skipped > 0 && !self.warned_undecodable {
            self.warned_undecodable = true;
            let (ticket_id, error) = first_skipped.unwrap_or_default();
            tracing::warn!(
                skipped_ticket_count = skipped,
                example_ticket_id = ticket_id,
                example_error = %error,
                "workflow summary skipped tickets whose payloads did not decode; \
                 branch and per-operation counts under-report by that many"
            );
        }
        self.branch_count = u32::try_from(branches.len()).unwrap_or(u32::MAX);
        for (operation, count) in ticket_counts {
            let operation_summary = self.per_operation.entry(operation).or_default();
            operation_summary.ticket_count = count;
            operation_summary.elapsed = elapsed;
            operation_summary.throughput_per_second =
                throughput(operation_summary.dispatch_count, elapsed);
        }

        let intervals = leases.timeline_for_job(job_id).await?;
        let mut transitions = Vec::with_capacity(intervals.len() * 2);
        for LeaseInterval {
            acquired_at,
            released_at,
            ..
        } in intervals
        {
            transitions.push((acquired_at, 1_i32));
            if let Some(released_at) = released_at {
                transitions.push((released_at, -1_i32));
            }
        }
        transitions.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        let mut active = 0_i32;
        let mut peak = 0_i32;
        for (_, delta) in transitions {
            active += delta;
            peak = peak.max(active);
        }
        self.peak_active_workflow_leases = self
            .peak_active_workflow_leases
            .max(u32::try_from(peak).unwrap_or(0));
        Ok(())
    }
}

pub(crate) fn is_synthetic_root_ticket(payload: &WorkflowTicketPayload) -> bool {
    payload.branch_id == "root"
        && payload.node_id == "scan"
        && payload.operation == OperationKind::ScanLibrary
        && payload.source_file.is_none()
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

#[cfg(test)]
#[path = "summary_test.rs"]
mod tests;
