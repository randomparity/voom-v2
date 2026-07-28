//! Phase planning/policy projection and per-phase report/summary aggregation.
//!
//! Pure projection helpers the phase loop uses to turn the policy and the
//! current working set into a planned phase, classify each file's node, and roll
//! per-file outcomes up to phase- and job-grain durable summaries.

use std::time::Duration;

use serde_json::{Value, json};
use voom_core::{FileVersionId, JobId, VoomError};
use voom_plan::{ExecutionPlan, NodeStatus, PlanOperationKind, PlanningContext, PlanningRequest};
use voom_policy::{PolicyInputSetDraft, TargetRef};
use voom_store::repo::identity::MediaSnapshot;
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, NewWorkflowSummary, PhaseOutcome, PhaseReport,
};

use crate::cases::policy::plans::ResolvedFileInput;
use crate::media_snapshot::planning_input;
use crate::workflow::coordinator::{Disposition, PhaseFile};

/// Classify all of an active file's nodes for a phase by `NodeStatus`. A phase
/// may contain several independent operations for one file, so any planned node
/// makes the file planned even when an earlier sibling node is already a no-op.
/// A file with no node (its target was skipped via `run_if`/`skip_if`) is
/// `Skipped`.
pub(super) fn classify_phase(
    files: &[PhaseFile],
    plan: &ExecutionPlan,
) -> Result<Vec<Disposition>, VoomError> {
    files
        .iter()
        .map(|file| {
            let mut node_ids = Vec::new();
            let mut blocked = false;
            let mut file_mutation_count = 0;
            for node in &plan.nodes {
                if !matches!(
                    node.target,
                    TargetRef::FileVersion { id } if id == file.version_id
                ) {
                    continue;
                }
                match node.status {
                    NodeStatus::Planned => {
                        node_ids.push(node.node_id.clone());
                        if matches!(
                            node.operation_kind,
                            PlanOperationKind::Remux
                                | PlanOperationKind::TranscodeVideo
                                | PlanOperationKind::TranscodeAudio
                        ) {
                            file_mutation_count += 1;
                        }
                    }
                    NodeStatus::Blocked => blocked = true,
                    NodeStatus::NoOp => {}
                }
            }
            if file_mutation_count > 1 {
                Err(VoomError::PolicyExecution(format!(
                    "phase planned {file_mutation_count} file mutations for branch `{}`; \
                     split same-file mutations into dependent phases",
                    file.branch_id
                )))
            } else if !node_ids.is_empty() {
                Ok(Disposition::Planned { node_ids })
            } else if blocked {
                Ok(Disposition::Blocked)
            } else {
                Ok(Disposition::Skipped)
            }
        })
        .collect()
}

/// Roll the per-file outcomes up to the phase grain (plan §3 step 6).
pub(super) fn phase_outcome(file_outcomes: &[FilePhaseOutcome]) -> PhaseOutcome {
    if file_outcomes.is_empty() {
        return PhaseOutcome::Skipped;
    }
    let any_completed = file_outcomes.iter().any(|outcome| {
        matches!(
            outcome,
            FilePhaseOutcome::Committed | FilePhaseOutcome::Verified
        )
    });
    let any_blocked = file_outcomes.contains(&FilePhaseOutcome::Blocked);
    if file_outcomes.iter().all(|outcome| {
        matches!(
            outcome,
            FilePhaseOutcome::Committed | FilePhaseOutcome::Verified
        )
    }) {
        PhaseOutcome::Completed
    } else if any_completed {
        PhaseOutcome::PartiallyCommitted
    } else if any_blocked {
        PhaseOutcome::Blocked
    } else {
        PhaseOutcome::Skipped
    }
}

/// Reject the legacy compiled `skip` strategy, which is not published source
/// syntax and has no execution semantics. Abort and continue are executable.
pub(super) fn reject_unpublished_on_error(
    policy: &voom_policy::CompiledPolicy,
) -> Result<(), VoomError> {
    for phase_name in &policy.phase_order {
        let Some(phase) = policy.phases.iter().find(|phase| phase.name == *phase_name) else {
            continue;
        };
        let label = match phase.on_error {
            None
            | Some(voom_policy::ErrorStrategy::Abort | voom_policy::ErrorStrategy::Continue) => {
                continue;
            }
            Some(voom_policy::ErrorStrategy::Skip) => "skip",
        };
        return Err(VoomError::PolicyValidationError(format!(
            "phase `{phase_name}` declares unpublished on_error `{label}`"
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
        .map(|file| planning_input(file.ordinal, &file.snapshot))
        .collect();
    draft
}

/// Clear a coordinator-resolved phase gate. When every file failed the gate,
/// also clear operations so the planner can produce a truthful zero-node report
/// from a non-empty input set.
pub(super) fn resolved_phase_policy(
    policy: &voom_policy::CompiledPolicy,
    phase_name: &str,
    suppress_operations: bool,
) -> Result<voom_policy::CompiledPolicy, VoomError> {
    let mut policy = policy.clone();
    let phase = policy
        .phases
        .iter_mut()
        .find(|phase| phase.name == phase_name)
        .ok_or_else(|| {
            VoomError::PolicyExecution(format!(
                "phase `{phase_name}` is in phase_order but has no compiled phase"
            ))
        })?;
    phase.run_if = None;
    if suppress_operations {
        phase.operations.clear();
    }
    Ok(policy)
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
    gate_admission: &[bool],
) -> Result<PhaseReport, VoomError> {
    if refreshed.len() != gate_admission.len() {
        return Err(VoomError::Internal(format!(
            "phase `{phase_name}` refreshed {} files for {} gate decisions",
            refreshed.len(),
            gate_admission.len()
        )));
    }
    let admitted = refreshed
        .iter()
        .zip(gate_admission)
        .filter(|(_, admitted)| **admitted)
        .map(|(refreshed, _)| refreshed.clone())
        .collect::<Vec<_>>();
    let suppress_operations = admitted.is_empty();
    let report_inputs = if suppress_operations {
        refreshed
    } else {
        admitted.as_slice()
    };
    let mut draft = base_draft.clone();
    draft.media_snapshots = report_inputs
        .iter()
        .map(|(ordinal, snapshot)| planning_input(*ordinal, snapshot))
        .collect();
    let policy = resolved_phase_policy(policy, phase_name, suppress_operations)?;
    let plan = voom_plan::plan_phase(
        PlanningRequest {
            policy,
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

/// Build the first-phase working set from the authority records retained by
/// stored-input resolution. No identity read occurs here.
pub(super) fn initial_phase_files(
    resolved: Vec<ResolvedFileInput>,
    branch_ids: Vec<(FileVersionId, String)>,
) -> Result<Vec<PhaseFile>, VoomError> {
    if resolved.len() != branch_ids.len() {
        return Err(VoomError::Internal(
            "resolved files and branch ids differ in length".to_owned(),
        ));
    }
    resolved
        .into_iter()
        .zip(branch_ids)
        .map(|(resolved, (version_id, branch_id))| {
            if version_id != resolved.selected_version_id {
                return Err(VoomError::Internal(format!(
                    "branch id version {version_id} does not match selected version {}",
                    resolved.selected_version_id
                )));
            }
            Ok(PhaseFile {
                asset_id: resolved.file_asset_id,
                version_id: resolved.active_version.id,
                snapshot: resolved.active_snapshot,
                branch_id,
                ordinal: resolved.ordinal,
                resume_ordinal: 0,
                phase_history: std::collections::BTreeMap::new(),
            })
        })
        .collect()
}
