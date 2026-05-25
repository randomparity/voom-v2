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
            NodeStatus::Planned if node.operation_kind == "set_container" => {
                nodes.push(WorkflowNode::Operation(OperationNode {
                    id: format!("policy-node_{}", node.node_id),
                    operation: OperationKind::Remux,
                    policy_target: None,
                    operation_payload: serde_json::Value::Null,
                    depends_on: Vec::new(),
                    depends_on_selected: Vec::new(),
                    provides_selected: None,
                }));
                summary.submitted_node_count += 1;
                *summary.per_operation.entry("remux".to_owned()).or_insert(0) += 1;
            }
            NodeStatus::Planned if node.operation_kind == "transcode_video" => {
                nodes.push(WorkflowNode::Operation(OperationNode {
                    id: format!("policy-node_{}", node.node_id),
                    operation: OperationKind::TranscodeVideo,
                    policy_target: Some(node.target.clone()),
                    operation_payload: node.operation_payload.clone(),
                    depends_on: Vec::new(),
                    depends_on_selected: Vec::new(),
                    provides_selected: None,
                }));
                summary.submitted_node_count += 1;
                *summary
                    .per_operation
                    .entry("transcode_video".to_owned())
                    .or_insert(0) += 1;
            }
            NodeStatus::Planned => {
                return Err(VoomError::PolicyExecution(format!(
                    "unsupported execution operation {}",
                    node.operation_kind
                )));
            }
            NodeStatus::NoOp => summary.skipped_no_op_count += 1,
            NodeStatus::Blocked => summary.blocked_count += 1,
        }
    }

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

#[cfg(test)]
#[path = "policy_bridge_test.rs"]
mod tests;
