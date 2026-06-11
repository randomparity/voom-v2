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
use voom_core::{OperationKind, TicketOperation};

pub const SCORING_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketCandidate {
    pub ticket_id: TicketId,
    pub operation: TicketOperation,
    pub priority: i64,
    pub next_eligible_at_epoch_seconds: i64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreReasonCode {
    Selected,
    NoReadyTicket,
    MissingCapability,
    MissingGrant,
    OperationDenied,
    WorkerNotExecutable,
    NodeNotExecutable,
    HeartbeatExpired,
    UnsupportedArtifactAccess,
    WorkerCapacityFull,
    NodeCapacityFull,
    NoEligibleCandidate,
}

impl ScoreReasonCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::NoReadyTicket => "no_ready_ticket",
            Self::MissingCapability => "missing_capability",
            Self::MissingGrant => "missing_grant",
            Self::OperationDenied => "operation_denied",
            Self::WorkerNotExecutable => "worker_not_executable",
            Self::NodeNotExecutable => "node_not_executable",
            Self::HeartbeatExpired => "heartbeat_expired",
            Self::UnsupportedArtifactAccess => "unsupported_artifact_access",
            Self::WorkerCapacityFull => "worker_capacity_full",
            Self::NodeCapacityFull => "node_capacity_full",
            Self::NoEligibleCandidate => "no_eligible_candidate",
        }
    }

    /// Returns a numeric priority used to surface the **most fundamental** rejection
    /// reason when a candidate has multiple hard-gate failures.  Lower values are
    /// more fundamental; the scorer selects the variant with the lowest priority
    /// value via `min_by_key(ScoreReasonCode::priority)`.
    ///
    /// Tier ordering (ascending priority value = increasingly peripheral cause):
    ///
    /// | Priority | Variant | Meaning |
    /// |----------|---------|---------|
    /// | 0 | `MissingCapability` | Worker cannot perform the operation at all |
    /// | 1 | `MissingGrant` | Worker lacks the required permission grant |
    /// | 2 | `OperationDenied` | Worker is explicitly denied for this operation |
    /// | 3 | `WorkerNotExecutable` | Worker is offline or marked non-executable |
    /// | 4 | `NodeNotExecutable` | Host node is offline or marked non-executable |
    /// | 5 | `HeartbeatExpired` | Node heartbeat is stale |
    /// | 6 | `UnsupportedArtifactAccess` | No supported artifact-access mode |
    /// | 7 | `WorkerCapacityFull` | Worker is at its active-lease limit |
    /// | 8 | `NodeCapacityFull` | Node is at its active-lease limit |
    /// | 9 | `Selected` / `NoReadyTicket` / `NoEligibleCandidate` | Non-rejection or aggregate outcome |
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::MissingCapability => 0,
            Self::MissingGrant => 1,
            Self::OperationDenied => 2,
            Self::WorkerNotExecutable => 3,
            Self::NodeNotExecutable => 4,
            Self::HeartbeatExpired => 5,
            Self::UnsupportedArtifactAccess => 6,
            Self::WorkerCapacityFull => 7,
            Self::NodeCapacityFull => 8,
            Self::Selected | Self::NoReadyTicket | Self::NoEligibleCandidate => 9,
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "selected" => Some(Self::Selected),
            "no_ready_ticket" => Some(Self::NoReadyTicket),
            "missing_capability" => Some(Self::MissingCapability),
            "missing_grant" => Some(Self::MissingGrant),
            "operation_denied" => Some(Self::OperationDenied),
            "worker_not_executable" => Some(Self::WorkerNotExecutable),
            "node_not_executable" => Some(Self::NodeNotExecutable),
            "heartbeat_expired" => Some(Self::HeartbeatExpired),
            "unsupported_artifact_access" => Some(Self::UnsupportedArtifactAccess),
            "worker_capacity_full" => Some(Self::WorkerCapacityFull),
            "node_capacity_full" => Some(Self::NodeCapacityFull),
            "no_eligible_candidate" => Some(Self::NoEligibleCandidate),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreDecision {
    pub outcome: ScoreOutcome,
    pub selected: Option<SelectedCandidate>,
    pub candidate_count: usize,
    pub reason_code: ScoreReasonCode,
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
                reason_code: ScoreReasonCode::NoReadyTicket,
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
            let access_mode = select_access_mode(&candidate.worker.artifact_access);
            let reasons = hard_gate_reasons(candidate, access_mode);
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
                "operation": candidate.ticket.operation.as_str(),
                "worker_id": candidate.worker.worker_id.0,
                "node_id": candidate.node.node_id.0,
                "eligible": eligible,
                "score": score,
                "selected_access_mode": access_mode,
                "factors": factor_json(candidate, access_mode, score),
                "reasons": reasons.iter().map(|reason| reason.as_str()).collect::<Vec<_>>(),
            }));
        }

        let explanation = json!({
            "scoring_version": SCORING_VERSION,
            "operation": operation.as_str(),
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
                reason_code: ScoreReasonCode::Selected,
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

fn hard_gate_reasons(
    candidate: &SchedulerCandidate,
    access_mode: Option<&str>,
) -> Vec<ScoreReasonCode> {
    let mut reasons = Vec::new();
    if !candidate.worker.has_capability {
        reasons.push(ScoreReasonCode::MissingCapability);
    }
    if !candidate.worker.has_grant {
        reasons.push(ScoreReasonCode::MissingGrant);
    }
    if candidate.worker.denied {
        reasons.push(ScoreReasonCode::OperationDenied);
    }
    if !candidate.worker.executable {
        reasons.push(ScoreReasonCode::WorkerNotExecutable);
    }
    if !candidate.node.executable {
        reasons.push(ScoreReasonCode::NodeNotExecutable);
    }
    if !candidate.node.heartbeat_fresh {
        reasons.push(ScoreReasonCode::HeartbeatExpired);
    }
    if access_mode.is_none() {
        reasons.push(ScoreReasonCode::UnsupportedArtifactAccess);
    }
    if candidate.worker.active_leases >= candidate.worker.max_parallel {
        reasons.push(ScoreReasonCode::WorkerCapacityFull);
    }
    if candidate.node.active_leases >= candidate.node.max_parallel_leases {
        reasons.push(ScoreReasonCode::NodeCapacityFull);
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

fn first_rejection_reason(rows: &[JsonValue]) -> ScoreReasonCode {
    rows.iter()
        .filter_map(|row| row["reasons"].as_array())
        .flat_map(|reasons| reasons.iter())
        .filter_map(serde_json::Value::as_str)
        .filter_map(ScoreReasonCode::parse)
        .min_by_key(|reason| reason.priority())
        .unwrap_or(ScoreReasonCode::NoEligibleCandidate)
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
