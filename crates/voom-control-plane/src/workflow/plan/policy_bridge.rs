use std::collections::BTreeMap;

use voom_core::OperationKind;
use voom_core::VoomError;
use voom_plan::{ExecutionPlan, NodeStatus, PlanOperationKind};

use super::{ConcurrencyPolicy, FanOutPolicy, OperationNode, TimingPolicy, WorkflowPlan};

const POLICY_WORKFLOW_NODE_ID_PREFIX: &str = "policy-node_";

#[derive(Debug, Clone, serde::Serialize)]
pub struct PolicyExecutionPlan {
    pub workflow: Option<WorkflowPlan>,
    pub summary: PolicyExecutionSummary,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PolicyExecutionSummary {
    pub plan_id: String,
    pub report_id: String,
    pub job_id: Option<voom_core::JobId>,
    pub submitted_node_count: u32,
    pub skipped_no_op_count: u32,
    pub blocked_count: u32,
    pub dispatch_count: u64,
    pub failure_count: u64,
    pub per_operation: BTreeMap<PlanOperationKind, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct WorkflowExecutionShape {
    max_files: usize,
    max_in_flight_dispatches: usize,
}

impl WorkflowExecutionShape {
    /// Create the workflow execution shape that the policy bridge must embed.
    ///
    /// # Errors
    /// Returns `POLICY_EXECUTION_ERROR` if either execution limit is zero.
    pub fn new(max_files: usize, max_in_flight_dispatches: usize) -> Result<Self, VoomError> {
        if max_files == 0 {
            return Err(VoomError::PolicyExecution(
                "workflow execution shape max_files must be greater than 0".to_owned(),
            ));
        }
        if max_in_flight_dispatches == 0 {
            return Err(VoomError::PolicyExecution(
                "workflow execution shape max_in_flight_dispatches must be greater than 0"
                    .to_owned(),
            ));
        }
        Ok(Self {
            max_files,
            max_in_flight_dispatches,
        })
    }

    #[cfg(test)]
    const fn single_file() -> Self {
        Self {
            max_files: 1,
            max_in_flight_dispatches: 1,
        }
    }
}

pub fn workflow_plan_from_compliance(
    plan: &ExecutionPlan,
    report: &voom_plan::ComplianceReport,
    shape: WorkflowExecutionShape,
) -> Result<PolicyExecutionPlan, VoomError> {
    let mut nodes = Vec::new();
    let mut workflow_node_ids_by_plan_node_id = BTreeMap::new();
    let mut summary = PolicyExecutionSummary {
        plan_id: plan.plan_id.clone(),
        report_id: report.report_id.clone(),
        job_id: None,
        submitted_node_count: 0,
        skipped_no_op_count: 0,
        blocked_count: 0,
        dispatch_count: 0,
        failure_count: 0,
        per_operation: BTreeMap::new(),
    };

    for node in &plan.nodes {
        match node.status {
            NodeStatus::Planned => {
                let operation = execution_operation(node.operation_kind)?;
                let workflow_node_id = policy_workflow_node_id(&node.node_id);
                workflow_node_ids_by_plan_node_id
                    .insert(node.node_id.clone(), workflow_node_id.clone());
                nodes.push(OperationNode {
                    id: workflow_node_id,
                    operation,
                    policy_target: Some(node.target.clone()),
                    operation_payload: node.operation_payload.clone(),
                    depends_on: Vec::new(),
                    depends_on_selected: Vec::new(),
                    provides_selected: None,
                });
                summary.submitted_node_count += 1;
                *summary
                    .per_operation
                    .entry(node.operation_kind)
                    .or_insert(0) += 1;
            }
            NodeStatus::NoOp => summary.skipped_no_op_count += 1,
            NodeStatus::Blocked => summary.blocked_count += 1,
        }
    }

    apply_plan_dependencies(plan, &workflow_node_ids_by_plan_node_id, &mut nodes);

    let workflow = if nodes.is_empty() {
        None
    } else {
        Some(WorkflowPlan {
            id: format!("policy-{}", report.report_id),
            seed: 6,
            nodes,
            fan_out: FanOutPolicy {
                max_files: shape.max_files,
            },
            concurrency: ConcurrencyPolicy {
                max_in_flight_dispatches: shape.max_in_flight_dispatches,
            },
            timing: TimingPolicy {
                base_duration_ms: 5,
                jitter_ms: 0,
            },
        })
    };

    Ok(PolicyExecutionPlan { workflow, summary })
}

pub(crate) fn policy_workflow_node_id(plan_node_id: &str) -> String {
    format!("{POLICY_WORKFLOW_NODE_ID_PREFIX}{plan_node_id}")
}

pub(crate) fn is_policy_workflow_node_id(node_id: &str) -> bool {
    node_id.starts_with(POLICY_WORKFLOW_NODE_ID_PREFIX)
}

#[cfg(test)]
fn single_file_workflow_plan_from_compliance(
    plan: &ExecutionPlan,
    report: &voom_plan::ComplianceReport,
) -> Result<PolicyExecutionPlan, VoomError> {
    workflow_plan_from_compliance(plan, report, WorkflowExecutionShape::single_file())
}

fn execution_operation(operation_kind: PlanOperationKind) -> Result<OperationKind, VoomError> {
    match operation_kind {
        PlanOperationKind::Remux => Ok(OperationKind::Remux),
        PlanOperationKind::TranscodeVideo => Ok(OperationKind::TranscodeVideo),
        PlanOperationKind::TranscodeAudio => Ok(OperationKind::TranscodeAudio),
        PlanOperationKind::ExtractAudio => Ok(OperationKind::ExtractAudio),
        PlanOperationKind::VerifyArtifact => Ok(OperationKind::VerifyArtifact),
        _ => Err(VoomError::PolicyExecution(format!(
            "unsupported execution operation {operation_kind}"
        ))),
    }
}

fn apply_plan_dependencies(
    plan: &ExecutionPlan,
    workflow_node_ids_by_plan_node_id: &BTreeMap<String, String>,
    nodes: &mut [OperationNode],
) {
    let mut dependencies_by_workflow_node_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in &plan.edges {
        let Some(from_workflow_node_id) = workflow_node_ids_by_plan_node_id.get(&edge.from_node_id)
        else {
            continue;
        };
        let Some(to_workflow_node_id) = workflow_node_ids_by_plan_node_id.get(&edge.to_node_id)
        else {
            continue;
        };
        dependencies_by_workflow_node_id
            .entry(to_workflow_node_id.clone())
            .or_default()
            .push(from_workflow_node_id.clone());
    }
    for node in nodes {
        if let Some(depends_on) = dependencies_by_workflow_node_id.remove(&node.id) {
            node.depends_on = depends_on;
        }
    }
}

#[cfg(test)]
#[path = "policy_bridge_test.rs"]
mod tests;
