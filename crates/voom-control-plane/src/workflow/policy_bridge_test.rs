use serde_json::json;
use voom_plan::{
    ArtifactExpectations, CapabilityHints, ExecutionPlan, InputIdentity, NodeStatus, PlanNode,
    PlanProvenance, PlanSummary, PolicyIdentity, ResourceEstimates, SafetyHints, SchedulingHints,
};
use voom_worker_protocol::OperationKind;

use super::*;

#[test]
fn bridge_maps_only_planned_set_container_to_remux() {
    let plan = plan(vec![
        node("set_container", NodeStatus::Planned),
        node("set_container", NodeStatus::NoOp),
        node("set_container", NodeStatus::Blocked),
    ]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let execution = workflow_plan_from_compliance(&plan, &report).unwrap();
    let workflow = execution.workflow.unwrap();

    assert_eq!(workflow.id, format!("policy-{}", report.report_id));
    assert_eq!(workflow.nodes.len(), 1);
    assert_eq!(
        workflow.nodes[0].id(),
        "policy-node_node_set_container_Planned"
    );
    assert_eq!(workflow.nodes[0].operation(), OperationKind::Remux);
    assert_eq!(execution.summary.submitted_node_count, 1);
    assert_eq!(execution.summary.skipped_no_op_count, 1);
    assert_eq!(execution.summary.blocked_count, 1);
}

#[test]
fn bridge_returns_empty_summary_without_job_for_no_executable_nodes() {
    let plan = plan(vec![node("set_container", NodeStatus::NoOp)]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let execution = workflow_plan_from_compliance(&plan, &report).unwrap();

    assert!(execution.workflow.is_none());
    assert_eq!(execution.summary.plan_id, plan.plan_id);
    assert_eq!(execution.summary.report_id, report.report_id);
    assert_eq!(execution.summary.job_id, None);
    assert_eq!(execution.summary.submitted_node_count, 0);
    assert_eq!(execution.summary.skipped_no_op_count, 1);
}

#[test]
fn bridge_rejects_planned_unsupported_operation_before_job_creation() {
    let plan = plan(vec![node("unsupported_operation", NodeStatus::Planned)]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let err = workflow_plan_from_compliance(&plan, &report).unwrap_err();

    assert_eq!(err.code(), "POLICY_EXECUTION_ERROR");
    assert_eq!(
        err.to_string(),
        "policy execution error: unsupported execution operation unsupported_operation"
    );
}

fn plan(nodes: Vec<PlanNode>) -> ExecutionPlan {
    ExecutionPlan {
        schema_version: 1,
        plan_id: "plan_test".to_owned(),
        plan_hash: "blake3:plan".to_owned(),
        policy: PolicyIdentity {
            slug: "container-metadata".to_owned(),
            source_hash: "abc".to_owned(),
            document_id: Some(voom_core::PolicyDocumentId(1)),
            version_id: Some(voom_core::PolicyVersionId(2)),
        },
        input: InputIdentity {
            slug: Some("synthetic".to_owned()),
            source_label: None,
            input_set_id: Some(voom_core::PolicyInputSetId(3)),
            fixture_labels: vec!["synthetic".to_owned()],
        },
        generated_at: None,
        summary: PlanSummary::default(),
        nodes,
        edges: Vec::new(),
        warnings: Vec::new(),
        diagnostics: Vec::new(),
        provenance: PlanProvenance::default(),
    }
}

fn node(operation_kind: &str, status: NodeStatus) -> PlanNode {
    PlanNode {
        node_id: format!("node_{operation_kind}_{status:?}"),
        phase_name: "normalize".to_owned(),
        ordinal: 0,
        target: voom_policy::TargetRef::Synthetic {
            key: "movie-a".to_owned(),
            kind: voom_policy::TargetKind::MediaWork,
        },
        operation_kind: operation_kind.to_owned(),
        operation_payload: json!({"container": "mkv"}),
        observed_state: Some(json!({"container": "mp4"})),
        status,
        status_reason: "container mp4 will be changed to mkv".to_owned(),
        capability_hints: CapabilityHints::default(),
        scheduling_hints: SchedulingHints::default(),
        resource_estimates: ResourceEstimates::default(),
        artifact_expectations: ArtifactExpectations::default(),
        safety_hints: SafetyHints::default(),
    }
}
