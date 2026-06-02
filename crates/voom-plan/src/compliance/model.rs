use std::collections::BTreeMap;

use crate::PlanOperationKind;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Compliant,
    Noncompliant,
    Blocked,
    Mixed,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Compliant,
    Noncompliant,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueActionHint {
    None,
    CreateOrUpdatePlanned,
    CreateOrUpdateOpen,
    ResolveMatching,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEligibility {
    Supported,
    NoOp,
    Blocked,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceReport {
    pub schema_version: u32,
    pub report_id: String,
    pub report_hash: String,
    pub plan_id: String,
    pub plan_hash: String,
    pub policy: CompliancePolicyIdentity,
    pub input: ComplianceInputIdentity,
    pub summary: ComplianceSummary,
    pub checks: Vec<ComplianceCheck>,
    pub diagnostics: Vec<ComplianceDiagnostic>,
    pub provenance: ComplianceProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompliancePolicyIdentity {
    pub slug: String,
    pub source_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<voom_core::PolicyDocumentId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<voom_core::PolicyVersionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceInputIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_set_id: Option<voom_core::PolicyInputSetId>,
    pub fixture_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceSummary {
    pub status: ReportStatus,
    pub total_check_count: u32,
    pub compliant_check_count: u32,
    pub noncompliant_check_count: u32,
    pub blocked_check_count: u32,
    pub executable_check_count: u32,
    pub operation_counts_by_kind: BTreeMap<PlanOperationKind, u32>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceCheck {
    pub check_id: String,
    pub node_id: String,
    pub target: crate::TargetRef,
    pub compliance_kind: String,
    pub operation_kind: PlanOperationKind,
    pub desired_state: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_state: Option<serde_json::Value>,
    pub check_status: CheckStatus,
    pub reason: String,
    pub issue_action_hint: IssueActionHint,
    pub execution_eligibility: ExecutionEligibility,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplianceDiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplianceDiagnosticCode {
    UnsupportedComplianceOperation,
    UnsupportedExecutionOperation,
    MissingDurablePolicyIdentity,
    MissingDurableInputIdentity,
    InvalidReportRequest,
    IssueApplicationConflict,
    DeterministicSerializationFailure,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceDiagnostic {
    pub severity: ComplianceDiagnosticSeverity,
    pub code: ComplianceDiagnosticCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<crate::TargetRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceProvenance {
    pub reporter: String,
    pub format: String,
}

impl Default for ComplianceProvenance {
    fn default() -> Self {
        Self {
            reporter: "voom-plan".to_owned(),
            format: "sprint6-v1".to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
