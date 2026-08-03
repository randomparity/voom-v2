//! Workflow failure classification and retry scheduling, plus the
//! `WorkflowRunError` surface and small sqlite/time conversion helpers shared
//! across the executor's children.

use std::time::Duration;

use time::OffsetDateTime;
use voom_core::{FailureClass, JobId, TicketId, VoomError};
use voom_events::Event;

use crate::workflow::execution::executor::WorkflowExecutor;
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;
use crate::workflow::summary::WorkflowRunSummary;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkflowFailureDisposition {
    IsolatedTicket,
    Fatal,
}

#[derive(Debug)]
pub struct WorkflowRunError {
    pub summary: WorkflowRunSummary,
    pub source: VoomError,
    pub(crate) job_failed: bool,
    pub(crate) disposition: WorkflowFailureDisposition,
    pub(crate) dispatch_started: bool,
}

impl WorkflowExecutor {
    pub(super) async fn first_failed_ticket_error(
        &self,
        job_id: JobId,
        workflow_id: &str,
    ) -> Result<Option<VoomError>, VoomError> {
        let Some(ticket) = self
            .control_plane
            .tickets
            .first_failed_workflow_ticket(job_id, workflow_id)
            .await?
        else {
            return Ok(None);
        };
        let workflow_payload =
            WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload).map_err(
                |e| {
                    VoomError::Internal(format!(
                        "workflow failed ticket {} payload decode: {e}",
                        ticket.id
                    ))
                },
            )?;
        Ok(Some(VoomError::Internal(format!(
            "workflow ticket {} failed",
            workflow_payload.node_id
        ))))
    }

    pub(super) async fn ticket_failure_class(
        &self,
        ticket_id: TicketId,
    ) -> Result<Option<FailureClass>, VoomError> {
        let Some(event) = self
            .control_plane
            .events
            .latest_ticket_failure(ticket_id)
            .await?
        else {
            return Ok(None);
        };
        match event.payload {
            Event::TicketFailedTerminal(payload) => Ok(Some(payload.class)),
            Event::TicketFailedRetriable(payload) => Ok(Some(payload.class)),
            other => Err(VoomError::Internal(format!(
                "latest failure event for {ticket_id} had unexpected kind {:?}",
                other.kind()
            ))),
        }
    }

    pub(super) async fn retry_delay(
        &self,
        job_id: JobId,
        workflow_id: &str,
        now: OffsetDateTime,
    ) -> Result<Option<Duration>, VoomError> {
        let Some(next_eligible) = self
            .control_plane
            .tickets
            .retry_eligible_at(job_id, workflow_id, now)
            .await?
        else {
            return Ok(None);
        };
        let wait = next_eligible - now;
        Duration::try_from(wait)
            .map(Some)
            .map_err(|e| VoomError::Internal(format!("workflow retry delay for {job_id}: {e}")))
    }
}

pub(super) fn selector_failure_class(source: &VoomError) -> Result<FailureClass, VoomError> {
    match source {
        VoomError::NoEligibleWorker(_) => Ok(FailureClass::NoEligibleWorker),
        VoomError::AmbiguousWorkerSelection(_) => Ok(FailureClass::AmbiguousWorkerSelection),
        other => Err(VoomError::Internal(format!(
            "selector returned unsupported workflow error: {other}"
        ))),
    }
}
