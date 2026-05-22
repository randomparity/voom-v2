#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Sprint 2 Phase 2: minimal `WorkerSelector` trait + a
//! single-worker-per-operation default implementation. Sprint 4
//! swaps in multi-worker scoring (capability + locality + cost)
//! behind the same trait without changing supervisor or test code.

use voom_core::{FailureClass, VoomError, WorkerId};
use voom_worker_protocol::OperationKind;

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
