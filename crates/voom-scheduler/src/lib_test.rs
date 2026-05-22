use super::*;
use voom_core::WorkerId;
use voom_worker_protocol::OperationKind;

fn view(id: u64, supports: &[OperationKind]) -> WorkerView {
    WorkerView {
        worker_id: WorkerId(id),
        supports: supports.to_vec(),
        active_leases: 0,
        max_parallel: 4,
    }
}

#[test]
fn single_eligible_worker_succeeds() {
    let s = SingleWorkerPerKindSelector;
    let workers = [view(1, &[OperationKind::ProbeFile])];
    let pick = s.select(OperationKind::ProbeFile, &workers).unwrap();
    assert_eq!(pick, WorkerId(1));
}

#[test]
fn zero_eligible_rejects_no_eligible_worker() {
    let s = SingleWorkerPerKindSelector;
    let workers = [view(1, &[OperationKind::HashFile])];
    let err = s.select(OperationKind::ProbeFile, &workers).unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::NoEligibleWorker);
}

#[test]
fn two_eligible_rejects_ambiguous() {
    let s = SingleWorkerPerKindSelector;
    let workers = [
        view(1, &[OperationKind::ProbeFile]),
        view(2, &[OperationKind::ProbeFile]),
    ];
    let err = s.select(OperationKind::ProbeFile, &workers).unwrap_err();
    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::AmbiguousWorkerSelection
    );
}

#[test]
fn at_capacity_filtered_out() {
    let s = SingleWorkerPerKindSelector;
    let workers = [WorkerView {
        worker_id: WorkerId(1),
        supports: vec![OperationKind::ProbeFile],
        active_leases: 4,
        max_parallel: 4,
    }];
    let err = s.select(OperationKind::ProbeFile, &workers).unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::NoEligibleWorker);
}
