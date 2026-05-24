#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::default_constructed_unit_structs,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Sprint 2 Phase 2: minimal `WorkerSelector` trait + a
//! single-worker-per-operation default implementation. Sprint 4
//! swaps in multi-worker scoring (capability + locality + cost)
//! behind the same trait without changing supervisor or test code.

use serde_json::{Value as JsonValue, json};
use voom_core::{FailureClass, NodeId, TicketId, VoomError, WorkerId};
use voom_worker_protocol::OperationKind;

pub const SCORING_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketCandidate {
    pub ticket_id: TicketId,
    pub operation: String,
    pub priority: i64,
    pub next_eligible_at_epoch_seconds: i64,
    pub payload: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "scheduler gate projections intentionally preserve independent boolean facts"
)]
pub struct WorkerCandidate {
    pub worker_id: WorkerId,
    pub node_id: NodeId,
    pub executable: bool,
    pub has_capability: bool,
    pub has_grant: bool,
    pub denied: bool,
    pub active_leases: u32,
    pub max_parallel: u32,
    pub artifact_access: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCandidate {
    pub node_id: NodeId,
    pub executable: bool,
    pub heartbeat_fresh: bool,
    pub active_leases: u32,
    pub max_parallel_leases: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerCandidate {
    pub ticket: TicketCandidate,
    pub worker: WorkerCandidate,
    pub node: NodeCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedCandidate {
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub node_id: NodeId,
    pub access_mode: String,
    pub score: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreOutcome {
    Selected,
    Idle,
    NoEligibleCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreDecision {
    pub outcome: ScoreOutcome,
    pub selected: Option<SelectedCandidate>,
    pub candidate_count: usize,
    pub reason_code: &'static str,
    pub explanation: JsonValue,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SchedulerScorer;

impl SchedulerScorer {
    pub fn score(&self, candidates: &[SchedulerCandidate]) -> Result<ScoreDecision, VoomError> {
        if candidates.is_empty() {
            return Ok(ScoreDecision {
                outcome: ScoreOutcome::Idle,
                selected: None,
                candidate_count: 0,
                reason_code: "no_ready_ticket",
                explanation: json!({
                    "scoring_version": SCORING_VERSION,
                    "operation": null,
                    "weights": weights_json(),
                    "candidates": []
                }),
            });
        }

        let mut best: Option<(usize, i64, String)> = None;
        let mut explanation_candidates = Vec::with_capacity(candidates.len());
        let operation = &candidates[0].ticket.operation;

        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.ticket.operation != *operation)
        {
            return Err(VoomError::Internal(format!(
                "scheduler candidate operation {:?} does not match {:?}",
                candidate.ticket.operation, operation
            )));
        }

        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.worker.node_id != candidate.node.node_id)
        {
            return Err(VoomError::Internal(format!(
                "scheduler candidate worker node {} does not match node {}",
                candidate.worker.node_id.0, candidate.node.node_id.0
            )));
        }

        for (index, candidate) in candidates.iter().enumerate() {
            let reasons = hard_gate_reasons(candidate);
            let access_mode = select_access_mode(&candidate.worker.artifact_access);
            let eligible = reasons.is_empty() && access_mode.is_some();
            let score = access_mode
                .filter(|_| eligible)
                .map_or(0, |mode| score_candidate(candidate, mode));

            if let (true, Some(mode)) = (eligible, access_mode) {
                match best {
                    None => best = Some((index, score, mode.to_owned())),
                    Some((best_index, best_score, _)) => {
                        if score > best_score
                            || (score == best_score
                                && tie_key(candidate) < tie_key(&candidates[best_index]))
                        {
                            best = Some((index, score, mode.to_owned()));
                        }
                    }
                }
            }

            explanation_candidates.push(json!({
                "ticket_id": candidate.ticket.ticket_id.0,
                "operation": candidate.ticket.operation,
                "worker_id": candidate.worker.worker_id.0,
                "node_id": candidate.node.node_id.0,
                "eligible": eligible,
                "score": score,
                "selected_access_mode": access_mode,
                "factors": factor_json(candidate, access_mode, score),
                "reasons": reasons,
            }));
        }

        let explanation = json!({
            "scoring_version": SCORING_VERSION,
            "operation": operation,
            "weights": weights_json(),
            "candidates": explanation_candidates,
        });

        if let Some((index, score, access_mode)) = best {
            let candidate = &candidates[index];
            return Ok(ScoreDecision {
                outcome: ScoreOutcome::Selected,
                selected: Some(SelectedCandidate {
                    ticket_id: candidate.ticket.ticket_id,
                    worker_id: candidate.worker.worker_id,
                    node_id: candidate.node.node_id,
                    access_mode,
                    score,
                }),
                candidate_count: candidates.len(),
                reason_code: "selected",
                explanation,
            });
        }

        Ok(ScoreDecision {
            outcome: ScoreOutcome::NoEligibleCandidate,
            selected: None,
            candidate_count: candidates.len(),
            reason_code: first_rejection_reason(&explanation_candidates),
            explanation,
        })
    }
}

fn weights_json() -> JsonValue {
    json!({
        "capability": 1000,
        "health": 500,
        "artifact_access": 100,
        "worker_capacity": 50,
        "node_capacity": 20,
        "locality": 20,
        "cost": 20,
        "tie_breaker": 1
    })
}

fn hard_gate_reasons(candidate: &SchedulerCandidate) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if !candidate.worker.has_capability {
        reasons.push("missing_capability");
    }
    if !candidate.worker.has_grant {
        reasons.push("missing_grant");
    }
    if candidate.worker.denied {
        reasons.push("operation_denied");
    }
    if !candidate.worker.executable {
        reasons.push("worker_not_executable");
    }
    if !candidate.node.executable {
        reasons.push("node_not_executable");
    }
    if !candidate.node.heartbeat_fresh {
        reasons.push("heartbeat_expired");
    }
    if select_access_mode(&candidate.worker.artifact_access).is_none() {
        reasons.push("unsupported_artifact_access");
    }
    if candidate.worker.active_leases >= candidate.worker.max_parallel {
        reasons.push("worker_capacity_full");
    }
    if candidate.node.active_leases >= candidate.node.max_parallel_leases {
        reasons.push("node_capacity_full");
    }
    reasons
}

fn select_access_mode(modes: &[String]) -> Option<&'static str> {
    if modes.iter().any(|mode| mode == "shared_mount") {
        Some("shared_mount")
    } else if modes.iter().any(|mode| mode == "control_plane_placeholder") {
        Some("control_plane_placeholder")
    } else if modes.iter().any(|mode| mode == "staged_output_placeholder") {
        Some("staged_output_placeholder")
    } else {
        None
    }
}

fn score_candidate(candidate: &SchedulerCandidate, access_mode: &str) -> i64 {
    let artifact_access_score = match access_mode {
        "shared_mount" => 100,
        "control_plane_placeholder" => 50,
        "staged_output_placeholder" => 25,
        _ => 0,
    };
    let worker_remaining = i64::from(
        candidate
            .worker
            .max_parallel
            .saturating_sub(candidate.worker.active_leases),
    );
    let node_remaining = i64::from(
        candidate
            .node
            .max_parallel_leases
            .saturating_sub(candidate.node.active_leases),
    );

    1000 + 500 + artifact_access_score + (worker_remaining * 50) + (node_remaining * 20)
}

fn factor_json(candidate: &SchedulerCandidate, access_mode: Option<&str>, score: i64) -> JsonValue {
    json!({
        "capability": if candidate.worker.has_capability && candidate.worker.has_grant && !candidate.worker.denied { 1000 } else { 0 },
        "health": if candidate.node.executable && candidate.node.heartbeat_fresh { 500 } else { 0 },
        "worker_capacity": i64::from(candidate
            .worker
            .max_parallel
            .saturating_sub(candidate.worker.active_leases)) * 50,
        "node_capacity": i64::from(candidate
            .node
            .max_parallel_leases
            .saturating_sub(candidate.node.active_leases)) * 20,
        "artifact_access": access_mode.map_or(0, |mode| match mode {
            "shared_mount" => 100,
            "control_plane_placeholder" => 50,
            "staged_output_placeholder" => 25,
            _ => 0,
        }),
        "tie_breaker": 0,
        "total": score
    })
}

fn tie_key(candidate: &SchedulerCandidate) -> (std::cmp::Reverse<i64>, i64, u64, u64, u64) {
    (
        std::cmp::Reverse(candidate.ticket.priority),
        candidate.ticket.next_eligible_at_epoch_seconds,
        candidate.node.node_id.0,
        candidate.worker.worker_id.0,
        candidate.ticket.ticket_id.0,
    )
}

fn first_rejection_reason(rows: &[JsonValue]) -> &'static str {
    rows.iter()
        .filter_map(|row| row["reasons"].as_array())
        .flat_map(|reasons| reasons.iter())
        .filter_map(serde_json::Value::as_str)
        .filter_map(reason_priority)
        .min_by_key(|(priority, _)| *priority)
        .map_or("no_eligible_candidate", |(_, reason)| reason)
}

fn reason_priority(reason: &str) -> Option<(u8, &'static str)> {
    static_reason_code(reason).map(|static_reason| {
        let priority = match static_reason {
            "missing_capability" => 0,
            "missing_grant" => 1,
            "operation_denied" => 2,
            "worker_not_executable" => 3,
            "node_not_executable" => 4,
            "heartbeat_expired" => 5,
            "unsupported_artifact_access" => 6,
            "worker_capacity_full" => 7,
            "node_capacity_full" => 8,
            _ => 9,
        };
        (priority, static_reason)
    })
}

fn static_reason_code(reason: &str) -> Option<&'static str> {
    match reason {
        "missing_capability" => Some("missing_capability"),
        "missing_grant" => Some("missing_grant"),
        "operation_denied" => Some("operation_denied"),
        "worker_not_executable" => Some("worker_not_executable"),
        "node_not_executable" => Some("node_not_executable"),
        "heartbeat_expired" => Some("heartbeat_expired"),
        "unsupported_artifact_access" => Some("unsupported_artifact_access"),
        "worker_capacity_full" => Some("worker_capacity_full"),
        "node_capacity_full" => Some("node_capacity_full"),
        _ => None,
    }
}

/// Lightweight worker projection the supervisor passes to
/// `WorkerSelector::select`. Pulls the few fields the selector
/// needs without leaking the full `voom-store` worker row type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerView {
    pub worker_id: WorkerId,
    pub supports: Vec<OperationKind>,
    pub active_leases: u32,
    pub max_parallel: u32,
}

pub trait WorkerSelector: Send + Sync + std::fmt::Debug {
    /// Select exactly one eligible worker for `operation` from
    /// `candidates`. Errors:
    /// - `NoEligibleWorker` if zero candidates advertise the
    ///   operation (or all are at capacity).
    /// - `AmbiguousWorkerSelection` if more than one candidate
    ///   advertises the operation and no explicit override is set.
    fn select(
        &self,
        operation: OperationKind,
        candidates: &[WorkerView],
    ) -> Result<WorkerId, VoomError>;
}

/// Default Sprint 2 selector: requires exactly one candidate
/// advertising the requested operation with capacity to spare.
#[derive(Debug, Default, Clone, Copy)]
pub struct SingleWorkerPerKindSelector;

impl WorkerSelector for SingleWorkerPerKindSelector {
    fn select(
        &self,
        operation: OperationKind,
        candidates: &[WorkerView],
    ) -> Result<WorkerId, VoomError> {
        let eligible: Vec<&WorkerView> = candidates
            .iter()
            .filter(|w| w.supports.contains(&operation) && w.active_leases < w.max_parallel)
            .collect();
        match eligible.len() {
            0 => Err(VoomError::NoEligibleWorker(format!(
                "no worker advertises {operation:?} with spare capacity"
            ))),
            1 => Ok(eligible[0].worker_id),
            n => {
                let _ = FailureClass::AmbiguousWorkerSelection;
                Err(VoomError::AmbiguousWorkerSelection(format!(
                    "{n} workers advertise {operation:?}; explicit override required"
                )))
            }
        }
    }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
