use std::collections::BTreeSet;

use serde_json::json;
use voom_core::{PolicyInputSetId, PolicyVersionId, VoomError};
use voom_events::{Event, SubjectType, payload::IssueLifecyclePayload};
use voom_store::repo::{
    IssueRepo, PolicyInputRepo, PolicyIssueDraft, PolicyIssueMutation, PolicyIssueMutationKind,
    PolicyIssueStatus, PolicyRepo,
};

use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ComplianceReportData {
    pub plan: voom_plan::ExecutionPlan,
    pub report: voom_plan::ComplianceReport,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct IssueApplicationSummary {
    pub created_count: u32,
    pub updated_count: u32,
    pub resolved_count: u32,
    pub skipped_count: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ComplianceApplyData {
    pub report: voom_plan::ComplianceReport,
    pub issues: IssueApplicationSummary,
}

struct DurableComplianceInputs {
    version: voom_store::repo::PolicyVersion,
    input: voom_store::repo::PolicyInputSet,
}

impl ControlPlane {
    /// Generate a compliance report from the current accepted policy version
    /// and durable policy input set.
    ///
    /// # Errors
    /// Returns `NotFound` for missing durable inputs, `PolicyValidationError`
    /// for stale policy versions, and report/planner errors otherwise.
    pub async fn generate_compliance_report(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<ComplianceReportData, VoomError> {
        let inputs = self
            .load_current_accepted_policy_and_input(policy_version_id, input_set_id)
            .await?;
        let policy: voom_policy::CompiledPolicy =
            serde_json::from_value(inputs.version.compiled_json.clone()).map_err(|e| {
                VoomError::PlanGeneration(format!("stored compiled policy JSON is invalid: {e}"))
            })?;
        if policy.source_hash != inputs.version.source_hash
            || policy.schema_version != inputs.version.schema_version
        {
            return Err(VoomError::PlanGeneration(format!(
                "stored compiled policy identity mismatch for policy version {policy_version_id}"
            )));
        }
        let plan = super::plans::plan_compiled_policy_with_input(
            policy,
            super::plans::input_set_to_draft(inputs.input),
            voom_plan::PlanningContext {
                policy_document_id: Some(inputs.version.policy_document_id),
                policy_version_id: Some(inputs.version.id),
                policy_input_set_id: Some(input_set_id),
                ..voom_plan::PlanningContext::default()
            },
        )?;
        let report = voom_plan::generate_compliance_report(&plan)
            .map_err(voom_plan::ComplianceReportError::into_voom_error)?;
        Ok(ComplianceReportData { plan, report })
    }

    /// Apply actionable compliance report checks to durable policy issues.
    ///
    /// # Errors
    /// Propagates durable input, report, issue, and event append failures.
    #[expect(
        clippy::too_many_lines,
        reason = "Sprint 6 apply flow keeps issue upsert, exact resolve, stale resolve, and event emission visibly ordered"
    )]
    pub async fn apply_compliance_report(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<ComplianceApplyData, VoomError> {
        let report_data = self
            .generate_compliance_report(policy_version_id, input_set_id)
            .await?;
        let policy_document_id =
            report_data.report.policy.document_id.ok_or_else(|| {
                VoomError::ComplianceReport("missing policy document id".to_owned())
            })?;
        let input_set_id =
            report_data.report.input.input_set_id.ok_or_else(|| {
                VoomError::ComplianceReport("missing policy input set id".to_owned())
            })?;
        let prefix = dedupe_prefix(policy_document_id, input_set_id);
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let mut summary = IssueApplicationSummary::default();
        let mut emitted_keys = BTreeSet::new();

        for check in &report_data.report.checks {
            if !matches!(
                check.issue_action_hint,
                voom_plan::IssueActionHint::CreateOrUpdateOpen
                    | voom_plan::IssueActionHint::CreateOrUpdatePlanned
                    | voom_plan::IssueActionHint::ResolveMatching
            ) {
                summary.skipped_count += 1;
                continue;
            }
            let key = dedupe_key(policy_document_id, input_set_id, check)?;
            emitted_keys.insert(key.clone());
            match check.issue_action_hint {
                voom_plan::IssueActionHint::CreateOrUpdatePlanned
                | voom_plan::IssueActionHint::CreateOrUpdateOpen => {
                    let mutation = self
                        .issues
                        .upsert_policy_noncompliant_in_tx(&mut tx, issue_draft(&key, check), now)
                        .await?;
                    count_and_emit_issue_event(
                        self,
                        &mut tx,
                        &mut summary,
                        mutation,
                        policy_version_id,
                        Some(report_data.report.report_id.clone()),
                        now,
                    )
                    .await?;
                }
                voom_plan::IssueActionHint::ResolveMatching => {
                    if let Some(mutation) = self
                        .issues
                        .resolve_policy_noncompliant_by_dedupe_key_in_tx(
                            &mut tx,
                            &key,
                            &format!("Policy compliance resolved: {}", check.compliance_kind),
                            "Current compliance report marks this check compliant.",
                            now,
                        )
                        .await?
                    {
                        count_and_emit_issue_event(
                            self,
                            &mut tx,
                            &mut summary,
                            mutation,
                            policy_version_id,
                            Some(report_data.report.report_id.clone()),
                            now,
                        )
                        .await?;
                    } else {
                        summary.skipped_count += 1;
                    }
                }
                voom_plan::IssueActionHint::None => {}
            }
        }

        for row in self
            .issues
            .list_live_policy_noncompliant_by_dedupe_prefix_in_tx(&mut tx, &prefix)
            .await?
        {
            if emitted_keys.contains(&row.dedupe_key) {
                continue;
            }
            if let Some(mutation) = self
                .issues
                .resolve_policy_noncompliant_by_dedupe_key_in_tx(
                    &mut tx,
                    &row.dedupe_key,
                    "Policy compliance resolved: check no longer emitted",
                    "Current compliance report no longer emits this check.",
                    now,
                )
                .await?
            {
                count_and_emit_issue_event(
                    self,
                    &mut tx,
                    &mut summary,
                    mutation,
                    policy_version_id,
                    Some(report_data.report.report_id.clone()),
                    now,
                )
                .await?;
            }
        }

        commit_tx(tx).await?;
        Ok(ComplianceApplyData {
            report: report_data.report,
            issues: summary,
        })
    }

    async fn load_current_accepted_policy_and_input(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<DurableComplianceInputs, VoomError> {
        let version = self
            .policies
            .get_version(policy_version_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!("policy version {policy_version_id} not found"))
            })?;
        let document = self
            .policies
            .get_document(version.policy_document_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!(
                    "policy document {} not found",
                    version.policy_document_id
                ))
            })?;
        if document.current_accepted_version_id != Some(policy_version_id) {
            return Err(VoomError::PolicyValidationError(format!(
                "policy version {policy_version_id} is not the current accepted version"
            )));
        }
        let input = self
            .policy_inputs
            .get_input_set(input_set_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!("policy input set {input_set_id} not found"))
            })?;
        Ok(DurableComplianceInputs { version, input })
    }
}

fn issue_draft(dedupe_key: &str, check: &voom_plan::ComplianceCheck) -> PolicyIssueDraft {
    let status = match check.issue_action_hint {
        voom_plan::IssueActionHint::CreateOrUpdatePlanned => PolicyIssueStatus::Planned,
        voom_plan::IssueActionHint::CreateOrUpdateOpen => PolicyIssueStatus::Open,
        voom_plan::IssueActionHint::None | voom_plan::IssueActionHint::ResolveMatching => {
            PolicyIssueStatus::Open
        }
    };
    PolicyIssueDraft {
        dedupe_key: dedupe_key.to_owned(),
        status,
        title: format!(
            "Policy compliance: {} for {:?}",
            check.compliance_kind, check.target
        ),
        body: format!(
            "Policy requires {}; observed {}; status {}.",
            check.desired_state,
            check
                .observed_state
                .as_ref()
                .map_or_else(|| "unknown".to_owned(), serde_json::Value::to_string),
            check.check_status_string()
        ),
        priority_reason: format!("compliance report {}", check.check_id),
    }
}

fn dedupe_prefix(
    policy_document_id: voom_core::PolicyDocumentId,
    input_set_id: voom_core::PolicyInputSetId,
) -> String {
    format!(
        "policy_noncompliant:v1:policy_document_id={}:input_set_id={}:%",
        policy_document_id.0, input_set_id.0
    )
}

fn dedupe_key(
    policy_document_id: voom_core::PolicyDocumentId,
    input_set_id: voom_core::PolicyInputSetId,
    check: &voom_plan::ComplianceCheck,
) -> Result<String, VoomError> {
    let preimage = json!({
        "target": check.target,
        "compliance_kind": check.compliance_kind,
        "operation_kind": check.operation_kind,
    });
    let canonical = voom_plan::hash::canonical_json(&preimage)
        .map_err(|e| VoomError::ComplianceReport(format!("dedupe key serialization: {e}")))?;
    Ok(format!(
        "policy_noncompliant:v1:policy_document_id={}:input_set_id={}:check={}",
        policy_document_id.0,
        input_set_id.0,
        blake3::hash(canonical.as_bytes()).to_hex()
    ))
}

async fn count_and_emit_issue_event(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    summary: &mut IssueApplicationSummary,
    mutation: PolicyIssueMutation,
    policy_version_id: PolicyVersionId,
    report_id: Option<String>,
    occurred_at: time::OffsetDateTime,
) -> Result<(), VoomError> {
    let event = match mutation.kind {
        PolicyIssueMutationKind::Created => {
            summary.created_count += 1;
            Event::IssueOpened(issue_payload(&mutation, policy_version_id, report_id))
        }
        PolicyIssueMutationKind::Updated => {
            summary.updated_count += 1;
            Event::IssueUpdated(issue_payload(&mutation, policy_version_id, report_id))
        }
        PolicyIssueMutationKind::Resolved => {
            summary.resolved_count += 1;
            Event::IssueResolved(issue_payload(&mutation, policy_version_id, report_id))
        }
        PolicyIssueMutationKind::Unchanged => {
            summary.skipped_count += 1;
            return Ok(());
        }
    };
    append_event(
        &cp.events,
        tx,
        SubjectType::System,
        Some(mutation.row.id.0),
        occurred_at,
        event,
    )
    .await
}

fn issue_payload(
    mutation: &PolicyIssueMutation,
    policy_version_id: PolicyVersionId,
    report_id: Option<String>,
) -> IssueLifecyclePayload {
    IssueLifecyclePayload {
        issue_id: mutation.row.id,
        kind: "policy_noncompliant".to_owned(),
        status: mutation.row.status.as_str().to_owned(),
        dedupe_key: Some(mutation.row.dedupe_key.clone()),
        policy_version_id: Some(policy_version_id),
        report_id,
    }
}

trait CheckStatusText {
    fn check_status_string(&self) -> &'static str;
}

impl CheckStatusText for voom_plan::ComplianceCheck {
    fn check_status_string(&self) -> &'static str {
        match self.check_status {
            voom_plan::CheckStatus::Compliant => "compliant",
            voom_plan::CheckStatus::Noncompliant => "planned",
            voom_plan::CheckStatus::Blocked => "open",
        }
    }
}

#[cfg(test)]
#[path = "compliance_test.rs"]
mod tests;
