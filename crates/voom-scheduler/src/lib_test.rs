use super::*;
use serde_json::json;
use voom_core::{NodeId, TicketId, WorkerId};
use voom_worker_protocol::OperationKind;

fn scored_candidate(
    ticket_id: u64,
    worker_id: u64,
    node_id: u64,
    operation: &str,
) -> SchedulerCandidate {
    SchedulerCandidate {
        ticket: TicketCandidate {
            ticket_id: TicketId(ticket_id),
            operation: operation.to_owned(),
            priority: 0,
            next_eligible_at_epoch_seconds: 0,
            payload: json!({
                "artifact_access": {
                    "inputs": ["handle:input:test"],
                    "outputs": ["handle:output:test"]
                }
            }),
        },
        worker: WorkerCandidate {
            worker_id: WorkerId(worker_id),
            node_id: NodeId(node_id),
            executable: true,
            has_capability: true,
            has_grant: true,
            denied: false,
            active_leases: 0,
            max_parallel: 2,
            artifact_access: vec!["shared_mount".to_owned()],
        },
        node: NodeCandidate {
            node_id: NodeId(node_id),
            executable: true,
            heartbeat_fresh: true,
            active_leases: 0,
            max_parallel_leases: 2,
        },
    }
}

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

#[test]
fn scorer_selects_eligible_candidate_with_explanation() {
    let scorer = SchedulerScorer::default();
    let out = scorer
        .score(&[scored_candidate(7, 11, 13, "probe_file")])
        .unwrap();

    assert_eq!(out.outcome, ScoreOutcome::Selected);
    assert_eq!(out.selected.as_ref().unwrap().ticket_id, TicketId(7));
    assert_eq!(out.selected.as_ref().unwrap().worker_id, WorkerId(11));
    assert_eq!(out.selected.as_ref().unwrap().node_id, NodeId(13));
    assert_eq!(out.selected.as_ref().unwrap().access_mode, "shared_mount");
    assert_eq!(out.explanation["scoring_version"], 1);
    assert_eq!(out.explanation["operation"], "probe_file");
    assert_eq!(out.explanation["candidates"][0]["eligible"], true);
    assert!(out.explanation["candidates"][0]["score"].as_i64().unwrap() > 0);
}

#[test]
fn scorer_rejects_hard_gate_failures_with_reason_codes() {
    let scorer = SchedulerScorer::default();
    let mut missing_grant = scored_candidate(1, 2, 3, "probe_file");
    missing_grant.worker.has_grant = false;
    let mut full_node = scored_candidate(4, 5, 6, "probe_file");
    full_node.node.active_leases = 2;
    full_node.node.max_parallel_leases = 2;

    let out = scorer.score(&[missing_grant, full_node]).unwrap();

    assert_eq!(out.outcome, ScoreOutcome::NoEligibleCandidate);
    let reasons0 = out.explanation["candidates"][0]["reasons"]
        .as_array()
        .unwrap();
    let reasons1 = out.explanation["candidates"][1]["reasons"]
        .as_array()
        .unwrap();
    assert!(reasons0.iter().any(|reason| reason == "missing_grant"));
    assert!(reasons1.iter().any(|reason| reason == "node_capacity_full"));
}

#[test]
fn scorer_uses_deterministic_tie_breakers() {
    let scorer = SchedulerScorer::default();
    let mut lower_priority = scored_candidate(1, 2, 3, "probe_file");
    lower_priority.ticket.priority = 9;
    let mut higher_priority = scored_candidate(4, 5, 6, "probe_file");
    higher_priority.ticket.priority = 10;
    let out = scorer.score(&[lower_priority, higher_priority]).unwrap();
    assert_eq!(out.selected.as_ref().unwrap().ticket_id, TicketId(4));

    let mut later = scored_candidate(7, 8, 9, "probe_file");
    later.ticket.next_eligible_at_epoch_seconds = 20;
    let mut earlier = scored_candidate(10, 11, 12, "probe_file");
    earlier.ticket.next_eligible_at_epoch_seconds = 10;
    let out = scorer.score(&[later, earlier]).unwrap();
    assert_eq!(out.selected.as_ref().unwrap().ticket_id, TicketId(10));
}

#[test]
fn scorer_reports_stable_no_eligible_reason_code() {
    let scorer = SchedulerScorer::default();
    let mut full_node = scored_candidate(1, 2, 3, "probe_file");
    full_node.node.active_leases = 2;
    full_node.node.max_parallel_leases = 2;
    let mut missing_grant = scored_candidate(4, 5, 6, "probe_file");
    missing_grant.worker.has_grant = false;

    let out = scorer.score(&[full_node, missing_grant]).unwrap();

    assert_eq!(out.selected, None);
    assert_eq!(out.reason_code, "missing_grant");
}

#[test]
fn scorer_rejects_incoherent_candidate_shapes() {
    let scorer = SchedulerScorer::default();
    let mut operation_mismatch = scored_candidate(1, 2, 3, "probe_file");
    operation_mismatch.ticket.operation = "hash_file".to_owned();
    let err = scorer
        .score(&[scored_candidate(4, 5, 6, "probe_file"), operation_mismatch])
        .unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::Internal);

    let mut node_mismatch = scored_candidate(7, 8, 9, "probe_file");
    node_mismatch.worker.node_id = NodeId(10);
    let err = scorer.score(&[node_mismatch]).unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::Internal);
}
