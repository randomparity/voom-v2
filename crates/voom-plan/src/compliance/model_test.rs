use serde_json::json;

use crate::PlanOperationKind;

use super::*;

#[test]
fn report_status_serializes_as_snake_case_contract() {
    assert_eq!(
        serde_json::to_value(ReportStatus::NotApplicable).unwrap(),
        json!("not_applicable")
    );
    assert_eq!(
        serde_json::to_value(CheckStatus::Noncompliant).unwrap(),
        json!("noncompliant")
    );
    assert_eq!(
        serde_json::to_value(IssueActionHint::CreateOrUpdatePlanned).unwrap(),
        json!("create_or_update_planned")
    );
    assert_eq!(
        serde_json::to_value(ExecutionEligibility::Supported).unwrap(),
        json!("supported")
    );
}

#[test]
fn compliance_report_serializes_expected_public_shape() {
    let report = ComplianceReport {
        schema_version: 1,
        report_id: "report_test".to_owned(),
        report_hash: "blake3:test".to_owned(),
        plan_id: "plan_test".to_owned(),
        plan_hash: "blake3:plan".to_owned(),
        policy: CompliancePolicyIdentity {
            slug: "container-metadata".to_owned(),
            source_hash: "abc".to_owned(),
            document_id: Some(voom_core::PolicyDocumentId(1)),
            version_id: Some(voom_core::PolicyVersionId(2)),
        },
        input: ComplianceInputIdentity {
            slug: Some("synthetic".to_owned()),
            source_label: None,
            input_set_id: Some(voom_core::PolicyInputSetId(3)),
            fixture_labels: vec!["synthetic".to_owned()],
        },
        summary: ComplianceSummary {
            status: ReportStatus::Noncompliant,
            total_check_count: 1,
            compliant_check_count: 0,
            noncompliant_check_count: 1,
            blocked_check_count: 0,
            executable_check_count: 1,
            operation_counts_by_kind: [(PlanOperationKind::SetContainer, 1)].into_iter().collect(),
        },
        checks: vec![ComplianceCheck {
            check_id: "check_test".to_owned(),
            node_id: "node_test".to_owned(),
            target: voom_policy::TargetRef::Synthetic {
                key: "movie-a".to_owned(),
                kind: voom_policy::TargetKind::MediaWork,
            },
            compliance_kind: "container".to_owned(),
            operation_kind: PlanOperationKind::SetContainer,
            desired_state: json!({"container": "mkv"}),
            observed_state: Some(json!({"container": "mp4"})),
            check_status: CheckStatus::Noncompliant,
            reason: "container mp4 will be changed to mkv".to_owned(),
            issue_action_hint: IssueActionHint::CreateOrUpdatePlanned,
            execution_eligibility: ExecutionEligibility::Supported,
        }],
        diagnostics: Vec::new(),
        provenance: ComplianceProvenance::default(),
    };

    let value = serde_json::to_value(report).unwrap();
    assert_eq!(value["summary"]["status"], "noncompliant");
    assert_eq!(value["checks"][0]["compliance_kind"], "container");
    assert_eq!(value["checks"][0]["observed_state"]["container"], "mp4");
}
