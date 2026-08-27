//! Root and node ticket creation, root-payload rendering for each operation
//! kind, and the small ticket/dependency helpers the expansion and spawn
//! children share.

use std::collections::HashSet;

use serde_json::Value;
use time::OffsetDateTime;
use voom_core::OperationKind;
use voom_core::{JobId, TicketOperation, VoomError, WORKFLOW_OPERATION_NAMESPACE};
use voom_store::repo::execution::tickets::{NewTicket, Ticket};
use voom_store::repo::media::identity::{FileLocationRepo, FileVersionRepo};

use crate::cases::commit_tx;
use crate::operation_source::{require_live_rooted, select_location};
use crate::workflow::execution::executor::{PlannedLineageGuard, WorkflowExecutor};
use crate::workflow::execution::timing::{EffectiveTiming, seeded_timing};
use crate::workflow::plan::access_declaration::{TicketStorageSource, declaration_for};
use crate::workflow::plan::binding::{
    BindingError, BranchContext, PolicyFileSource, render_default_payload,
    render_default_payload_with_fan_out, render_policy_extract_audio_payload,
    render_policy_remux_payload, render_policy_transcode_audio_payload,
    render_policy_transcode_payload, render_policy_verify_artifact_payload,
};
use crate::workflow::plan::envelope;
use crate::workflow::plan::model::{OperationNode, WorkflowPlan};
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;
use voom_store::tx::{begin_serialized_read, begin_write_first};

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

    /// Create root tickets behind the planned-lineage staleness guard.
    ///
    /// The guard is a precondition on dispatch, so it runs FIRST: a prepared
    /// run whose active versions were superseded must be rejected with
    /// `STALE_IDENTITY_EVIDENCE` before any ticket work happens. Envelope
    /// rendering during `render_node_ticket` can fail with configuration
    /// errors of its own; letting it precede the guard would misreport a
    /// superseded run as `CONFIG_INVALID` and hide the replan-needed cause.
    pub(super) async fn create_guarded_root_tickets(
        &self,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        now: OffsetDateTime,
        lineage_guard: &PlannedLineageGuard,
    ) -> Result<(), VoomError> {
        // Serialized, not a plain read: the guard must see what an in-flight
        // promoter commits. A WAL reader that does not wait for it passes on
        // the pre-promotion snapshot and dispatches a superseded run.
        let mut guard_tx = begin_serialized_read(
            &self.control_plane.pool,
            "tickets: create_guarded_root_tickets",
        )
        .await?;
        self.control_plane
            .identity
            .require_active_file_versions_in_tx(&mut guard_tx, lineage_guard.expectations())
            .await?;
        commit_tx(guard_tx).await?;

        let mut inputs = Vec::new();
        for node in &plan.nodes {
            if node.depends_on().is_empty() && node.depends_on_selected().is_empty() {
                inputs.push(
                    self.render_node_ticket(plan, node, workflow_id, job_id, now)
                        .await?,
                );
            }
        }

        let mut tx = begin_write_first(
            &self.control_plane.pool,
            "tickets: create_guarded_root_tickets",
        )
        .await?;
        for input in inputs {
            let ticket = self
                .control_plane
                .create_ticket_in_tx(&mut tx, input)
                .await?;
            let promoted = self
                .control_plane
                .mark_ready_if_unblocked_in_tx(&mut tx, ticket.id, now)
                .await?;
            if promoted.len() != 1 {
                return Err(VoomError::Internal(format!(
                    "workflow root ticket {} was not promoted to ready",
                    ticket.id
                )));
            }
        }
        commit_tx(tx).await
    }

    pub(super) async fn create_node_ticket(
        &self,
        plan: &WorkflowPlan,
        node: &OperationNode,
        workflow_id: &str,
        job_id: JobId,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let input = self
            .render_node_ticket(plan, node, workflow_id, job_id, now)
            .await?;
        let ticket = self.control_plane.create_ticket(input).await?;
        self.control_plane
            .mark_ready_if_unblocked(ticket.id, now)
            .await?;
        Ok(())
    }

    async fn render_node_ticket(
        &self,
        plan: &WorkflowPlan,
        node: &OperationNode,
        workflow_id: &str,
        job_id: JobId,
        now: OffsetDateTime,
    ) -> Result<NewTicket, VoomError> {
        let operation = node.operation();
        // Resolved once, here, and handed to every renderer. Two independent
        // resolutions of the same target within one render could disagree if a
        // rescan landed between them, leaving the declaration and
        // `source_location_id` naming different locations. Unconditional on
        // byte-touching: a non-byte-touching node's payload is what its
        // byte-touching children thread from.
        let policy_source = match node.policy_target() {
            Some(target) => Some(
                self.resolve_policy_file_source(target, operation.as_str())
                    .await?,
            ),
            None => None,
        };
        let branch = BranchContext {
            branch_id: "root".to_owned(),
            path: "/library/root.mkv".to_owned(),
            probe_codec: Some("h264".to_owned()),
            source_file: None,
            storage_source: policy_source.map(|source| TicketStorageSource::Location {
                storage_root_id: source.storage_root_id,
                file_location_id: source.location_id,
            }),
        };
        let timing = seeded_timing(
            plan.seed,
            node.id(),
            &branch.branch_id,
            plan.timing.base_duration_ms,
            plan.timing.jitter_ms,
        );
        let mut rendered_payload =
            Self::render_root_payload(plan, node, &branch, policy_source, timing)?;
        // ADR 0075 flip: byte-touching media tickets whose planning inputs are
        // fully derivable carry a handle-shaped dispatch envelope and route to
        // their storage owner's agent instead of the bundled adapters.
        if operation.is_node_local_media_dispatch()
            && let Some(source) = policy_source.as_ref()
            && let Some(media_dispatch) = envelope::policy_envelope(
                &self.control_plane,
                &branch.branch_id,
                operation,
                source,
                node.operation_payload(),
            )
            .await?
            && let Some(object) = rendered_payload.as_object_mut()
        {
            object.insert("media_dispatch".to_owned(), media_dispatch);
        }
        let declared_artifact_access = declaration_for(operation, branch.storage_source.as_ref())?;
        let payload = WorkflowTicketPayload {
            workflow_id: workflow_id.to_owned(),
            plan_id: plan.id.clone(),
            node_id: node.id().to_owned(),
            branch_id: branch.branch_id.clone(),
            operation,
            rendered_payload,
            timing,
            source_file: None,
            declared_artifact_access,
        }
        .to_ticket_payload()
        .map_err(|e| VoomError::Config(format!("workflow ticket payload encode: {e}")))?;
        Ok(NewTicket {
            job_id: Some(job_id),
            kind: ticket_kind(operation)?,
            priority: 0,
            payload,
            max_attempts: self.options.queue.max_attempts,
            created_at: now,
        })
    }

    fn render_root_payload(
        plan: &WorkflowPlan,
        node: &OperationNode,
        branch: &BranchContext,
        policy_source: Option<PolicyFileSource>,
        timing: EffectiveTiming,
    ) -> Result<Value, VoomError> {
        let operation = node.operation();
        match (operation, policy_source) {
            (OperationKind::ScanLibrary, _) => {
                root_payload_result(render_default_payload_with_fan_out(
                    operation,
                    branch,
                    timing,
                    plan.fan_out.max_files,
                ))
            }
            (OperationKind::TranscodeVideo, Some(source)) => root_payload_result(
                render_policy_transcode_payload(source, node.operation_payload(), timing),
            ),
            (OperationKind::Remux, Some(source)) => root_payload_result(
                render_policy_remux_payload(source, node.operation_payload(), timing),
            ),
            (OperationKind::TranscodeAudio, Some(source)) => root_payload_result(
                render_policy_transcode_audio_payload(source, node.operation_payload(), timing),
            ),
            (OperationKind::ExtractAudio, Some(source)) => root_payload_result(
                render_policy_extract_audio_payload(source, node.operation_payload(), timing),
            ),
            (OperationKind::VerifyArtifact, Some(source)) => {
                root_payload_result(render_policy_verify_artifact_payload(source, timing))
            }
            _ => root_payload_result(render_default_payload(operation, branch, timing)),
        }
    }

    /// Resolve a node's policy target to the one live rooted location it names.
    ///
    /// A `FileVersion` routes through `select_location`, which picks its single
    /// live rooted location. A `FileLocation` names the row directly, so it is
    /// read once and checked in place: routing it back through `select_location`
    /// would re-read the same row to compare its `file_version_id` against a value
    /// taken from that row.
    async fn resolve_policy_file_source(
        &self,
        target: &voom_plan::TargetRef,
        operation_name: &str,
    ) -> Result<PolicyFileSource, VoomError> {
        let location = match target {
            voom_plan::TargetRef::FileVersion { id } => {
                select_location(&self.control_plane, *id, None).await?
            }
            voom_plan::TargetRef::FileLocation { id } => {
                let location = self
                    .control_plane
                    .identity
                    .get_file_location(*id)
                    .await?
                    .ok_or_else(|| VoomError::NotFound(format!("file_location {id}")))?;
                require_live_rooted(&location)?;
                location
            }
            other => {
                return Err(VoomError::Config(format!(
                    "{operation_name} requires file_version or file_location target, got {other:?}"
                )));
            }
        };
        // Both arms have already rejected a non-rooted address, so this arm is
        // unreachable — propagated rather than expected, because that is an
        // argument about callers, not a type-level guarantee.
        let (storage_root_id, _) = location.rooted_address()?;
        Ok(PolicyFileSource {
            file_version_id: location.file_version_id,
            storage_root_id,
            location_id: location.id,
        })
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
        "{WORKFLOW_OPERATION_NAMESPACE}{}",
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
