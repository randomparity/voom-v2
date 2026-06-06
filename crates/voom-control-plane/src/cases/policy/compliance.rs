use std::collections::{BTreeSet, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;
use serde_json::json;
use sqlx::Row;
use voom_core::{
    OperationKind, PROTOCOL_VERSION, PolicyInputSetId, PolicyVersionId, VoomError, WorkerId,
};
use voom_events::{Event, SubjectType, payload::IssueLifecyclePayload};
use voom_plan::PlanOperationKind;
use voom_store::repo::{
    PolicyIssueDraft, PolicyIssueMutation, PolicyIssueMutationKind, PolicyIssueStatus,
};
use voom_worker_protocol::{HttpClient, WorkerCredentials};

use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};
use crate::workflow::WorkerRuntimeRegistry;
use crate::workflow::execution::executor::{
    OperationArtifactRoots, WorkflowArtifactRoots, WorkflowExecutorOptions,
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

/// The durable result of a `compliance execute` run: the issues applied from
/// the initial report, plus the phase-barrier coordinator's job-grain summary
/// and per-phase / per-`(file, phase)` rows. The flat single-report / flat-ticket
/// shape of Sprints 12–15 is relocated here — per-phase reports live on
/// [`PhaseSummaryView`], and the tickets each branch ran live on
/// [`FilePhaseSummaryView::ticket_ids`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComplianceExecuteData {
    /// Execute-only: the applied-findings summary from this run's initial report.
    /// Intentionally absent from `report --job-id`, which regenerates and applies
    /// nothing; every other field here matches [`ComplianceRunReportData`].
    pub issues: IssueApplicationSummary,
    pub summary: WorkflowSummaryView,
    pub phases: Vec<PhaseSummaryView>,
    pub file_phases: Vec<FilePhaseSummaryView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_phase_index: Option<usize>,
}

impl ComplianceExecuteData {
    fn from_outcome(
        issues: IssueApplicationSummary,
        outcome: &crate::workflow::coordinator::CoordinatorOutcome,
    ) -> Self {
        let phases: Vec<PhaseSummaryView> =
            outcome.phases.iter().map(PhaseSummaryView::from).collect();
        let latest_phase_index = latest_phase_index(&phases);
        Self {
            issues,
            summary: WorkflowSummaryView::from(&outcome.summary),
            phases,
            file_phases: outcome
                .file_phases
                .iter()
                .map(FilePhaseSummaryView::from)
                .collect(),
            latest_phase_index,
        }
    }
}

/// Index into an ascending-`phase_ordinal` phase chain of the latest (highest
/// `phase_ordinal`) phase. `None` for a zero-phase run. Shared by `execute` and
/// `report --job-id` so both modes compute the latest-phase pointer identically.
fn latest_phase_index(phases: &[PhaseSummaryView]) -> Option<usize> {
    phases.len().checked_sub(1)
}

/// Job-grain workflow summary, rendered without the nondeterministic
/// `elapsed`/`created_at` columns so the CLI golden is stable.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkflowSummaryView {
    pub job_id: u64,
    pub branch_count: u32,
    pub ticket_count: u32,
    pub dispatch_count: u64,
    pub retry_count: u64,
    pub failure_count: u64,
    pub peak_active_workflow_leases: u32,
    pub per_operation: serde_json::Value,
}

impl From<&voom_store::repo::workflow_summaries::WorkflowSummary> for WorkflowSummaryView {
    fn from(summary: &voom_store::repo::workflow_summaries::WorkflowSummary) -> Self {
        Self {
            job_id: summary.job_id.0,
            branch_count: summary.branch_count,
            ticket_count: summary.ticket_count,
            dispatch_count: summary.dispatch_count,
            retry_count: summary.retry_count,
            failure_count: summary.failure_count,
            peak_active_workflow_leases: summary.peak_active_workflow_leases,
            per_operation: summary.per_operation.clone(),
        }
    }
}

/// Per-phase summary, rendered without the row `id`/`created_at`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PhaseSummaryView {
    pub phase_ordinal: u32,
    pub phase_name: String,
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<serde_json::Value>,
}

impl From<&voom_store::repo::workflow_summaries::PhaseSummary> for PhaseSummaryView {
    fn from(phase: &voom_store::repo::workflow_summaries::PhaseSummary) -> Self {
        Self {
            phase_ordinal: phase.phase_ordinal,
            phase_name: phase.phase_name.clone(),
            outcome: phase.outcome.as_str(),
            report_id: phase.report.as_ref().map(|report| report.report_id.clone()),
            report: phase.report.as_ref().map(|report| report.report.clone()),
        }
    }
}

/// Per-`(file, phase)` summary, rendered without the row `id`/`created_at`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FilePhaseSummaryView {
    pub phase_ordinal: u32,
    pub branch_id: String,
    pub outcome: &'static str,
    pub ticket_ids: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub produced_file_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub produced_file_location_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_handle_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reprobe_snapshot_id: Option<u64>,
}

impl From<&voom_store::repo::workflow_summaries::FilePhaseSummary> for FilePhaseSummaryView {
    fn from(file_phase: &voom_store::repo::workflow_summaries::FilePhaseSummary) -> Self {
        Self {
            phase_ordinal: file_phase.phase_ordinal,
            branch_id: file_phase.branch_id.clone(),
            outcome: file_phase.outcome.as_str(),
            ticket_ids: file_phase.ticket_ids.iter().map(|id| id.0).collect(),
            produced_file_version_id: file_phase.produced_file_version_id.map(|id| id.0),
            produced_file_location_id: file_phase.produced_file_location_id.map(|id| id.0),
            artifact_handle_id: file_phase.artifact_handle_id.map(|id| id.0),
            reprobe_snapshot_id: file_phase.reprobe_snapshot_id.map(|id| id.0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ComplianceExecutionOptions {
    pub transcode_staging_root: PathBuf,
    pub transcode_target_dir: PathBuf,
    pub remux_staging_root: PathBuf,
    pub remux_target_dir: PathBuf,
    pub audio_staging_root: PathBuf,
    pub audio_target_dir: PathBuf,
}

impl Default for ComplianceExecutionOptions {
    fn default() -> Self {
        let defaults = WorkflowExecutorOptions::default();
        Self {
            transcode_staging_root: defaults.artifact_roots.transcode.staging_root,
            transcode_target_dir: defaults.artifact_roots.transcode.target_dir,
            remux_staging_root: defaults.artifact_roots.remux.staging_root,
            remux_target_dir: defaults.artifact_roots.remux.target_dir,
            audio_staging_root: defaults.artifact_roots.audio.staging_root,
            audio_target_dir: defaults.artifact_roots.audio.target_dir,
        }
    }
}

impl ComplianceExecutionOptions {
    /// Route a single staging-root override to every operation family.
    pub fn apply_staging_root(&mut self, root: PathBuf) {
        self.transcode_staging_root.clone_from(&root);
        self.remux_staging_root.clone_from(&root);
        self.audio_staging_root = root;
    }

    /// Route a single output-directory override to every operation family.
    pub fn apply_output_dir(&mut self, dir: PathBuf) {
        self.transcode_target_dir.clone_from(&dir);
        self.remux_target_dir.clone_from(&dir);
        self.audio_target_dir = dir;
    }

    /// Per-operation `(working dir, --output-dir)` pairs the phase-barrier
    /// coordinator uses for post-run promotion. Every phase commits its artifact
    /// into the working dir (see [`committed_working_dir`]); after the run each
    /// file's terminal artifact is moved from its working dir into the matching
    /// `output_dir`. The `*_target_dir` fields carry the operator's intended
    /// `--output-dir`, NOT the commit target.
    pub(crate) fn promotion_plan(&self) -> PromotionPlan {
        PromotionPlan {
            pairs: vec![
                PromotionPair {
                    working_dir: committed_working_dir(&self.transcode_staging_root, "transcode"),
                    output_dir: self.transcode_target_dir.clone(),
                },
                PromotionPair {
                    working_dir: committed_working_dir(&self.remux_staging_root, "remux"),
                    output_dir: self.remux_target_dir.clone(),
                },
                PromotionPair {
                    working_dir: committed_working_dir(&self.audio_staging_root, "audio"),
                    output_dir: self.audio_target_dir.clone(),
                },
            ],
        }
    }
}

/// Subdirectory of a staging root that holds committed-but-not-yet-promoted
/// artifacts, partitioned per operation so each family's working dir is a
/// distinct prefix (promotion matches a terminal artifact to its output dir by
/// working-dir prefix).
const COMMITTED_SUBDIR: &str = ".committed";

/// The directory a given operation family commits its artifacts into. Distinct
/// from the operator's `--output-dir`: intermediate (non-terminal) artifacts
/// stay here, and only each file's terminal artifact is promoted out.
pub(crate) fn committed_working_dir(staging_root: &Path, operation: &str) -> PathBuf {
    staging_root.join(COMMITTED_SUBDIR).join(operation)
}

/// One operation family's commit working dir paired with the operator output
/// dir its terminal artifacts promote into.
#[derive(Debug, Clone)]
pub(crate) struct PromotionPair {
    pub(crate) working_dir: PathBuf,
    pub(crate) output_dir: PathBuf,
}

/// Per-operation promotion pairs for a run, derived from the execution options.
#[derive(Debug, Clone)]
pub(crate) struct PromotionPlan {
    pub(crate) pairs: Vec<PromotionPair>,
}

impl From<ComplianceExecutionOptions> for WorkflowExecutorOptions {
    fn from(options: ComplianceExecutionOptions) -> Self {
        // Destructure exhaustively so a new facade path field is a compile
        // error here rather than being silently dropped by `..default()`.
        let ComplianceExecutionOptions {
            transcode_staging_root,
            transcode_target_dir,
            remux_staging_root,
            remux_target_dir,
            audio_staging_root,
            audio_target_dir,
        } = options;
        // Each operation commits into a per-family working dir under its staging
        // root (the `OperationArtifactRoots::target_dir`), NOT the operator's
        // `--output-dir` (the `*_target_dir` fields here). Post-run promotion
        // moves each file's terminal artifact out to the output dir;
        // intermediates stay in the working dir.
        let _ = (&transcode_target_dir, &remux_target_dir, &audio_target_dir);
        Self {
            artifact_roots: WorkflowArtifactRoots {
                transcode: OperationArtifactRoots::new(
                    transcode_staging_root.clone(),
                    committed_working_dir(&transcode_staging_root, "transcode"),
                ),
                remux: OperationArtifactRoots::new(
                    remux_staging_root.clone(),
                    committed_working_dir(&remux_staging_root, "remux"),
                ),
                audio: OperationArtifactRoots::new(
                    audio_staging_root.clone(),
                    committed_working_dir(&audio_staging_root, "audio"),
                ),
            },
            ..WorkflowExecutorOptions::default()
        }
    }
}

/// Read-only view of a completed run's durable workflow summary: the job-grain
/// counters, the ordered per-phase chain (each carrying its folded report), the
/// per-`(file, phase)` rows, and an index into `phases` of the latest (highest
/// `phase_ordinal`) phase. An index, not a duplicated row, so the latest
/// report has a single wire representation (ADR-0010).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComplianceRunReportData {
    pub summary: WorkflowSummaryView,
    pub phases: Vec<PhaseSummaryView>,
    pub file_phases: Vec<FilePhaseSummaryView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_phase_index: Option<usize>,
}

#[derive(Debug)]
pub struct ComplianceExecuteError {
    pub source: VoomError,
    pub partial: Option<ComplianceExecuteData>,
}

pub(crate) struct DurableComplianceInputs {
    pub(crate) version: voom_store::repo::PolicyVersion,
    pub(crate) input: voom_store::repo::PolicyInputSet,
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
        let policy = self.compiled_policy_for_version(&inputs.version).await?;
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
    pub async fn apply_compliance_report(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<ComplianceApplyData, VoomError> {
        let report_data = self
            .generate_compliance_report(policy_version_id, input_set_id)
            .await?;
        self.apply_generated_compliance_report(&report_data, policy_version_id)
            .await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Sprint 6 apply flow keeps issue upsert, exact resolve, stale resolve, and event emission visibly ordered"
    )]
    async fn apply_generated_compliance_report(
        &self,
        report_data: &ComplianceReportData,
        policy_version_id: PolicyVersionId,
    ) -> Result<ComplianceApplyData, VoomError> {
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
            report: report_data.report.clone(),
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
        self.execute_compliance_policy_with_options(
            policy_version_id,
            input_set_id,
            ComplianceExecutionOptions::default(),
        )
        .await
    }

    pub async fn execute_compliance_policy_with_options(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
    ) -> Result<ComplianceExecuteData, ComplianceExecuteError> {
        let registered = self.policy_runtime_registry().await.map_err(no_partial)?;
        let live = self.probe_live_runtimes(registered.clone()).await;
        let report_data = self
            .generate_compliance_report(policy_version_id, input_set_id)
            .await
            .map_err(no_partial)?;
        self.reject_dead_endpoint_operations(&report_data.plan, &registered, &live)
            .await
            .map_err(no_partial)?;
        self.execute_compliance_with_report(
            report_data,
            policy_version_id,
            input_set_id,
            options,
            live,
        )
        .await
    }

    /// Apply the initial report's findings to durable issues, then drive the
    /// phase-barrier coordinator with the given runtime registry. Issue
    /// application runs once up front (unchanged from Sprints 12–15); the
    /// per-phase workflow execution is the coordinator's.
    #[cfg(test)]
    async fn execute_compliance_with_runtimes(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<ComplianceExecuteData, ComplianceExecuteError> {
        let report_data = self
            .generate_compliance_report(policy_version_id, input_set_id)
            .await
            .map_err(no_partial)?;
        self.execute_compliance_with_report(
            report_data,
            policy_version_id,
            input_set_id,
            options,
            runtimes,
        )
        .await
    }

    async fn execute_compliance_with_report(
        &self,
        report_data: ComplianceReportData,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<ComplianceExecuteData, ComplianceExecuteError> {
        let apply_data = self
            .apply_generated_compliance_report(&report_data, policy_version_id)
            .await
            .map_err(no_partial)?;
        let issues = apply_data.issues;
        match self
            .run_phase_barrier_with_runtimes(policy_version_id, input_set_id, options, runtimes)
            .await
        {
            Ok(outcome) => Ok(ComplianceExecuteData::from_outcome(issues, &outcome)),
            Err(err) => Err(ComplianceExecuteError {
                source: err.source,
                partial: err
                    .partial
                    .map(|outcome| ComplianceExecuteData::from_outcome(issues, &outcome)),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) async fn execute_compliance_policy_with_runtime_registry_and_options_for_test(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        runtimes: WorkerRuntimeRegistry,
        options: ComplianceExecutionOptions,
    ) -> Result<ComplianceExecuteData, ComplianceExecuteError> {
        self.execute_compliance_with_runtimes(policy_version_id, input_set_id, options, runtimes)
            .await
    }

    pub(crate) async fn policy_runtime_registry(&self) -> Result<WorkerRuntimeRegistry, VoomError> {
        let mut registry = WorkerRuntimeRegistry::new();
        let rows = sqlx::query(
            "SELECT w.id, w.epoch, wc.extra \
             FROM workers w \
             JOIN worker_capabilities wc ON wc.worker_id = w.id \
             WHERE w.status IN ('registered', 'active') \
               AND wc.operation IN (?, ?, ?, ?) \
             ORDER BY w.id ASC",
        )
        .bind(OperationKind::Remux.as_str())
        .bind(OperationKind::TranscodeVideo.as_str())
        .bind(OperationKind::TranscodeAudio.as_str())
        .bind(OperationKind::ExtractAudio.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("policy runtime registry: {e}")))?;

        for row in rows {
            let worker_id_raw = row
                .try_get::<i64, _>("id")
                .map_err(|e| VoomError::Database(format!("policy runtime worker id: {e}")))?;
            let worker_epoch_raw = row
                .try_get::<i64, _>("epoch")
                .map_err(|e| VoomError::Database(format!("policy runtime worker epoch: {e}")))?;
            let worker_id = WorkerId(sqlite_u64(worker_id_raw, "worker id")?);
            let worker_epoch = sqlite_u64(worker_epoch_raw, "worker epoch")?;
            let extra: String = row
                .try_get("extra")
                .map_err(|e| VoomError::Database(format!("policy runtime registry extra: {e}")))?;
            let Some((endpoint, secret)) = runtime_metadata(&extra)? else {
                continue;
            };
            registry.register_in_process_runtime(
                worker_id,
                Arc::new(HttpClient::new(endpoint)),
                WorkerCredentials {
                    worker_id,
                    worker_epoch,
                    secret,
                },
            );
        }
        Ok(registry)
    }

    /// [`Self::policy_runtime_registry`] with each worker's recorded endpoint
    /// probed for liveness, dropping any whose `handshake` does not succeed
    /// within [`LIVENESS_PROBE_TIMEOUT`]. A stale endpoint left by a
    /// hard-killed `run-local` is excluded here so `execute` fails fast before
    /// dispatch instead of mid-workflow after partial commits.
    ///
    /// # Errors
    /// Propagates database errors from [`Self::policy_runtime_registry`].
    #[cfg(test)]
    pub(crate) async fn live_policy_runtime_registry(
        &self,
    ) -> Result<WorkerRuntimeRegistry, VoomError> {
        let registered = self.policy_runtime_registry().await?;
        Ok(self.probe_live_runtimes(registered).await)
    }

    /// Drop every worker whose endpoint does not complete a handshake within
    /// [`LIVENESS_PROBE_TIMEOUT`], returning only the reachable runtimes.
    async fn probe_live_runtimes(
        &self,
        mut registry: WorkerRuntimeRegistry,
    ) -> WorkerRuntimeRegistry {
        let mut dead = Vec::new();
        for (worker_id, runtime) in registry.entries() {
            let probe = tokio::time::timeout(
                LIVENESS_PROBE_TIMEOUT,
                runtime.client.handshake(PROTOCOL_VERSION),
            )
            .await;
            if !matches!(probe, Ok(Ok(_))) {
                dead.push(worker_id);
            }
        }
        for worker_id in dead {
            registry.remove(worker_id);
        }
        registry
    }

    /// Reject a run whose plan has a planned operation whose only registered
    /// worker(s) failed the liveness probe — the stale-endpoint case that would
    /// otherwise surface as a `missing runtime for worker` crash mid-dispatch.
    /// Operations with no registered worker at all are left to the per-ticket
    /// no-eligible-worker path, preserving partial-coverage dispatch.
    async fn reject_dead_endpoint_operations(
        &self,
        plan: &voom_plan::ExecutionPlan,
        registered: &WorkerRuntimeRegistry,
        live: &WorkerRuntimeRegistry,
    ) -> Result<(), VoomError> {
        let dead_operations = self
            .operations_lost_to_dead_endpoints(registered, live)
            .await?;
        for node in &plan.nodes {
            if node.status != voom_plan::NodeStatus::Planned {
                continue;
            }
            let Some((operation, worker_kind)) = policy_worker_requirement(node.operation_kind)
            else {
                continue;
            };
            if dead_operations.contains(operation) {
                return Err(VoomError::Config(format!(
                    "no live worker for operation '{operation}'; start one with: \
                     voom worker run-local --kind {worker_kind}"
                )));
            }
        }
        Ok(())
    }

    /// Policy operation names (`remux`, `transcode_video`, …) that have a
    /// registered worker but no *live* one: every worker for the operation
    /// failed its endpoint liveness probe.
    async fn operations_lost_to_dead_endpoints(
        &self,
        registered: &WorkerRuntimeRegistry,
        live: &WorkerRuntimeRegistry,
    ) -> Result<HashSet<String>, VoomError> {
        let registered_ids: HashSet<WorkerId> = registered.entries().map(|(id, _)| id).collect();
        let live_ids: HashSet<WorkerId> = live.entries().map(|(id, _)| id).collect();
        if registered_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let rows = sqlx::query(
            "SELECT w.id, wc.operation \
             FROM workers w \
             JOIN worker_capabilities wc ON wc.worker_id = w.id \
             WHERE w.status IN ('registered', 'active') \
               AND wc.operation IN (?, ?, ?, ?)",
        )
        .bind(OperationKind::Remux.as_str())
        .bind(OperationKind::TranscodeVideo.as_str())
        .bind(OperationKind::TranscodeAudio.as_str())
        .bind(OperationKind::ExtractAudio.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("dead endpoint operations: {e}")))?;

        let mut registered_operations = HashSet::new();
        let mut live_operations = HashSet::new();
        for row in rows {
            let worker_id_raw = row
                .try_get::<i64, _>("id")
                .map_err(|e| VoomError::Database(format!("dead endpoint worker id: {e}")))?;
            let worker_id = WorkerId(sqlite_u64(worker_id_raw, "worker id")?);
            let operation: String = row
                .try_get("operation")
                .map_err(|e| VoomError::Database(format!("dead endpoint operation: {e}")))?;
            if live_ids.contains(&worker_id) {
                live_operations.insert(operation);
            } else if registered_ids.contains(&worker_id) {
                registered_operations.insert(operation);
            }
        }
        Ok(&registered_operations - &live_operations)
    }

    pub(crate) async fn load_current_accepted_policy_and_input(
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

    /// Deserialize a policy version's stored compiled policy, verify its stored
    /// identity, and resolve video-profile references in-memory before the pure
    /// planner runs. Shared by the single-shot report path and the phase-barrier
    /// coordinator so both plan against the same compiled policy.
    pub(crate) async fn compiled_policy_for_version(
        &self,
        version: &voom_store::repo::PolicyVersion,
    ) -> Result<voom_policy::CompiledPolicy, VoomError> {
        let mut policy: voom_policy::CompiledPolicy =
            serde_json::from_value(version.compiled_json.clone()).map_err(|e| {
                VoomError::PlanGeneration(format!("stored compiled policy JSON is invalid: {e}"))
            })?;
        if policy.source_hash != version.source_hash
            || policy.schema_version != version.schema_version
        {
            return Err(VoomError::PlanGeneration(format!(
                "stored compiled policy identity mismatch for policy version {}",
                version.id
            )));
        }
        // Resolve after the stored-identity check so the mutation cannot affect
        // `source_hash`.
        super::plans::resolve_profiles_in_policy(self, &mut policy).await?;
        Ok(policy)
    }

    /// Read a completed phase-barrier run's durable summary by job id.
    ///
    /// Read-only: opens no transaction, submits no tickets, and regenerates no
    /// report. The reports it returns are the ones the run already folded into
    /// the per-phase rows (ADR-0008/0010), so post-run identity equals what
    /// `execute` returned. The per-phase and per-`(file, phase)` rows preserve
    /// the repo's `phase_ordinal` (then `branch_id`) ordering.
    ///
    /// # Errors
    /// Returns `NotFound` for both an unknown job id and a known job that has no
    /// workflow summary yet (still running or not a workflow job), with distinct
    /// messages; propagates database errors from the underlying repo reads.
    pub async fn read_compliance_run_report(
        &self,
        job_id: voom_core::JobId,
    ) -> Result<ComplianceRunReportData, VoomError> {
        let repo = &self.workflow_summaries;
        let Some(summary) = repo.get_summary(job_id).await? else {
            let message = if self.jobs.get(job_id).await?.is_some() {
                format!(
                    "job {} has no completed workflow summary (still running or not a workflow job)",
                    job_id.0
                )
            } else {
                format!("no job with id {}", job_id.0)
            };
            return Err(VoomError::NotFound(message));
        };
        let phases: Vec<PhaseSummaryView> = repo
            .phases_for_job(job_id)
            .await?
            .iter()
            .map(PhaseSummaryView::from)
            .collect();
        let file_phases = repo
            .file_phases_for_job(job_id)
            .await?
            .iter()
            .map(FilePhaseSummaryView::from)
            .collect();
        let latest_phase_index = latest_phase_index(&phases);
        Ok(ComplianceRunReportData {
            summary: WorkflowSummaryView::from(&summary),
            phases,
            file_phases,
            latest_phase_index,
        })
    }
}

/// How long an endpoint has to complete a handshake before it is treated as
/// dead. Short enough that a stale endpoint fails the run quickly; long enough
/// that a busy-but-live worker is not falsely dropped.
const LIVENESS_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

fn no_partial(source: VoomError) -> ComplianceExecuteError {
    ComplianceExecuteError {
        source,
        partial: None,
    }
}

/// Map a plan operation to the worker operation name it dispatches as and the
/// `run-local --kind` value that starts a worker for it. Operations with no
/// executing worker (metadata edits, container set, conditionals) return
/// `None`: the coordinator handles them without a runtime.
fn policy_worker_requirement(kind: PlanOperationKind) -> Option<(&'static str, &'static str)> {
    match kind {
        PlanOperationKind::Remux => Some(("remux", "mkvtoolnix")),
        PlanOperationKind::TranscodeVideo => Some(("transcode_video", "ffmpeg")),
        PlanOperationKind::TranscodeAudio => Some(("transcode_audio", "ffmpeg")),
        PlanOperationKind::ExtractAudio => Some(("extract_audio", "ffmpeg")),
        PlanOperationKind::SetContainer
        | PlanOperationKind::KeepTracks
        | PlanOperationKind::RemoveTracks
        | PlanOperationKind::ReorderTracks
        | PlanOperationKind::SetDefaults
        | PlanOperationKind::ClearTrackActions
        | PlanOperationKind::ClearTags
        | PlanOperationKind::SetTag
        | PlanOperationKind::DeleteTag
        | PlanOperationKind::Conditional
        | PlanOperationKind::Rules => None,
    }
}

fn runtime_metadata(extra: &str) -> Result<Option<(SocketAddr, SecretString)>, VoomError> {
    let value: serde_json::Value = serde_json::from_str(extra)
        .map_err(|e| VoomError::Database(format!("worker capability extra JSON: {e}")))?;
    let endpoint = value
        .get("endpoint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| VoomError::Config("worker runtime endpoint is missing".to_owned()))?;
    let secret = value
        .get("secret")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| VoomError::Config("worker runtime secret is missing".to_owned()))?;
    let endpoint = endpoint
        .parse::<SocketAddr>()
        .map_err(|e| VoomError::Config(format!("worker endpoint {endpoint:?}: {e}")))?;
    Ok(Some((endpoint, SecretString::from(secret.to_owned()))))
}

fn sqlite_u64(value: i64, label: &str) -> Result<u64, VoomError> {
    u64::try_from(value).map_err(|_| VoomError::Database(format!("{label} was negative: {value}")))
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
