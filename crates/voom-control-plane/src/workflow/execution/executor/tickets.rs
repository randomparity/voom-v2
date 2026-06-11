//! Root and node ticket creation, root-payload rendering for each operation
//! kind, and the small ticket/dependency helpers the expansion and spawn
//! children share.

use std::collections::HashSet;

use serde_json::Value;
use time::OffsetDateTime;
use voom_core::OperationKind;
use voom_core::{JobId, TicketOperation, VoomError};
use voom_store::repo::identity::IdentityRepo;
use voom_store::repo::tickets::{NewTicket, Ticket};

use crate::workflow::execution::executor::WorkflowExecutor;
use crate::workflow::execution::timing::{EffectiveTiming, seeded_timing};
use crate::workflow::plan::binding::{
    BindingError, BranchContext, PolicyFileSource, render_default_payload,
    render_default_payload_with_fan_out, render_policy_extract_audio_payload,
    render_policy_remux_payload, render_policy_transcode_audio_payload,
    render_policy_transcode_payload,
};
use crate::workflow::plan::model::{OperationNode, WorkflowPlan};
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;

impl WorkflowExecutor {
    pub(super) async fn create_root_tickets(
        &self,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        for node in &plan.nodes {
            if !node.depends_on().is_empty() || !node.depends_on_selected().is_empty() {
                continue;
            }
            self.create_node_ticket(plan, node, workflow_id, job_id, now)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn create_node_ticket(
        &self,
        plan: &WorkflowPlan,
        node: &OperationNode,
        workflow_id: &str,
        job_id: JobId,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let operation = node.operation();
        let branch = BranchContext {
            branch_id: "root".to_owned(),
            path: "/library/root.mkv".to_owned(),
            probe_codec: Some("h264".to_owned()),
            source_file: None,
        };
        let timing = seeded_timing(
            plan.seed,
            node.id(),
            &branch.branch_id,
            plan.timing.base_duration_ms,
            plan.timing.jitter_ms,
        );
        let rendered_payload = self
            .render_root_payload(plan, node, &branch, timing)
            .await?;
        let payload = WorkflowTicketPayload {
            workflow_id: workflow_id.to_owned(),
            plan_id: plan.id.clone(),
            node_id: node.id().to_owned(),
            branch_id: branch.branch_id.clone(),
            operation,
            rendered_payload,
            timing,
            source_file: None,
        }
        .to_ticket_payload()
        .map_err(|e| VoomError::Config(format!("workflow ticket payload encode: {e}")))?;
        let ticket = self
            .control_plane
            .create_ticket(NewTicket {
                job_id: Some(job_id),
                kind: ticket_kind(operation)?,
                priority: 0,
                payload,
                max_attempts: self.options.queue.max_attempts,
                created_at: now,
            })
            .await?;
        self.control_plane
            .mark_ready_if_unblocked(ticket.id, now)
            .await?;
        Ok(())
    }

    async fn render_root_payload(
        &self,
        plan: &WorkflowPlan,
        node: &OperationNode,
        branch: &BranchContext,
        timing: EffectiveTiming,
    ) -> Result<Value, VoomError> {
        let operation = node.operation();
        let roots = &self.options.artifact_roots;
        match operation {
            OperationKind::ScanLibrary => root_payload_result(render_default_payload_with_fan_out(
                operation,
                branch,
                timing,
                plan.fan_out.max_files,
            )),
            OperationKind::TranscodeVideo => match node.policy_target() {
                Some(target) => root_payload_result(render_policy_transcode_payload(
                    self.resolve_policy_file_source(target, "transcode_video")
                        .await?,
                    node.operation_payload(),
                    &roots.transcode.staging_root,
                    &roots.transcode.target_dir,
                    timing,
                )),
                None => root_payload_result(render_default_payload(operation, branch, timing)),
            },
            OperationKind::Remux => self.render_root_remux_payload(node, branch, timing).await,
            OperationKind::TranscodeAudio => match node.policy_target() {
                Some(target) => root_payload_result(render_policy_transcode_audio_payload(
                    self.resolve_policy_file_source(target, "transcode_audio")
                        .await?,
                    node.operation_payload(),
                    &roots.audio.staging_root,
                    &roots.audio.target_dir,
                    timing,
                )),
                None => root_payload_result(render_default_payload(operation, branch, timing)),
            },
            OperationKind::ExtractAudio => match node.policy_target() {
                Some(target) => root_payload_result(render_policy_extract_audio_payload(
                    self.resolve_policy_file_source(target, "extract_audio")
                        .await?,
                    node.operation_payload(),
                    &roots.audio.staging_root,
                    &roots.audio.target_dir,
                    timing,
                )),
                None => root_payload_result(render_default_payload(operation, branch, timing)),
            },
            _ => root_payload_result(render_default_payload(operation, branch, timing)),
        }
    }

    async fn render_root_remux_payload(
        &self,
        node: &OperationNode,
        branch: &BranchContext,
        timing: EffectiveTiming,
    ) -> Result<Value, VoomError> {
        match node.policy_target() {
            Some(
                target @ (voom_plan::TargetRef::FileVersion { .. }
                | voom_plan::TargetRef::FileLocation { .. }),
            ) => {
                let roots = &self.options.artifact_roots.remux;
                let rendered = render_policy_remux_payload(
                    self.resolve_policy_file_source(target, "remux").await?,
                    node.operation_payload(),
                    &roots.staging_root,
                    &roots.target_dir,
                    timing,
                );
                root_payload_result(rendered)
            }
            Some(target) => Err(root_payload_error(&BindingError::new(format!(
                "remux requires file_version or file_location target, got {target:?}"
            )))),
            None => {
                root_payload_result(render_default_payload(OperationKind::Remux, branch, timing))
            }
        }
    }

    async fn resolve_policy_file_source(
        &self,
        target: &voom_plan::TargetRef,
        operation_name: &str,
    ) -> Result<PolicyFileSource, VoomError> {
        match target {
            voom_plan::TargetRef::FileVersion { id } => Ok(PolicyFileSource {
                file_version_id: *id,
                location_id: None,
            }),
            voom_plan::TargetRef::FileLocation { id } => {
                let location = self
                    .control_plane
                    .identity
                    .get_file_location(*id)
                    .await?
                    .ok_or_else(|| VoomError::NotFound(format!("file_location {id}")))?;
                if location.retired_at.is_some() {
                    return Err(VoomError::Config(format!("file_location {id} is retired")));
                }
                Ok(PolicyFileSource {
                    file_version_id: location.file_version_id,
                    location_id: Some(*id),
                })
            }
            other => Err(VoomError::Config(format!(
                "{operation_name} requires file_version or file_location target, got {other:?}"
            ))),
        }
    }
}

fn root_payload_result(result: Result<Value, BindingError>) -> Result<Value, VoomError> {
    result.map_err(|error| root_payload_error(&error))
}

fn root_payload_error(error: &BindingError) -> VoomError {
    VoomError::Config(format!("workflow root payload binding: {error}"))
}

pub(super) fn parse_payload(ticket: &Ticket) -> Result<WorkflowTicketPayload, VoomError> {
    WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload.clone())
        .map_err(|e| VoomError::Config(format!("workflow ticket payload decode: {e}")))
}

fn ticket_kind(operation: OperationKind) -> Result<TicketOperation, VoomError> {
    TicketOperation::new(format!(
        "synthetic.workflow.operation.{}",
        operation.as_str()
    ))
}

/// Reports whether `node` lists `parent_id` among its direct dependencies.
///
/// Only `depends_on` (node ids) is consulted. `depends_on_selected` holds
/// dependency-*group* names resolved through [`OperationNode::provides_selected`],
/// not node ids, and no policy plan currently emits selected dependencies; their
/// completion gating is therefore left undefined here rather than guessed.
pub(super) fn depends_on_node(node: &OperationNode, parent_id: &str) -> bool {
    node.depends_on().iter().any(|id| id == parent_id)
}

/// Reports whether every direct dependency of `node` has a succeeded ticket. A
/// join node is created only once all of its parents are present in `succeeded`,
/// so the last parent to finish triggers creation exactly once.
pub(super) fn all_dependencies_succeeded(
    node: &OperationNode,
    succeeded: &HashSet<String>,
) -> bool {
    node.depends_on()
        .iter()
        .all(|dependency| succeeded.contains(dependency))
}
