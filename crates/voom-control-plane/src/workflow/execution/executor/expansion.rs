//! Post-success ticket expansion and the workflow state queries (succeeded
//! node ids, ticket existence, ready tickets, finished check) that drive the
//! run loop.

use std::collections::HashSet;

use voom_core::{JobId, TicketId, VoomError};
use voom_store::repo::execution::tickets::Ticket;

use crate::cases::{begin_tx, commit_tx};
use crate::workflow::execution::executor::tickets::{
    all_dependencies_succeeded, depends_on_node, parse_payload,
};
use crate::workflow::execution::executor::{WorkflowExecutor, WorkflowIdleState};
use crate::workflow::plan::expansion::{
    ExpansionContext, expand_backup_completion, expand_probe_completion, expand_quality_completion,
    expand_scanner_completion, expand_transform_completion,
};
use crate::workflow::plan::model::WorkflowPlan;
use crate::workflow::plan::policy_bridge::is_policy_workflow_node_id;
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;

impl WorkflowExecutor {
    pub(super) async fn expand_successful_ticket(
        &self,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        ticket_id: TicketId,
    ) -> Result<(), VoomError> {
        let ticket = self
            .control_plane
            .tickets
            .get(ticket_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("ticket {ticket_id}")))?;
        let payload = parse_payload(&ticket)?;
        let ctx = ExpansionContext::new(
            &self.control_plane,
            plan,
            workflow_id,
            &plan.id,
            job_id,
            self.control_plane.clock().now(),
        );
        match payload.node_id.as_str() {
            "scan" => {
                expand_scanner_completion(&ctx, &ticket).await?;
            }
            "probe" => {
                expand_probe_completion(&ctx, &payload.branch_id, &ticket).await?;
            }
            "quality" => {
                expand_quality_completion(&ctx, &payload.branch_id, &ticket).await?;
            }
            "remux" | "transcode" => {
                expand_transform_completion(&ctx, &payload.branch_id, &ticket).await?;
            }
            "backup" => {
                expand_backup_completion(&ctx, &payload.branch_id, &ticket).await?;
            }
            node_id if is_policy_workflow_node_id(node_id) => {
                self.expand_policy_node_completion(plan, workflow_id, job_id, node_id)
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Dynamically expands the dependents of a just-succeeded policy-bridge node.
    ///
    /// Policy plan nodes can be arbitrary DAGs whose
    /// edges are declared via [`crate::workflow::plan::model::OperationNode::depends_on`]. Workflow tickets do not
    /// use the store's declarative dependency table, so each downstream node's
    /// ticket must be created here once all of its parents have succeeded.
    async fn expand_policy_node_completion(
        &self,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        completed_node_id: &str,
    ) -> Result<(), VoomError> {
        let succeeded = self.succeeded_node_ids(job_id, workflow_id).await?;
        let now = self.control_plane.clock().now();
        for node in &plan.nodes {
            if !depends_on_node(node, completed_node_id) {
                continue;
            }
            if self
                .node_ticket_exists(job_id, workflow_id, node.id())
                .await?
            {
                continue;
            }
            if !all_dependencies_succeeded(node, &succeeded) {
                continue;
            }
            self.create_node_ticket(plan, node, workflow_id, job_id, now)
                .await?;
        }
        Ok(())
    }

    /// Returns the set of node ids whose tickets are in the `succeeded` state for
    /// this workflow. Used to decide whether a join node's parents have all
    /// completed.
    async fn succeeded_node_ids(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<HashSet<String>, VoomError> {
        Ok(self
            .control_plane
            .tickets
            .succeeded_workflow_node_ids(job_id, workflow_id)
            .await?
            .into_iter()
            .collect())
    }

    /// Reports whether a ticket already exists for the given node id in this
    /// workflow, in any state. Guards against creating duplicate tickets for a
    /// join node when more than one parent succeeds.
    async fn node_ticket_exists(
        &self,
        job_id: JobId,
        workflow_id: &str,
        node_id: &str,
    ) -> Result<bool, VoomError> {
        let mut tx = begin_tx(&self.control_plane.pool).await?;
        let exists = self
            .control_plane
            .tickets
            .workflow_ticket_exists_in_tx(&mut tx, job_id, workflow_id, "root", node_id)
            .await?;
        commit_tx(tx).await?;
        Ok(exists)
    }

    pub(super) async fn ready_workflow_tickets(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<Vec<Ticket>, VoomError> {
        let tickets = self
            .control_plane
            .tickets
            .ready_workflow_tickets(
                job_id,
                workflow_id,
                self.control_plane.clock().now(),
                self.options.queue.ready_batch_size,
            )
            .await?;
        // Skip an undecodable ticket rather than raising, so one bad row cannot
        // stall dispatch of every well-formed ticket beside it in the batch. This
        // is the same containment ADR 0068 applies at
        // `remote_acquire_candidates_in_tx`, where a candidate is scored
        // ineligible instead of failing acquisition for the whole set — and it
        // matters here for the same reason it matters there, now that the
        // declaration requirement makes a decode failure the expected shape of an
        // undrained pre-upgrade row rather than a sign of corruption.
        //
        // The skip is recorded and not counted: `branch_count` and `ticket_count`
        // are durable summary columns, so a visible counter would need a column
        // and a migration this slice forbids, and an in-memory one would leave the
        // persisted row exactly as inconsistent while adding a `merge_invocation`
        // rule a later resume can get wrong.
        let mut ready = Vec::with_capacity(tickets.len());
        let mut undecodable = Vec::new();
        for ticket in tickets {
            match WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload.clone())
            {
                Ok(_) => ready.push(ticket),
                Err(error) => {
                    tracing::warn!(
                        job_id = job_id.0,
                        workflow_id,
                        ticket_id = ticket.id.0,
                        ticket_kind = ticket.kind.as_str(),
                        %error,
                        "skipping a ready workflow ticket whose payload did not decode"
                    );
                    undecodable.push(format!("ticket {}: {error}", ticket.id.0));
                }
            }
        }
        // Raise only once nothing else can proceed. Skipping alone would not
        // terminate: an undecodable ticket stays `ready`, so `workflow_idle_state`
        // keeps reporting `Ready`, `wait_or_fail_idle` keeps returning `Ok`, and
        // the run spins on a batch that filters to empty. Trading a loud abort for
        // a livelock is worse than the behavior being fixed.
        //
        // Failing the ticket terminally instead — so it leaves `ready` and opens
        // an ADR 0018 issue — needs a terminal transition that does not require a
        // lease, which does not exist yet. #486 owns it. Until then this is where
        // the run stops, and the message names the tickets so an operator can drain
        // exactly them.
        if ready.is_empty() && !undecodable.is_empty() {
            return Err(VoomError::Internal(format!(
                "workflow ready tickets for {job_id}: no dispatchable ticket remains after \
                 payload decode failures; drain these tickets (see the ADR 0068 upgrade step \
                 in docs/release-process.md): {}",
                undecodable.join("; ")
            )));
        }
        Ok(ready)
    }

    pub(super) async fn workflow_finished(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<bool, VoomError> {
        let facts = self
            .control_plane
            .tickets
            .workflow_ticket_facts(job_id, workflow_id)
            .await?;
        Ok(facts.unfinished == 0)
    }

    pub(super) async fn workflow_idle_state(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<WorkflowIdleState, VoomError> {
        let facts = self
            .control_plane
            .tickets
            .workflow_ticket_facts(job_id, workflow_id)
            .await?;
        if facts.unfinished == 0 {
            Ok(WorkflowIdleState::Finished)
        } else if facts.ready > 0 {
            Ok(WorkflowIdleState::Ready)
        } else if facts.leased > 0 {
            Ok(WorkflowIdleState::Leased)
        } else {
            Ok(WorkflowIdleState::Blocked)
        }
    }
}
