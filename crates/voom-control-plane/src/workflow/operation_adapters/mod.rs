use std::future::Future;

use serde_json::Value;
use voom_core::{FileLocationId, FileVersionId, JobId, LeaseId, TicketId, VoomError};
use voom_store::repo::tickets::Ticket;
use voom_worker_protocol::OperationKind;

use crate::ControlPlane;
use crate::workflow::executor::WorkflowExecutorOptions;
use crate::workflow::runtime::WorkerRuntime;

#[cfg(test)]
pub(super) use crate::remux::workflow::dispatch_control_plane_remux;

pub(super) async fn dispatch_control_plane_ticket(
    control: &ControlPlane,
    runtime: &WorkerRuntime,
    ticket: &Ticket,
    operation: OperationKind,
    lease_id: LeaseId,
    payload: &Value,
    options: &WorkflowExecutorOptions,
) -> Option<Result<(), VoomError>> {
    payload.get("source_file_version_id")?;
    let context = OperationAdapterContext {
        control,
        runtime,
        ticket,
        lease_id,
        payload,
        options,
    };
    match operation {
        OperationKind::TranscodeVideo => {
            Some(crate::transcode::workflow::dispatch_control_plane_transcode(context).await)
        }
        OperationKind::Remux => {
            Some(crate::remux::workflow::dispatch_control_plane_remux_context(context).await)
        }
        OperationKind::TranscodeAudio => {
            Some(crate::audio::workflow::dispatch_control_plane_transcode_audio(context).await)
        }
        OperationKind::ExtractAudio => {
            Some(crate::audio::workflow::dispatch_control_plane_extract_audio(context).await)
        }
        _ => None,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct OperationAdapterContext<'a> {
    pub(crate) control: &'a ControlPlane,
    pub(crate) runtime: &'a WorkerRuntime,
    pub(crate) ticket: &'a Ticket,
    pub(crate) lease_id: LeaseId,
    pub(crate) payload: &'a Value,
    pub(crate) options: &'a WorkflowExecutorOptions,
}

impl<'a> OperationAdapterContext<'a> {
    pub(crate) fn runtime_dispatch_context(self) -> RuntimeDispatchContext<'a> {
        RuntimeDispatchContext {
            control: self.control,
            runtime: self.runtime,
            ticket_id: self.ticket.id,
            lease_id: self.lease_id,
            options: self.options,
        }
    }

    pub(crate) fn job_id(self, operation: &str) -> Result<JobId, VoomError> {
        self.ticket.job_id.ok_or_else(|| {
            VoomError::Config(format!(
                "{operation} ticket {} missing job_id",
                self.ticket.id
            ))
        })
    }

    pub(crate) fn source_file_version_id(self) -> Result<FileVersionId, VoomError> {
        Ok(FileVersionId(required_u64(
            self.payload,
            "source_file_version_id",
        )?))
    }

    pub(crate) fn source_location_id(self) -> Option<FileLocationId> {
        optional_u64(self.payload, "source_location_id").map(FileLocationId)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RuntimeDispatchContext<'a> {
    pub(crate) control: &'a ControlPlane,
    pub(crate) runtime: &'a WorkerRuntime,
    pub(crate) ticket_id: TicketId,
    pub(crate) lease_id: LeaseId,
    pub(crate) options: &'a WorkflowExecutorOptions,
}

pub(crate) async fn await_with_lease_heartbeats<F, T>(
    context: RuntimeDispatchContext<'_>,
    operation: OperationKind,
    future: F,
) -> Result<T, VoomError>
where
    F: Future<Output = Result<T, VoomError>>,
{
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + context.options.heartbeat_interval,
        context.options.heartbeat_interval,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = heartbeat.tick(), if !context.options.chaos.suppresses_heartbeats_for(operation) => {
                crate::workflow::leases::heartbeat_lease_with_retry(
                    context.control,
                    context.lease_id,
                    crate::workflow::leases::time_duration(context.options.lease_ttl)?,
                )
                .await?;
            }
        }
    }
}

pub(crate) fn workflow_idempotency_key(ticket_id: TicketId, lease_id: LeaseId) -> String {
    format!("ticket-{}-lease-{}", ticket_id.0, lease_id.0)
}

fn required_u64(payload: &Value, field: &str) -> Result<u64, VoomError> {
    payload
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| VoomError::Config(format!("workflow payload missing `{field}`")))
}

fn optional_u64(payload: &Value, field: &str) -> Option<u64> {
    payload.get(field).and_then(Value::as_u64)
}
