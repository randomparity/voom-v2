use super::model::{
    ConcurrencyPolicy, FanOutPolicy, OperationNode, TimingPolicy, WorkflowNode, WorkflowPlan,
};
use voom_worker_protocol::OperationKind;

#[test]
fn default_ci_plan_has_seed_two_three_files_and_parallel_dispatches() {
    let plan = WorkflowPlan::default_ci();
    assert_eq!(plan.id, "sprint-2-phase-7-default");
    assert_eq!(plan.seed, 2);
    assert_eq!(plan.fan_out.max_files, 3);
    assert!(plan.concurrency.max_in_flight_dispatches > 1);
    let backup = plan
        .nodes
        .iter()
        .find(|node| node.id() == "backup")
        .unwrap();
    assert_eq!(backup.depends_on(), &[] as &[String]);
    assert_eq!(backup.depends_on_selected(), &["transform".to_owned()]);
    plan.validate().unwrap();
}

#[test]
fn validation_rejects_duplicate_node_ids() {
    let plan = plan_with_nodes([
        node("scan", OperationKind::ScanLibrary, []),
        node("scan", OperationKind::ProbeFile, []),
    ]);

    let err = plan.validate().unwrap_err();
    assert!(err.to_string().contains("duplicate node id"));
}

#[test]
fn validation_rejects_missing_dependencies() {
    let plan = plan_with_nodes([node("probe", OperationKind::ProbeFile, ["scan"])]);

    let err = plan.validate().unwrap_err();
    assert!(err.to_string().contains("missing dependency"));
}

#[test]
fn validation_rejects_missing_selected_dependency_groups() {
    let plan = plan_with_nodes([
        node("scan", OperationKind::ScanLibrary, []),
        node_after_selected("backup", OperationKind::BackUpFile, ["transform"]),
    ]);

    let err = plan.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("missing selected dependency group")
    );
}

#[test]
fn validation_rejects_cycles() {
    let plan = plan_with_nodes([
        node("scan", OperationKind::ScanLibrary, ["quality"]),
        node("probe", OperationKind::ProbeFile, ["scan"]),
        node("quality", OperationKind::ScoreQuality, ["probe"]),
    ]);

    let err = plan.validate().unwrap_err();
    assert!(err.to_string().contains("cycle"));
}

#[test]
fn validation_rejects_invalid_fan_out() {
    let mut plan = valid_plan();
    plan.fan_out.max_files = 0;

    let err = plan.validate().unwrap_err();
    assert!(err.to_string().contains("fan_out.max_files"));
}

#[test]
fn validation_rejects_invalid_concurrency() {
    let mut plan = valid_plan();
    plan.concurrency.max_in_flight_dispatches = 0;

    let err = plan.validate().unwrap_err();
    assert!(err.to_string().contains("max_in_flight_dispatches"));
}

fn valid_plan() -> WorkflowPlan {
    plan_with_nodes([
        node("scan", OperationKind::ScanLibrary, []),
        node("probe", OperationKind::ProbeFile, ["scan"]),
    ])
}

fn plan_with_nodes<const N: usize>(nodes: [WorkflowNode; N]) -> WorkflowPlan {
    WorkflowPlan {
        id: "test-plan".to_owned(),
        seed: 1,
        nodes: nodes.into(),
        fan_out: FanOutPolicy { max_files: 3 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 2,
        },
        timing: TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 5,
        },
    }
}

fn node<const N: usize>(id: &str, operation: OperationKind, depends_on: [&str; N]) -> WorkflowNode {
    WorkflowNode::Operation(OperationNode {
        id: id.to_owned(),
        operation,
        depends_on: depends_on.into_iter().map(str::to_owned).collect(),
        depends_on_selected: Vec::new(),
        provides_selected: None,
    })
}

fn node_after_selected<const N: usize>(
    id: &str,
    operation: OperationKind,
    depends_on_selected: [&str; N],
) -> WorkflowNode {
    WorkflowNode::Operation(OperationNode {
        id: id.to_owned(),
        operation,
        depends_on: Vec::new(),
        depends_on_selected: depends_on_selected.into_iter().map(str::to_owned).collect(),
        provides_selected: None,
    })
}
