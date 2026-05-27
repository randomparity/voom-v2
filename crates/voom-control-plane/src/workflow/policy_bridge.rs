use std::collections::BTreeMap;

use voom_core::VoomError;
use voom_plan::{ExecutionPlan, NodeStatus};
use voom_worker_protocol::OperationKind;

use super::{
    ConcurrencyPolicy, FanOutPolicy, OperationNode, TimingPolicy, WorkflowNode, WorkflowPlan,
};

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
    pub per_operation: BTreeMap<String, u64>,
}

pub fn workflow_plan_from_compliance(
    plan: &ExecutionPlan,
    report: &voom_plan::ComplianceReport,
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
                let operation = match node.operation_kind.as_str() {
                    "remux" => OperationKind::Remux,
                    "transcode_video" => OperationKind::TranscodeVideo,
                    "transcode_audio" => OperationKind::TranscodeAudio,
                    "extract_audio" => OperationKind::ExtractAudio,
                    _ => {
                        return Err(VoomError::PolicyExecution(format!(
                            "unsupported execution operation {}",
                            node.operation_kind
                        )));
                    }
                };
                let workflow_node_id = format!("policy-node_{}", node.node_id);
                workflow_node_ids_by_plan_node_id
                    .insert(node.node_id.clone(), workflow_node_id.clone());
                nodes.push(WorkflowNode::Operation(OperationNode {
                    id: workflow_node_id,
                    operation,
                    policy_target: Some(node.target.clone()),
                    operation_payload: node.operation_payload.clone(),
                    depends_on: Vec::new(),
                    depends_on_selected: Vec::new(),
                    provides_selected: None,
                }));
                summary.submitted_node_count += 1;
                *summary
                    .per_operation
                    .entry(node.operation_kind.clone())
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
            fan_out: FanOutPolicy { max_files: 1 },
            concurrency: ConcurrencyPolicy {
                max_in_flight_dispatches: 1,
            },
            timing: TimingPolicy {
                base_duration_ms: 5,
                jitter_ms: 0,
            },
        })
    };

    Ok(PolicyExecutionPlan { workflow, summary })
}

fn apply_plan_dependencies(
    plan: &ExecutionPlan,
    workflow_node_ids_by_plan_node_id: &BTreeMap<String, String>,
    nodes: &mut [WorkflowNode],
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
        match node {
            WorkflowNode::Operation(operation) => {
                if let Some(depends_on) = dependencies_by_workflow_node_id.remove(&operation.id) {
                    operation.depends_on = depends_on;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "policy_bridge_test.rs"]
mod tests;
