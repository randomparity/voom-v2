use std::collections::{BTreeMap, BTreeSet};
use std::pin::Pin;
use std::sync::Arc;

use secrecy::SecretString;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use voom_core::{PolicyInputSetId, PolicyVersionId, VoomError, WorkerId};
use voom_events::{Event, SubjectType, payload::IssueLifecyclePayload};
use voom_scheduler::SingleWorkerPerKindSelector;
use voom_store::repo::{
    IssueRepo, PolicyInputRepo, PolicyIssueDraft, PolicyIssueMutation, PolicyIssueMutationKind,
    PolicyIssueStatus, PolicyRepo,
    workers::{NewCapability, NewGrant, NewWorker, WorkerKind},
};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, OperationKind, OperationRequest, OperationResponse,
    ProgressFrame, ProtocolError, WorkerCredentials,
};

use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};
use crate::workflow::{
    WorkerRuntimeRegistry, WorkflowExecutor,
    executor::WorkflowExecutorOptions,
    policy_bridge::{PolicyExecutionSummary, workflow_plan_from_compliance},
};

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

#[derive(Debug, Clone, serde::Serialize)]
pub struct ComplianceExecuteData {
    pub report: voom_plan::ComplianceReport,
    pub issues: IssueApplicationSummary,
    pub execution: PolicyExecutionSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_diagnostic: Option<voom_plan::ComplianceDiagnostic>,
}

#[derive(Debug)]
pub struct ComplianceExecuteError {
    pub source: VoomError,
    pub partial: Option<ComplianceExecuteData>,
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

    /// Apply compliance issues, then execute supported planned compliance work.
    ///
    /// # Errors
    /// Returns partial data when issue application completed but bridge or
    /// workflow execution failed.
    pub async fn execute_compliance_policy(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<ComplianceExecuteData, ComplianceExecuteError> {
        let report_data = self
            .generate_compliance_report(policy_version_id, input_set_id)
            .await
            .map_err(|source| ComplianceExecuteError {
                source,
                partial: None,
            })?;
        let apply_data = self
            .apply_compliance_report(policy_version_id, input_set_id)
            .await
            .map_err(|source| ComplianceExecuteError {
                source,
                partial: None,
            })?;

        let bridge = match workflow_plan_from_compliance(&report_data.plan, &apply_data.report) {
            Ok(bridge) => bridge,
            Err(source) => {
                let partial = ComplianceExecuteData {
                    report: apply_data.report,
                    issues: apply_data.issues,
                    execution: empty_execution_summary(&report_data.plan, &report_data.report),
                    execution_diagnostic: Some(execution_diagnostic(
                        &source,
                        &report_data.plan.plan_id,
                        &report_data.report.report_id,
                    )),
                };
                return Err(ComplianceExecuteError {
                    source,
                    partial: Some(partial),
                });
            }
        };

        let Some(workflow) = bridge.workflow else {
            return Ok(ComplianceExecuteData {
                report: apply_data.report,
                issues: apply_data.issues,
                execution: bridge.summary,
                execution_diagnostic: None,
            });
        };

        let runtimes = self
            .synthetic_policy_runtime_registry()
            .await
            .map_err(|source| ComplianceExecuteError {
                source,
                partial: None,
            })?;
        self.execute_compliance_workflow(apply_data, bridge.summary, workflow, runtimes)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn execute_compliance_policy_without_runtime_for_test(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<ComplianceExecuteData, ComplianceExecuteError> {
        let report_data = self
            .generate_compliance_report(policy_version_id, input_set_id)
            .await
            .map_err(|source| ComplianceExecuteError {
                source,
                partial: None,
            })?;
        let apply_data = self
            .apply_compliance_report(policy_version_id, input_set_id)
            .await
            .map_err(|source| ComplianceExecuteError {
                source,
                partial: None,
            })?;
        let bridge = workflow_plan_from_compliance(&report_data.plan, &apply_data.report).map_err(
            |source| ComplianceExecuteError {
                source,
                partial: None,
            },
        )?;
        self.register_synthetic_policy_worker()
            .await
            .map_err(|source| ComplianceExecuteError {
                source,
                partial: None,
            })?;
        self.execute_compliance_workflow(
            apply_data,
            bridge.summary,
            bridge.workflow.unwrap(),
            WorkerRuntimeRegistry::new(),
        )
        .await
    }

    async fn execute_compliance_workflow(
        &self,
        apply_data: ComplianceApplyData,
        mut bridge_summary: PolicyExecutionSummary,
        workflow: crate::workflow::WorkflowPlan,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<ComplianceExecuteData, ComplianceExecuteError> {
        let executor = WorkflowExecutor::with_options(
            self.clone(),
            SingleWorkerPerKindSelector,
            runtimes,
            WorkflowExecutorOptions::for_tests(),
        );
        let result = executor.submit_and_run(workflow).await;
        let run = match result {
            Ok(summary) => summary,
            Err(err) => {
                merge_run_summary(&mut bridge_summary, &err.summary);
                let partial = ComplianceExecuteData {
                    report: apply_data.report,
                    issues: apply_data.issues,
                    execution: bridge_summary,
                    execution_diagnostic: None,
                };
                return Err(ComplianceExecuteError {
                    source: err.source,
                    partial: Some(partial),
                });
            }
        };
        merge_run_summary(&mut bridge_summary, &run);
        Ok(ComplianceExecuteData {
            report: apply_data.report,
            issues: apply_data.issues,
            execution: bridge_summary,
            execution_diagnostic: None,
        })
    }

    async fn synthetic_policy_runtime_registry(&self) -> Result<WorkerRuntimeRegistry, VoomError> {
        let worker_id = self.register_synthetic_policy_worker().await?;
        Ok(WorkerRuntimeRegistry::new().with_in_process_runtime(
            worker_id,
            Arc::new(SyntheticPolicyClient),
            WorkerCredentials {
                worker_id,
                worker_epoch: 0,
                secret: SecretString::from("policy-synthetic-secret"),
            },
        ))
    }

    async fn register_synthetic_policy_worker(&self) -> Result<WorkerId, VoomError> {
        let worker = self
            .register_worker(NewWorker {
                name: "policy-synthetic-remux".to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: self.clock().now(),
            })
            .await?;
        self.record_capability(NewCapability {
            worker_id: worker.id,
            operation: operation_name(OperationKind::Remux).to_owned(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: json!({}),
        })
        .await?;
        self.record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec![operation_name(OperationKind::Remux).to_owned()],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({ operation_name(OperationKind::Remux): 1 }),
        })
        .await?;
        Ok(worker.id)
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

#[derive(Debug)]
struct SyntheticPolicyClient;

#[async_trait::async_trait]
impl ClientHandle for SyntheticPolicyClient {
    async fn handshake(
        &self,
        _offered: u32,
    ) -> Result<voom_worker_protocol::HandshakeResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        let response = OperationResponse {
            lease_id: request.lease_id,
            accepted_at: chrono::Utc::now(),
        };
        let frame = ProgressFrame::Result {
            lease_id: request.lease_id,
            seq: 0,
            emitted_at: chrono::Utc::now(),
            payload: json!({"status": "ok"}),
        };
        let body = serde_json::to_vec(&frame).map_err(|_| ProtocolError::InternalServerError)?;
        let (mut writer, reader) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            let _ = writer.write_all(&body).await;
            let _ = writer.write_all(b"\n").await;
        });
        Ok(DispatchStream {
            response,
            frames: voom_worker_protocol::NdjsonReader::new(
                Pin::from(Box::new(reader) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
                request.lease_id,
            ),
        })
    }
}

fn empty_execution_summary(
    plan: &voom_plan::ExecutionPlan,
    report: &voom_plan::ComplianceReport,
) -> PolicyExecutionSummary {
    PolicyExecutionSummary {
        plan_id: plan.plan_id.clone(),
        report_id: report.report_id.clone(),
        job_id: None,
        submitted_node_count: 0,
        skipped_no_op_count: 0,
        blocked_count: 0,
        dispatch_count: 0,
        failure_count: 0,
        per_operation: BTreeMap::new(),
    }
}

fn execution_diagnostic(
    source: &VoomError,
    plan_id: &str,
    report_id: &str,
) -> voom_plan::ComplianceDiagnostic {
    voom_plan::ComplianceDiagnostic {
        severity: voom_plan::ComplianceDiagnosticSeverity::Error,
        code: voom_plan::ComplianceDiagnosticCode::UnsupportedExecutionOperation,
        message: source.to_string(),
        plan_id: Some(plan_id.to_owned()),
        report_id: Some(report_id.to_owned()),
        node_id: None,
        check_id: None,
        target: None,
        suggestion: None,
    }
}

fn merge_run_summary(
    execution: &mut PolicyExecutionSummary,
    run: &crate::workflow::WorkflowRunSummary,
) {
    execution.job_id = Some(run.job_id);
    execution.dispatch_count = run.dispatch_count;
    execution.failure_count = run.failure_count;
    execution.per_operation = run
        .per_operation
        .iter()
        .map(|(operation, summary)| (operation_name(*operation).to_owned(), summary.success_count))
        .collect();
}

fn operation_name(operation: OperationKind) -> &'static str {
    match operation {
        OperationKind::ScanLibrary => "scan_library",
        OperationKind::ProbeFile => "probe_file",
        OperationKind::HashFile => "hash_file",
        OperationKind::IdentifyMedia => "identify_media",
        OperationKind::ScoreQuality => "score_quality",
        OperationKind::SyncExternalSystem => "sync_external_system",
        OperationKind::BackUpFile => "back_up_file",
        OperationKind::Remux => "remux",
        OperationKind::TranscodeVideo => "transcode_video",
        OperationKind::EditTracks => "edit_tracks",
        OperationKind::ExtractAudio => "extract_audio",
        OperationKind::TranscribeAudio => "transcribe_audio",
        OperationKind::VerifyArtifact => "verify_artifact",
        OperationKind::CommitArtifact => "commit_artifact",
        OperationKind::DeleteArtifact => "delete_artifact",
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
