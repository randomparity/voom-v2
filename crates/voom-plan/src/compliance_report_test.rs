use serde_json::json;

use crate::{
    ArtifactExpectations, CapabilityHints, ExecutionPlan, InputIdentity, NodeStatus, PlanNode,
    PlanProvenance, PlanSummary, PolicyIdentity, ResourceEstimates, SafetyHints, SchedulingHints,
};

use super::*;

fn target() -> crate::TargetRef {
    voom_policy::TargetRef::Synthetic {
        key: "movie-a".to_owned(),
        kind: voom_policy::TargetKind::MediaWork,
    }
}

fn node(
    status: NodeStatus,
    operation_kind: &str,
    observed_state: Option<serde_json::Value>,
) -> PlanNode {
    PlanNode {
        node_id: format!("node_{operation_kind}_{status:?}"),
        phase_name: "normalize".to_owned(),
        ordinal: 0,
        target: target(),
        operation_kind: operation_kind.to_owned(),
        operation_payload: json!({"container": "mkv"}),
        observed_state,
        status,
        status_reason: "container mp4 will be changed to mkv".to_owned(),
        capability_hints: CapabilityHints::default(),
        scheduling_hints: SchedulingHints::default(),
        resource_estimates: ResourceEstimates::default(),
        artifact_expectations: ArtifactExpectations::default(),
        safety_hints: SafetyHints::default(),
    }
}

fn transcode_node(status: NodeStatus) -> PlanNode {
    PlanNode {
        operation_payload: json!({
            "type": "transcode_video",
            "target_codec": "hevc",
            "container": "mkv",
            "profile": "default-hevc"
        }),
        observed_state: Some(json!({
            "container": "mp4",
            "video_codec": "h264",
            "video_stream_count": 1
        })),
        ..node(status, "transcode_video", None)
    }
}

fn remux_node(status: NodeStatus) -> PlanNode {
    PlanNode {
        operation_payload: json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": []
        }),
        observed_state: Some(json!({"container": "mp4"})),
        ..node(status, "remux", None)
    }
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

#[test]
fn no_op_node_maps_to_compliant_check_and_report() {
    let report = generate_compliance_report(&plan(vec![node(
        NodeStatus::NoOp,
        "set_container",
        Some(json!({"container": "mkv"})),
    )]))
    .unwrap();

    assert_eq!(report.summary.status, crate::ReportStatus::Compliant);
    assert_eq!(report.checks[0].check_status, crate::CheckStatus::Compliant);
    assert_eq!(
        report.checks[0].issue_action_hint,
        crate::IssueActionHint::ResolveMatching
    );
    assert_eq!(
        report.checks[0].execution_eligibility,
        crate::ExecutionEligibility::NoOp
    );
}

#[test]
fn planned_node_maps_to_noncompliant_supported_check() {
    let report = generate_compliance_report(&plan(vec![node(
        NodeStatus::Planned,
        "set_container",
        Some(json!({"container": "mp4"})),
    )]))
    .unwrap();

    assert_eq!(report.summary.status, crate::ReportStatus::Noncompliant);
    assert_eq!(
        report.checks[0].check_status,
        crate::CheckStatus::Noncompliant
    );
    assert_eq!(report.checks[0].compliance_kind, "container");
    assert_eq!(
        report.checks[0].execution_eligibility,
        crate::ExecutionEligibility::Supported
    );
    assert_eq!(
        report.checks[0].issue_action_hint,
        crate::IssueActionHint::CreateOrUpdatePlanned
    );
    assert_eq!(report.checks[0].desired_state, json!({"container": "mkv"}));
    assert_eq!(
        report.checks[0].observed_state,
        Some(json!({"container": "mp4"}))
    );
}

#[test]
fn blocked_node_maps_to_blocked_check() {
    let report = generate_compliance_report(&plan(vec![node(
        NodeStatus::Blocked,
        "set_container",
        None,
    )]))
    .unwrap();

    assert_eq!(report.summary.status, crate::ReportStatus::Blocked);
    assert_eq!(report.checks[0].check_status, crate::CheckStatus::Blocked);
    assert_eq!(
        report.checks[0].issue_action_hint,
        crate::IssueActionHint::CreateOrUpdateOpen
    );
    assert_eq!(
        report.checks[0].execution_eligibility,
        crate::ExecutionEligibility::Blocked
    );
}

#[test]
fn transcode_video_planned_node_maps_to_supported_check() {
    let report =
        generate_compliance_report(&plan(vec![transcode_node(NodeStatus::Planned)])).unwrap();

    assert_eq!(report.summary.status, crate::ReportStatus::Noncompliant);
    assert_eq!(report.checks[0].compliance_kind, "transcode_video");
    assert_eq!(
        report.checks[0].execution_eligibility,
        crate::ExecutionEligibility::Supported
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn transcode_video_blocked_node_maps_to_blocked_check() {
    let report =
        generate_compliance_report(&plan(vec![transcode_node(NodeStatus::Blocked)])).unwrap();

    assert_eq!(report.summary.status, crate::ReportStatus::Blocked);
    assert_eq!(
        report.checks[0].execution_eligibility,
        crate::ExecutionEligibility::Blocked
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn remux_planned_node_maps_to_supported_check() {
    let report = generate_compliance_report(&plan(vec![remux_node(NodeStatus::Planned)])).unwrap();

    assert_eq!(report.summary.status, crate::ReportStatus::Noncompliant);
    assert_eq!(report.checks[0].compliance_kind, "container");
    assert_eq!(
        report.checks[0].execution_eligibility,
        crate::ExecutionEligibility::Supported
    );
    assert_eq!(
        report.checks[0].issue_action_hint,
        crate::IssueActionHint::CreateOrUpdatePlanned
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn remux_blocked_node_maps_to_blocked_check() {
    let mut node = remux_node(NodeStatus::Blocked);
    node.status_reason = "snapshot container is unknown".to_owned();
    let report = generate_compliance_report(&plan(vec![node])).unwrap();

    assert_eq!(report.summary.status, crate::ReportStatus::Blocked);
    assert_eq!(report.checks[0].compliance_kind, "container");
    assert_eq!(report.checks[0].reason, "snapshot container is unknown");
    assert_eq!(
        report.checks[0].execution_eligibility,
        crate::ExecutionEligibility::Blocked
    );
    assert_eq!(
        report.checks[0].issue_action_hint,
        crate::IssueActionHint::CreateOrUpdateOpen
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn planned_plus_blocked_maps_to_mixed_report() {
    let report = generate_compliance_report(&plan(vec![
        node(
            NodeStatus::Planned,
            "set_container",
            Some(json!({"container": "mp4"})),
        ),
        node(NodeStatus::Blocked, "set_container", None),
    ]))
    .unwrap();

    assert_eq!(report.summary.status, crate::ReportStatus::Mixed);
    assert_eq!(report.summary.noncompliant_check_count, 1);
    assert_eq!(report.summary.blocked_check_count, 1);
}

#[test]
fn empty_plan_maps_to_not_applicable_report() {
    let report = generate_compliance_report(&plan(Vec::new())).unwrap();

    assert_eq!(report.summary.status, crate::ReportStatus::NotApplicable);
    assert!(report.checks.is_empty());
}

#[test]
fn identical_plans_produce_identical_report_id_and_hash() {
    let left = generate_compliance_report(&plan(vec![node(
        NodeStatus::Planned,
        "set_container",
        Some(json!({"container": "mp4"})),
    )]))
    .unwrap();
    let right = generate_compliance_report(&plan(vec![node(
        NodeStatus::Planned,
        "set_container",
        Some(json!({"container": "mp4"})),
    )]))
    .unwrap();

    assert_eq!(left.report_id, right.report_id);
    assert_eq!(left.report_hash, right.report_hash);
}
