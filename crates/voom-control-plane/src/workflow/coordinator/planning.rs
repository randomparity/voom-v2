//! Phase planning/policy projection and per-phase report/summary aggregation.
//!
//! Pure projection helpers the phase loop uses to turn the policy and the
//! current working set into a planned phase, classify each file's node, and roll
//! per-file outcomes up to phase- and job-grain durable summaries.

use std::time::Duration;

use serde_json::{Value, json};
use voom_core::{FileVersionId, JobId, VoomError};
use voom_plan::{ExecutionPlan, NodeStatus, PlanningContext, PlanningRequest};
use voom_policy::{PolicyInputSetDraft, TargetRef};
use voom_store::repo::identity::{IdentityRepo, MediaSnapshot};
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, NewWorkflowSummary, PhaseOutcome, PhaseReport,
};

use crate::ControlPlane;
use crate::workflow::coordinator::resume::{
    active_version_with_snapshot, project_media_snapshot_input,
};
use crate::workflow::coordinator::{Disposition, PhaseFile};

/// Classify each active file's node for a phase by `NodeStatus`. A file with no
/// node (its target was skipped via `run_if`/`skip_if`) is `Skipped`.
pub(super) fn classify_phase(files: &[PhaseFile], plan: &ExecutionPlan) -> Vec<Disposition> {
    files
        .iter()
        .map(|file| {
            let node = plan.nodes.iter().find(|node| {
                matches!(node.target, TargetRef::FileVersion { id } if id == file.version_id)
            });
            match node {
                Some(node) => match node.status {
                    NodeStatus::Blocked => Disposition::Blocked,
                    NodeStatus::NoOp => Disposition::Skipped,
                    NodeStatus::Planned => Disposition::Planned {
                        node_id: node.node_id.clone(),
                    },
                },
                None => Disposition::Skipped,
            }
        })
        .collect()
}

/// Roll the per-file outcomes up to the phase grain (plan §3 step 6).
pub(super) fn phase_outcome(file_outcomes: &[FilePhaseOutcome]) -> PhaseOutcome {
    if file_outcomes.is_empty() {
        return PhaseOutcome::Skipped;
    }
    let any_committed = file_outcomes.contains(&FilePhaseOutcome::Committed);
    let any_blocked = file_outcomes.contains(&FilePhaseOutcome::Blocked);
    if file_outcomes
        .iter()
        .all(|outcome| *outcome == FilePhaseOutcome::Committed)
    {
        PhaseOutcome::Completed
    } else if any_committed {
        PhaseOutcome::PartiallyCommitted
    } else if any_blocked {
        PhaseOutcome::Blocked
    } else {
        PhaseOutcome::Skipped
    }
}

/// Reject a policy whose any `phase_order` phase declares a non-default
/// `on_error` strategy. `continue`/`skip` are deferred this sprint (Sprint 16
/// §11); honoring them partially would be indistinguishable at runtime from real
/// handling, so they are rejected at resolve time before any job opens (#165).
pub(super) fn reject_unhandled_on_error(
    policy: &voom_policy::CompiledPolicy,
) -> Result<(), VoomError> {
    for phase_name in &policy.phase_order {
        let Some(phase) = policy.phases.iter().find(|phase| phase.name == *phase_name) else {
            continue;
        };
        let label = match phase.on_error {
            None | Some(voom_policy::ErrorStrategy::Abort) => continue,
            Some(voom_policy::ErrorStrategy::Continue) => "continue",
            Some(voom_policy::ErrorStrategy::Skip) => "skip",
        };
        return Err(VoomError::PolicyValidationError(format!(
            "phase `{phase_name}` declares on_error `{label}`, which is not supported this sprint \
             (only the default abort); see Sprint 16 §11"
        )));
    }
    Ok(())
}

/// Build a phase's planning input: the input set's identity with each still-active
/// file's current snapshot projected in place of the original snapshots.
pub(super) fn phase_draft(base: &PolicyInputSetDraft, files: &[PhaseFile]) -> PolicyInputSetDraft {
    let mut draft = base.clone();
    draft.media_snapshots = files
        .iter()
        .map(|file| project_media_snapshot_input(file.ordinal, &file.snapshot))
        .collect();
    draft
}

/// Regenerate the per-phase compliance report against the phase's refreshed facts
/// (ADR-0008): re-project every file that *entered* the phase at its refreshed
/// chain tip (committed files at their produced version + re-probe snapshot,
/// others unchanged), re-plan the same phase, and generate the report. Pure: the
/// `refreshed` snapshots are supplied by `finalize_phase`, so this does no
/// database reads, dispatches no tickets, advances no version, and adds no phase.
pub(super) fn regenerate_phase_report(
    policy: &voom_policy::CompiledPolicy,
    context: &PlanningContext,
    base_draft: &PolicyInputSetDraft,
    phase_name: &str,
    refreshed: &[(u32, MediaSnapshot)],
) -> Result<PhaseReport, VoomError> {
    let mut draft = base_draft.clone();
    draft.media_snapshots = refreshed
        .iter()
        .map(|(ordinal, snapshot)| project_media_snapshot_input(*ordinal, snapshot))
        .collect();
    let plan = voom_plan::plan_phase(
        PlanningRequest {
            policy: policy.clone(),
            input: draft,
            context: context.clone(),
        },
        phase_name,
    )
    .map_err(voom_plan::PlanGenerationError::into_voom_error)?;
    let report = voom_plan::generate_compliance_report(&plan)
        .map_err(voom_plan::ComplianceReportError::into_voom_error)?;
    Ok(PhaseReport {
        report_id: report.report_id.clone(),
        report: serde_json::to_value(&report)
            .map_err(|e| VoomError::Internal(format!("phase report encode: {e}")))?,
    })
}

/// Job-grain summary counters from the last phase that dispatched work (counts
/// are job-cumulative, so the final run reflects the whole job), or zeros when
/// no phase dispatched.
pub(super) fn job_grain_summary(
    job_id: JobId,
    run: Option<&crate::workflow::WorkflowRunSummary>,
) -> NewWorkflowSummary {
    match run {
        Some(run) => NewWorkflowSummary {
            job_id,
            branch_count: run.branch_count,
            ticket_count: run.ticket_count,
            dispatch_count: run.dispatch_count,
            retry_count: run.retry_count,
            failure_count: run.failure_count,
            peak_active_workflow_leases: run.peak_active_workflow_leases,
            elapsed: run.elapsed,
            per_operation: per_operation_json(run),
        },
        None => zero_phase_summary(job_id),
    }
}

pub(super) fn zero_phase_summary(job_id: JobId) -> NewWorkflowSummary {
    NewWorkflowSummary {
        job_id,
        branch_count: 0,
        ticket_count: 0,
        dispatch_count: 0,
        retry_count: 0,
        failure_count: 0,
        peak_active_workflow_leases: 0,
        elapsed: Duration::ZERO,
        per_operation: json!({}),
    }
}

/// Per-operation counters as an opaque JSON object keyed by operation name (the
/// store keeps `per_operation` decoupled from the executor's summary type).
fn per_operation_json(run: &crate::workflow::WorkflowRunSummary) -> Value {
    let map = run
        .per_operation
        .iter()
        .map(|(kind, summary)| {
            (
                kind.as_str().to_owned(),
                json!({
                    "ticket_count": summary.ticket_count,
                    "dispatch_count": summary.dispatch_count,
                    "success_count": summary.success_count,
                    "retry_count": summary.retry_count,
                    "failure_count": summary.failure_count,
                }),
            )
        })
        .collect::<serde_json::Map<String, Value>>();
    Value::Object(map)
}

impl ControlPlane {
    /// Resolve every active file's current chain tip (and its latest snapshot)
    /// into the per-phase working set.
    pub(super) async fn initial_phase_files(
        &self,
        branch_ids: &[(FileVersionId, String)],
    ) -> Result<Vec<PhaseFile>, VoomError> {
        let mut files = Vec::with_capacity(branch_ids.len());
        for (index, (version_id, branch_id)) in branch_ids.iter().enumerate() {
            let version = self
                .identity
                .get_file_version(*version_id)
                .await?
                .ok_or_else(|| {
                    VoomError::NotFound(format!("file version {version_id} not found"))
                })?;
            let (tip, snapshot) =
                active_version_with_snapshot(&self.identity, version.file_asset_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::NotFound(format!(
                            "file version {version_id} has no active snapshot to project"
                        ))
                    })?;
            files.push(PhaseFile {
                asset_id: version.file_asset_id,
                version_id: tip.id,
                start_version_id: *version_id,
                snapshot,
                branch_id: branch_id.clone(),
                ordinal: u32::try_from(index + 1)
                    .map_err(|e| VoomError::Internal(format!("file ordinal overflow: {e}")))?,
                resume_ordinal: 0,
            });
        }
        Ok(files)
    }
}
