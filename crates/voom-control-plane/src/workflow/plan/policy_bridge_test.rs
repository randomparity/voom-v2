use serde_json::json;
use voom_core::OperationKind;
use voom_plan::{
    ArtifactExpectations, CapabilityHints, DependencyKind, Edge, ExecutionPlan, InputIdentity,
    NodeStatus, PlanNode, PlanOperationKind, PlanProvenance, PlanSummary, PolicyIdentity,
    ResourceEstimates, SafetyHints, SchedulingHints,
};
use voom_policy::TargetRef;

use super::*;

#[test]
fn bridge_maps_planned_remux_with_policy_target_and_payload() {
    let plan = plan(vec![node(PlanOperationKind::Remux, NodeStatus::Planned)]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let execution = single_file_workflow_plan_from_compliance(&plan, &report).unwrap();
    let workflow = execution.workflow.unwrap();

    assert_eq!(workflow.id, format!("policy-{}", report.report_id));
    assert_eq!(workflow.nodes.len(), 1);
    assert_eq!(workflow.nodes[0].id(), "policy-node_node_remux_Planned");
    assert_eq!(workflow.nodes[0].operation(), OperationKind::Remux);
    assert_eq!(
        workflow.nodes[0].policy_target(),
        Some(&TargetRef::FileVersion {
            id: voom_core::FileVersionId(42)
        })
    );
    assert_eq!(workflow.nodes[0].operation_payload()["type"], "remux");
    assert_eq!(workflow.nodes[0].operation_payload()["container"], "mkv");
    assert_eq!(execution.summary.submitted_node_count, 1);
    assert_eq!(
        execution.summary.per_operation[&PlanOperationKind::Remux],
        1
    );
}

#[test]
fn bridge_builds_workflow_with_requested_limits() {
    let plan = plan(vec![node(PlanOperationKind::Remux, NodeStatus::Planned)]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let shape = WorkflowExecutionShape::new(4, 2).unwrap();
    let execution = workflow_plan_from_compliance(&plan, &report, shape).unwrap();
    let workflow = execution.workflow.unwrap();

    assert_eq!(workflow.fan_out.max_files, 4);
    assert_eq!(workflow.concurrency.max_in_flight_dispatches, 2);
}

#[test]
fn bridge_rejects_zero_execution_shape_limits() {
    let max_files_err = WorkflowExecutionShape::new(0, 2).unwrap_err();
    assert_eq!(
        max_files_err.to_string(),
        "policy execution error: workflow execution shape max_files must be greater than 0"
    );

    let max_in_flight_err = WorkflowExecutionShape::new(2, 0).unwrap_err();
    assert_eq!(
        max_in_flight_err.to_string(),
        concat!(
            "policy execution error: ",
            "workflow execution shape max_in_flight_dispatches must be greater than 0"
        )
    );
}

#[test]
fn bridge_rejects_legacy_planned_set_container() {
    let plan = plan(vec![node(
        PlanOperationKind::SetContainer,
        NodeStatus::Planned,
    )]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let err = single_file_workflow_plan_from_compliance(&plan, &report).unwrap_err();

    assert_eq!(err.code(), "POLICY_EXECUTION_ERROR");
    assert_eq!(
        err.to_string(),
        "policy execution error: unsupported execution operation set_container"
    );
}

#[test]
fn bridge_counts_non_planned_remux_nodes_without_submission() {
    let plan = plan(vec![
        node(PlanOperationKind::Remux, NodeStatus::NoOp),
        node(PlanOperationKind::Remux, NodeStatus::Blocked),
    ]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let execution = single_file_workflow_plan_from_compliance(&plan, &report).unwrap();

    assert!(execution.workflow.is_none());
    assert_eq!(execution.summary.submitted_node_count, 0);
    assert_eq!(execution.summary.skipped_no_op_count, 1);
    assert_eq!(execution.summary.blocked_count, 1);
}

#[test]
fn bridge_returns_empty_summary_without_job_for_no_executable_nodes() {
    let plan = plan(vec![node(
        PlanOperationKind::SetContainer,
        NodeStatus::NoOp,
    )]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let execution = single_file_workflow_plan_from_compliance(&plan, &report).unwrap();

    assert!(execution.workflow.is_none());
    assert_eq!(execution.summary.plan_id, plan.plan_id);
    assert_eq!(execution.summary.report_id, report.report_id);
    assert_eq!(execution.summary.job_id, None);
    assert_eq!(execution.summary.submitted_node_count, 0);
    assert_eq!(execution.summary.skipped_no_op_count, 1);
}

#[test]
fn bridge_rejects_planned_unsupported_operation_before_job_creation() {
    let plan = plan(vec![node(
        PlanOperationKind::KeepTracks,
        NodeStatus::Planned,
    )]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let err = single_file_workflow_plan_from_compliance(&plan, &report).unwrap_err();

    assert_eq!(err.code(), "POLICY_EXECUTION_ERROR");
    assert_eq!(
        err.to_string(),
        "policy execution error: unsupported execution operation keep_tracks"
    );
}

#[test]
fn bridge_maps_planned_transcode_video() {
    let plan = plan(vec![node(
        PlanOperationKind::TranscodeVideo,
        NodeStatus::Planned,
    )]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let bridged = single_file_workflow_plan_from_compliance(&plan, &report).unwrap();
    let workflow = bridged.workflow.unwrap();

    assert_eq!(workflow.nodes[0].operation(), OperationKind::TranscodeVideo);
    assert_eq!(
        bridged.summary.per_operation[&PlanOperationKind::TranscodeVideo],
        1
    );
    assert_eq!(
        workflow.nodes[0].policy_target(),
        Some(&TargetRef::FileVersion {
            id: voom_core::FileVersionId(42)
        })
    );
    assert_eq!(
        workflow.nodes[0].operation_payload()["target_codec"],
        "hevc"
    );
}

#[test]
fn bridge_maps_planned_transcode_audio() {
    let plan = plan(vec![node(
        PlanOperationKind::TranscodeAudio,
        NodeStatus::Planned,
    )]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let bridged = single_file_workflow_plan_from_compliance(&plan, &report).unwrap();
    let workflow = bridged.workflow.unwrap();

    assert_eq!(workflow.nodes[0].operation(), OperationKind::TranscodeAudio);
    assert_eq!(
        bridged.summary.per_operation[&PlanOperationKind::TranscodeAudio],
        1
    );
    assert_eq!(
        workflow.nodes[0].operation_payload()["type"],
        "transcode_audio"
    );
}

#[test]
fn bridge_maps_planned_extract_audio() {
    let plan = plan(vec![node(
        PlanOperationKind::ExtractAudio,
        NodeStatus::Planned,
    )]);
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let bridged = single_file_workflow_plan_from_compliance(&plan, &report).unwrap();
    let workflow = bridged.workflow.unwrap();

    assert_eq!(workflow.nodes[0].operation(), OperationKind::ExtractAudio);
    assert_eq!(
        bridged.summary.per_operation[&PlanOperationKind::ExtractAudio],
        1
    );
    assert_eq!(
        workflow.nodes[0].operation_payload()["type"],
        "extract_audio"
    );
}

#[test]
fn bridge_preserves_plan_edges_between_included_planned_nodes() {
    let first = node_with_id(
        "node_remux_first",
        "normalize",
        PlanOperationKind::Remux,
        NodeStatus::Planned,
    );
    let second = node_with_id(
        "node_remux_second",
        "tracks",
        PlanOperationKind::Remux,
        NodeStatus::Planned,
    );
    let plan = plan_with_edges(
        vec![first, second],
        vec![Edge {
            edge_id: "edge_first_second".to_owned(),
            from_node_id: "node_remux_first".to_owned(),
            to_node_id: "node_remux_second".to_owned(),
            dependency_kind: DependencyKind::PhaseDependsOn,
        }],
    );
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let bridged = single_file_workflow_plan_from_compliance(&plan, &report).unwrap();
    let workflow = bridged.workflow.unwrap();

    assert_eq!(workflow.nodes[0].id(), "policy-node_node_remux_first");
    assert_eq!(workflow.nodes[1].id(), "policy-node_node_remux_second");
    assert_eq!(
        workflow.nodes[1].depends_on(),
        ["policy-node_node_remux_first".to_owned()]
    );
}

#[test]
fn bridge_omits_dependencies_to_skipped_nodes() {
    let skipped = node_with_id(
        "node_remux_noop",
        "normalize",
        PlanOperationKind::Remux,
        NodeStatus::NoOp,
    );
    let planned = node_with_id(
        "node_remux_planned",
        "tracks",
        PlanOperationKind::Remux,
        NodeStatus::Planned,
    );
    let plan = plan_with_edges(
        vec![skipped, planned],
        vec![Edge {
            edge_id: "edge_skipped_planned".to_owned(),
            from_node_id: "node_remux_noop".to_owned(),
            to_node_id: "node_remux_planned".to_owned(),
            dependency_kind: DependencyKind::PhaseDependsOn,
        }],
    );
    let report = voom_plan::generate_compliance_report(&plan).unwrap();

    let bridged = single_file_workflow_plan_from_compliance(&plan, &report).unwrap();
    let workflow = bridged.workflow.unwrap();

    assert_eq!(workflow.nodes.len(), 1);
    assert!(workflow.nodes[0].depends_on().is_empty());
}

fn plan(nodes: Vec<PlanNode>) -> ExecutionPlan {
    plan_with_edges(nodes, Vec::new())
}

fn plan_with_edges(nodes: Vec<PlanNode>, edges: Vec<Edge>) -> ExecutionPlan {
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
        edges,
        warnings: Vec::new(),
        diagnostics: Vec::new(),
        provenance: PlanProvenance::default(),
    }
}

fn node(operation_kind: PlanOperationKind, status: NodeStatus) -> PlanNode {
    node_with_id(
        &format!("node_{operation_kind}_{status:?}"),
        "normalize",
        operation_kind,
        status,
    )
}

fn node_with_id(
    node_id: &str,
    phase_name: &str,
    operation_kind: PlanOperationKind,
    status: NodeStatus,
) -> PlanNode {
    PlanNode {
        node_id: node_id.to_owned(),
        phase_name: phase_name.to_owned(),
        ordinal: 0,
        target: TargetRef::FileVersion {
            id: voom_core::FileVersionId(42),
        },
        operation_kind,
        operation_payload: operation_payload(operation_kind),
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

fn operation_payload(operation_kind: PlanOperationKind) -> serde_json::Value {
    match operation_kind {
        PlanOperationKind::Remux => json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": []
        }),
        PlanOperationKind::TranscodeVideo => json!({
            "type": "transcode_video",
            "target_codec": "hevc",
            "container": "mkv",
            "profile": "default-hevc"
        }),
        PlanOperationKind::TranscodeAudio => json!({
            "type": "transcode_audio",
            "target_codec": "opus",
            "container": "mkv",
            "source_media_snapshot_id": 99
        }),
        PlanOperationKind::ExtractAudio => json!({
            "type": "extract_audio",
            "target_codec": "opus",
            "container": "ogg",
            "source_media_snapshot_id": 99
        }),
        _ => json!({"container": "mkv"}),
    }
}
