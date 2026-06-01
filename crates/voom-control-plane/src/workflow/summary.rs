use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use serde_json::Value;
use sqlx::Row;
use voom_core::OperationKind;
use voom_core::{FailureClass, JobId, WorkerId};

use super::plan::ticket_payload::WorkflowTicketPayload;
use crate::ControlPlane;

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

impl WorkflowRunSummary {
    pub(super) fn empty(job_id: JobId, elapsed: Duration) -> Self {
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
        control: &ControlPlane,
        job_id: JobId,
        elapsed: Duration,
    ) {
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

fn sqlite_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn sqlite_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn sqlite_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}
