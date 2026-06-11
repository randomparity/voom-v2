//! Workflow failure classification and retry scheduling, plus the
//! `WorkflowRunError` surface and small sqlite/time conversion helpers shared
//! across the executor's children.

use std::time::Duration;

use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;
use voom_core::{FailureClass, JobId, TicketId, VoomError};

use crate::workflow::execution::executor::WorkflowExecutor;
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;
use crate::workflow::summary::WorkflowRunSummary;

#[derive(Debug)]
pub struct WorkflowRunError {
    pub summary: WorkflowRunSummary,
    pub source: VoomError,
}

impl WorkflowExecutor {
    pub(super) async fn first_failed_ticket_error(
        &self,
        job_id: JobId,
    ) -> Result<Option<VoomError>, VoomError> {
        let row = sqlx::query(
            "SELECT id, kind, payload FROM tickets \
             WHERE job_id = ? AND state = 'failed' ORDER BY id ASC LIMIT 1",
        )
        .bind(sqlite_i64(job_id.0))
        .fetch_optional(&self.control_plane.pool)
        .await
        .map_err(|e| {
            VoomError::database_context(format!("workflow failed ticket for {job_id}"), e)
        })?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id: i64 = row
            .try_get("id")
            .map_err(|e| VoomError::database_context("workflow failed ticket id", e))?;
        let ticket_id = TicketId(sqlite_u64(id));
        let kind: String = row.try_get("kind").map_err(|e| {
            VoomError::database_context(format!("workflow failed ticket {ticket_id} kind"), e)
        })?;
        let payload: String = row.try_get("payload").map_err(|e| {
            VoomError::database_context(format!("workflow failed ticket {ticket_id} payload"), e)
        })?;
        let payload: Value = serde_json::from_str(&payload).map_err(|e| {
            VoomError::Internal(format!(
                "workflow failed ticket {ticket_id} payload JSON: {e}"
            ))
        })?;
        let workflow_payload =
            WorkflowTicketPayload::parse_ticket(&kind, payload).map_err(|e| {
                VoomError::Internal(format!(
                    "workflow failed ticket {ticket_id} payload decode: {e}"
                ))
            })?;
        Ok(Some(VoomError::Internal(format!(
            "workflow ticket {} failed",
            workflow_payload.node_id
        ))))
    }

    pub(super) async fn ticket_failure_class(
        &self,
        ticket_id: TicketId,
    ) -> Result<Option<FailureClass>, VoomError> {
        let row = sqlx::query(
            "SELECT event_id, payload FROM events \
             WHERE kind IN ('ticket.failed_terminal', 'ticket.failed_retriable') \
               AND subject_type = 'ticket' \
               AND subject_id = ? \
             ORDER BY event_id DESC LIMIT 1",
        )
        .bind(sqlite_i64(ticket_id.0))
        .fetch_optional(&self.control_plane.pool)
        .await
        .map_err(|e| {
            VoomError::database_context(format!("workflow failure event for {ticket_id}"), e)
        })?;
        let Some(row) = row else {
            return Ok(None);
        };
        let event_id: i64 = row.try_get("event_id").map_err(|e| {
            VoomError::database_context(format!("workflow failure event id for {ticket_id}"), e)
        })?;
        let payload: String = row.try_get("payload").map_err(|e| {
            VoomError::database(format!(
                "workflow failure event {event_id} payload for {ticket_id}: {e}"
            ))
        })?;
        let payload: Value = serde_json::from_str(&payload).map_err(|e| {
            VoomError::Internal(format!(
                "workflow failure event {event_id} payload JSON for {ticket_id}: {e}"
            ))
        })?;
        let class = payload.get("class").ok_or_else(|| {
            VoomError::Internal(format!(
                "workflow failure event {event_id} for {ticket_id} missing class"
            ))
        })?;
        serde_json::from_value(class.clone())
            .map(Some)
            .map_err(|e| {
                VoomError::Internal(format!(
                    "workflow failure event {event_id} class for {ticket_id}: {e}"
                ))
            })
    }

    pub(super) async fn retry_delay(
        &self,
        job_id: JobId,
        workflow_id: &str,
        now: OffsetDateTime,
    ) -> Result<Option<Duration>, VoomError> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT MIN(next_eligible_at) FROM tickets \
             WHERE job_id = ? \
               AND state = 'ready' \
               AND next_eligible_at > ? \
               AND json_extract(payload, '$.workflow_id') = ?",
        )
        .bind(sqlite_i64(job_id.0))
        .bind(format_time(now)?)
        .bind(workflow_id)
        .fetch_optional(&self.control_plane.pool)
        .await
        .map_err(|e| {
            VoomError::database_context(format!("workflow retry delay for {job_id}"), e)
        })?;
        let Some((Some(next_eligible),)) = row else {
            return Ok(None);
        };
        let next_eligible = OffsetDateTime::parse(
            &next_eligible,
            &time::format_description::well_known::Iso8601::DEFAULT,
        )
        .map_err(|e| {
            VoomError::Internal(format!("workflow retry delay timestamp for {job_id}: {e}"))
        })?;
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

pub(super) fn format_time(t: OffsetDateTime) -> Result<String, VoomError> {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::Internal(format!("format iso8601: {e}")))
}

pub(super) fn sqlite_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(super) fn sqlite_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub(super) fn sqlite_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}
