//! Post-success ticket expansion and the workflow state queries (succeeded
//! node ids, ticket existence, ready tickets, finished check) that drive the
//! run loop.

use std::collections::HashSet;

use sqlx::Row;
use voom_core::{JobId, TicketId, VoomError};
use voom_store::repo::tickets::Ticket;

use crate::workflow::execution::executor::WorkflowExecutor;
use crate::workflow::execution::executor::errors::{format_time, sqlite_i64, sqlite_u64};
use crate::workflow::execution::executor::tickets::{
    all_dependencies_succeeded, depends_on_node, parse_payload,
};
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
        let rows = sqlx::query(
            "SELECT json_extract(payload, '$.node_id') AS node_id FROM tickets \
             WHERE job_id = ? \
               AND state = 'succeeded' \
               AND json_extract(payload, '$.workflow_id') = ?",
        )
        .bind(sqlite_i64(job_id.0))
        .bind(workflow_id)
        .fetch_all(&self.control_plane.pool)
        .await
        .map_err(|e| VoomError::database_context("workflow succeeded node ids", e))?;
        let mut node_ids = HashSet::new();
        for row in rows {
            let node_id: Option<String> = row
                .try_get("node_id")
                .map_err(|e| VoomError::database_context("succeeded node id row", e))?;
            if let Some(node_id) = node_id {
                node_ids.insert(node_id);
            }
        }
        Ok(node_ids)
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
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM tickets \
             WHERE job_id = ? \
               AND json_extract(payload, '$.workflow_id') = ? \
               AND json_extract(payload, '$.node_id') = ?",
        )
        .bind(sqlite_i64(job_id.0))
        .bind(workflow_id)
        .bind(node_id)
        .fetch_one(&self.control_plane.pool)
        .await
        .map_err(|e| VoomError::database_context("workflow node ticket exists", e))?;
        Ok(count > 0)
    }

    pub(super) async fn ready_workflow_tickets(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<Vec<Ticket>, VoomError> {
        let now = format_time(self.control_plane.clock().now())?;
        let rows = sqlx::query(
            "SELECT id FROM tickets \
             WHERE job_id = ? \
               AND state = 'ready' \
               AND next_eligible_at <= ? \
               AND json_extract(payload, '$.workflow_id') = ? \
             ORDER BY priority DESC, next_eligible_at ASC, id ASC \
             LIMIT ?",
        )
        .bind(sqlite_i64(job_id.0))
        .bind(now)
        .bind(workflow_id)
        .bind(i64::from(self.options.queue.ready_batch_size))
        .fetch_all(&self.control_plane.pool)
        .await
        .map_err(|e| {
            VoomError::database_context(format!("workflow ready tickets for {job_id}"), e)
        })?;
        let mut tickets = Vec::with_capacity(rows.len());
        for row in rows {
            let id: i64 = row
                .try_get("id")
                .map_err(|e| VoomError::database_context("workflow ready ticket id", e))?;
            let ticket_id = TicketId(sqlite_u64(id));
            let ticket = self
                .control_plane
                .tickets
                .get(ticket_id)
                .await
                .map_err(|e| {
                    VoomError::database(format!(
                        "load workflow ready ticket {ticket_id} for {job_id}: {e}"
                    ))
                })?
                .ok_or_else(|| {
                    VoomError::NotFound(format!("workflow ready ticket {ticket_id} for {job_id}"))
                })?;
            WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload.clone())
                .map_err(|e| {
                    VoomError::Internal(format!(
                        "workflow ready tickets for {job_id}: ticket {} payload decode: {e}",
                        ticket.id
                    ))
                })?;
            tickets.push(ticket);
        }
        Ok(tickets)
    }

    pub(super) async fn workflow_finished(&self, job_id: JobId) -> Result<bool, VoomError> {
        let (unfinished,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM tickets \
             WHERE job_id = ? AND state IN ('pending', 'ready', 'leased')",
        )
        .bind(sqlite_i64(job_id.0))
        .fetch_one(&self.control_plane.pool)
        .await
        .map_err(|e| {
            VoomError::database_context(format!("workflow unfinished tickets for {job_id}"), e)
        })?;
        Ok(unfinished == 0)
    }
}
