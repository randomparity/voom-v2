use std::collections::BTreeMap;

use serde_json::json;

use crate::{
    CheckStatus, ComplianceCheck, ComplianceDiagnostic, ComplianceDiagnosticCode,
    ComplianceDiagnosticSeverity, ComplianceInputIdentity, CompliancePolicyIdentity,
    ComplianceProvenance, ComplianceReport, ComplianceSummary, ExecutionEligibility, ExecutionPlan,
    IssueActionHint, NodeStatus, PlanNode, PlanOperationKind, ReportStatus,
};

#[derive(Debug)]
pub struct ComplianceReportError {
    pub diagnostic: Box<ComplianceDiagnostic>,
}

impl ComplianceReportError {
    #[must_use]
    pub fn into_voom_error(self) -> voom_core::VoomError {
        voom_core::VoomError::ComplianceReport(self.diagnostic.message)
    }
}

pub fn generate_compliance_report(
    plan: &ExecutionPlan,
) -> Result<ComplianceReport, ComplianceReportError> {
    let report_id_preimage = report_id_preimage(plan);
    let report_id_preimage_json = crate::hash::canonical_json(&report_id_preimage)
        .map_err(|err| serialization_error(&err))?;
    let provisional_report_id = crate::compliance_hash::report_id(&report_id_preimage)
        .map_err(|err| serialization_error(&err))?;
    let checks: Vec<ComplianceCheck> = plan
        .nodes
        .iter()
        .map(|node| check_from_node(&report_id_preimage_json, node))
        .collect();
    let diagnostics = compliance_diagnostics(plan, &checks);
    let summary = summarize_checks(&checks);

    let mut report = ComplianceReport {
        schema_version: 1,
        report_id: provisional_report_id,
        report_hash: String::new(),
        plan_id: plan.plan_id.clone(),
        plan_hash: plan.plan_hash.clone(),
        policy: CompliancePolicyIdentity {
            slug: plan.policy.slug.clone(),
            source_hash: plan.policy.source_hash.clone(),
            document_id: plan.policy.document_id,
            version_id: plan.policy.version_id,
        },
        input: ComplianceInputIdentity {
            slug: plan.input.slug.clone(),
            source_label: plan.input.source_label.clone(),
            input_set_id: plan.input.input_set_id,
            fixture_labels: plan.input.fixture_labels.clone(),
        },
        summary,
        checks,
        diagnostics,
        provenance: ComplianceProvenance::default(),
    };
    report.report_hash =
        crate::compliance_hash::report_hash(&report).map_err(|err| serialization_error(&err))?;
    Ok(report)
}

fn report_id_preimage(plan: &ExecutionPlan) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "plan_id": plan.plan_id,
        "plan_hash": plan.plan_hash,
        "policy": {
            "slug": plan.policy.slug,
            "source_hash": plan.policy.source_hash,
            "document_id": plan.policy.document_id,
            "version_id": plan.policy.version_id,
        },
        "input": {
            "slug": plan.input.slug,
            "source_label": plan.input.source_label,
            "input_set_id": plan.input.input_set_id,
            "fixture_labels": plan.input.fixture_labels,
        },
        "nodes": plan.nodes.iter().map(|node| {
            json!({
                "node_id": node.node_id,
                "status": node.status,
                "operation_kind": node.operation_kind,
            })
        }).collect::<Vec<_>>(),
    })
}

fn check_from_node(report_id_preimage: &str, node: &PlanNode) -> ComplianceCheck {
    ComplianceCheck {
        check_id: crate::compliance_hash::check_id(
            report_id_preimage,
            &node.node_id,
            node.operation_kind.as_str(),
        ),
        node_id: node.node_id.clone(),
        target: node.target.clone(),
        compliance_kind: compliance_kind(node).to_owned(),
        operation_kind: node.operation_kind,
        desired_state: node.operation_payload.clone(),
        observed_state: node.observed_state.clone(),
        check_status: check_status(node),
        reason: node.status_reason.clone(),
        issue_action_hint: issue_action_hint(node),
        execution_eligibility: execution_eligibility(node),
    }
}

fn compliance_kind(node: &PlanNode) -> &'static str {
    match (node.status, node.operation_kind) {
        (_, PlanOperationKind::Remux) | (NodeStatus::NoOp, PlanOperationKind::SetContainer) => {
            "container"
        }
        (_, PlanOperationKind::TranscodeVideo) => "transcode_video",
        (_, PlanOperationKind::TranscodeAudio) => "transcode_audio",
        (_, PlanOperationKind::ExtractAudio) => "extract_audio",
        _ => "unsupported",
    }
}

fn check_status(node: &PlanNode) -> CheckStatus {
    match node.status {
        NodeStatus::NoOp => CheckStatus::Compliant,
        NodeStatus::Planned => CheckStatus::Noncompliant,
        NodeStatus::Blocked => CheckStatus::Blocked,
    }
}

fn issue_action_hint(node: &PlanNode) -> IssueActionHint {
    match (node.status, node.operation_kind) {
        (NodeStatus::NoOp, _) => IssueActionHint::ResolveMatching,
        (
            NodeStatus::Planned,
            PlanOperationKind::Remux
            | PlanOperationKind::TranscodeVideo
            | PlanOperationKind::TranscodeAudio
            | PlanOperationKind::ExtractAudio,
        ) => IssueActionHint::CreateOrUpdatePlanned,
        (
            NodeStatus::Blocked,
            PlanOperationKind::Remux
            | PlanOperationKind::TranscodeVideo
            | PlanOperationKind::TranscodeAudio
            | PlanOperationKind::ExtractAudio,
        ) => IssueActionHint::CreateOrUpdateOpen,
        _ => IssueActionHint::None,
    }
}

fn execution_eligibility(node: &PlanNode) -> ExecutionEligibility {
    match (node.status, node.operation_kind) {
        (
            NodeStatus::Planned,
            PlanOperationKind::Remux
            | PlanOperationKind::TranscodeVideo
            | PlanOperationKind::TranscodeAudio
            | PlanOperationKind::ExtractAudio,
        ) => ExecutionEligibility::Supported,
        (NodeStatus::NoOp, _) => ExecutionEligibility::NoOp,
        (
            NodeStatus::Blocked,
            PlanOperationKind::Remux
            | PlanOperationKind::TranscodeVideo
            | PlanOperationKind::TranscodeAudio
            | PlanOperationKind::ExtractAudio,
        ) => ExecutionEligibility::Blocked,
        _ => ExecutionEligibility::Unsupported,
    }
}

fn compliance_diagnostics(
    plan: &ExecutionPlan,
    checks: &[ComplianceCheck],
) -> Vec<ComplianceDiagnostic> {
    checks
        .iter()
        .filter(|check| check.compliance_kind == "unsupported")
        .map(|check| ComplianceDiagnostic {
            severity: ComplianceDiagnosticSeverity::Warning,
            code: ComplianceDiagnosticCode::UnsupportedComplianceOperation,
            message: format!(
                "operation {} is not supported by compliance reports",
                check.operation_kind
            ),
            plan_id: Some(plan.plan_id.clone()),
            report_id: None,
            node_id: Some(check.node_id.clone()),
            check_id: Some(check.check_id.clone()),
            target: Some(check.target.clone()),
            suggestion: None,
        })
        .collect()
}

fn summarize_checks(checks: &[ComplianceCheck]) -> ComplianceSummary {
    let mut summary = ComplianceSummary {
        status: ReportStatus::NotApplicable,
        total_check_count: checked_count(checks.len()),
        compliant_check_count: 0,
        noncompliant_check_count: 0,
        blocked_check_count: 0,
        executable_check_count: 0,
        operation_counts_by_kind: BTreeMap::new(),
    };

    for check in checks {
        *summary
            .operation_counts_by_kind
            .entry(check.operation_kind)
            .or_insert(0) += 1;
        match check.check_status {
            CheckStatus::Compliant => summary.compliant_check_count += 1,
            CheckStatus::Noncompliant => summary.noncompliant_check_count += 1,
            CheckStatus::Blocked => summary.blocked_check_count += 1,
        }
        if check.execution_eligibility == ExecutionEligibility::Supported {
            summary.executable_check_count += 1;
        }
    }

    summary.status = report_status(&summary);
    summary
}

fn report_status(summary: &ComplianceSummary) -> ReportStatus {
    match (
        summary.total_check_count,
        summary.noncompliant_check_count,
        summary.blocked_check_count,
    ) {
        (0, _, _) => ReportStatus::NotApplicable,
        (_, 0, 0) => ReportStatus::Compliant,
        (_, 0, _) => ReportStatus::Blocked,
        (_, _, 0) => ReportStatus::Noncompliant,
        _ => ReportStatus::Mixed,
    }
}

fn checked_count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

fn serialization_error(err: &serde_json::Error) -> ComplianceReportError {
    ComplianceReportError {
        diagnostic: Box::new(ComplianceDiagnostic {
            severity: ComplianceDiagnosticSeverity::Error,
            code: ComplianceDiagnosticCode::DeterministicSerializationFailure,
            message: format!("deterministic compliance report serialization failed: {err}"),
            plan_id: None,
            report_id: None,
            node_id: None,
            check_id: None,
            target: None,
            suggestion: None,
        }),
    }
}

#[cfg(test)]
#[path = "compliance_report_test.rs"]
mod tests;
