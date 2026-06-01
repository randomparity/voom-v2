use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;
use voom_core::{ErrorCode, FailureClass, LeaseId, TicketId, VoomError, WorkerId};
use voom_store::repo::tickets::Ticket;
use voom_worker_protocol::{
    DispatchStream, NdjsonOutcome, OperationKind, OperationRequest, ProgressFrame, ProtocolError,
};

use super::executor::{WorkflowChaosOptions, WorkflowExecutorOptions};
use super::leases::{
    fail_if_watchdog_elapsed, fail_lease_and_return, failure_class_for_error,
    heartbeat_workflow_lease, release_lease_with_retry,
};
use super::operation_adapters::dispatch_control_plane_ticket;
use super::runtime::WorkerRuntime;
use crate::ControlPlane;
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;

#[derive(Debug)]
pub(super) struct DispatchOutcome {
    pub(super) ticket_id: TicketId,
    pub(super) worker_id: WorkerId,
    pub(super) operation: OperationKind,
    pub(super) terminal: DispatchTerminal,
}

#[derive(Debug)]
pub(super) enum DispatchTerminal {
    Success,
    Failure { source: VoomError },
}

pub(super) async fn dispatch_ticket(
    control: ControlPlane,
    runtime: WorkerRuntime,
    ticket: Ticket,
    workflow_payload: WorkflowTicketPayload,
    lease_id: LeaseId,
    options: WorkflowExecutorOptions,
) -> DispatchOutcome {
    let worker_id = runtime.credentials.worker_id;
    let operation = workflow_payload.operation;
    let terminal = match dispatch_ticket_inner(
        &control,
        &runtime,
        &ticket,
        &workflow_payload,
        lease_id,
        options,
    )
    .await
    {
        Ok(()) => DispatchTerminal::Success,
        Err(source) => DispatchTerminal::Failure { source },
    };
    DispatchOutcome {
        ticket_id: ticket.id,
        worker_id,
        operation,
        terminal,
    }
}

async fn dispatch_ticket_inner(
    control: &ControlPlane,
    runtime: &WorkerRuntime,
    ticket: &Ticket,
    workflow_payload: &WorkflowTicketPayload,
    lease_id: LeaseId,
    options: WorkflowExecutorOptions,
) -> Result<(), VoomError> {
    let mut payload = workflow_payload.rendered_payload.clone();
    apply_chaos_payload_override(&mut payload, workflow_payload.operation, &options.chaos)?;
    if let Some(result) = dispatch_control_plane_ticket(
        control,
        runtime,
        ticket,
        workflow_payload.operation,
        lease_id,
        &payload,
        &options,
    )
    .await
    {
        return result;
    }
    let request = OperationRequest {
        operation: workflow_payload.operation,
        lease_id,
        payload,
        heartbeat_deadline_ms: duration_millis_u32(options.heartbeat_timeout),
        progress_idle_deadline_ms: duration_millis_u32(options.progress_idle_timeout),
    };
    let idempotency_key = format!("ticket-{}-lease-{}", ticket.id.0, lease_id.0);
    let dispatch_timeout = no_response_timeout(&options);
    let dispatch = tokio::time::timeout(
        dispatch_timeout,
        runtime
            .client
            .dispatch(&runtime.credentials, &idempotency_key, request),
    )
    .await
    .map_err(|_| {
        VoomError::WorkerTimeout(format!(
            "dispatch response timeout for lease {lease_id} after {dispatch_timeout:?}"
        ))
    })
    .and_then(|result| result.map_err(|err| map_dispatch_setup_protocol_error(&err)));
    let dispatch = match dispatch {
        Ok(dispatch) => dispatch,
        Err(source) => {
            return fail_lease_and_return(
                control,
                lease_id,
                failure_class_for_error(&source),
                source,
            )
            .await;
        }
    };
    if dispatch.response.lease_id != lease_id {
        return fail_lease_and_return(
            control,
            lease_id,
            FailureClass::MalformedWorkerResult,
            VoomError::MalformedWorkerResult(format!(
                "worker accepted lease {:?} for expected {:?}",
                dispatch.response.lease_id, lease_id
            )),
        )
        .await;
    }
    consume_dispatch_stream(
        control,
        lease_id,
        workflow_payload.operation,
        dispatch,
        options,
    )
    .await
}

async fn consume_dispatch_stream(
    control: &ControlPlane,
    lease_id: LeaseId,
    operation: OperationKind,
    mut dispatch: DispatchStream,
    options: WorkflowExecutorOptions,
) -> Result<(), VoomError> {
    let mut last_progress = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut heartbeat = tokio::time::interval(options.heartbeat_interval);
    loop {
        let progress_deadline = sleep_until(last_progress + options.progress_idle_timeout);
        let heartbeat_deadline = sleep_until(last_heartbeat + options.heartbeat_timeout);
        tokio::pin!(progress_deadline);
        tokio::pin!(heartbeat_deadline);
        tokio::select! {
            biased;
            frame = dispatch.frames.next_frame() => {
                match frame {
                    Ok(NdjsonOutcome::Frame(frame)) => {
                        validate_frame_lease(&frame, lease_id)?;
                        fail_if_watchdog_elapsed(
                            control,
                            lease_id,
                            last_heartbeat,
                            last_progress,
                            &options,
                        )
                        .await?;
                        last_progress = Instant::now();
                        if !options.chaos.suppresses_heartbeats_for(operation) {
                            heartbeat_workflow_lease(control, lease_id, &mut last_heartbeat, &options).await?;
                        }
                    }
                    Ok(NdjsonOutcome::Terminated(frame)) => {
                        validate_frame_lease(&frame, lease_id)?;
                        fail_if_watchdog_elapsed(
                            control,
                            lease_id,
                            last_heartbeat,
                            last_progress,
                            &options,
                        )
                        .await?;
                        return handle_terminal_frame(
                            control,
                            lease_id,
                            frame,
                        )
                        .await;
                    }
                    Ok(NdjsonOutcome::StreamEnd { .. } | NdjsonOutcome::Closed) => {
                        return fail_lease_and_return(
                            control,
                            lease_id,
                            FailureClass::WorkerCrash,
                            VoomError::WorkerCrash(format!("worker stream closed before terminal frame for lease {lease_id}")),
                        ).await;
                    }
                    Err(err) => {
                        return fail_lease_and_return(
                            control,
                            lease_id,
                            FailureClass::MalformedWorkerResult,
                            map_protocol_error(&err),
                        ).await;
                    }
                }
            }
            () = &mut heartbeat_deadline => {
                return fail_lease_and_return(
                    control,
                    lease_id,
                    FailureClass::WorkerTimeout,
                    VoomError::WorkerTimeout(format!("heartbeat timeout for lease {lease_id}")),
                ).await;
            }
            () = &mut progress_deadline => {
                return fail_lease_and_return(
                    control,
                    lease_id,
                    FailureClass::ProgressTimeout,
                    VoomError::WorkerTimeout(format!("progress timeout for lease {lease_id}")),
                ).await;
            }
            _ = heartbeat.tick(), if !options.chaos.suppresses_heartbeats_for(operation) => {
                heartbeat_workflow_lease(control, lease_id, &mut last_heartbeat, &options).await?;
            }
        }
    }
}

async fn handle_terminal_frame(
    control: &ControlPlane,
    lease_id: LeaseId,
    frame: ProgressFrame,
) -> Result<(), VoomError> {
    match frame {
        ProgressFrame::Result { payload, .. } => {
            if !payload.is_object() {
                return fail_lease_and_return(
                    control,
                    lease_id,
                    FailureClass::MalformedWorkerResult,
                    VoomError::MalformedWorkerResult(format!(
                        "result payload for lease {lease_id} must be an object"
                    )),
                )
                .await;
            }
            release_lease_with_retry(control, lease_id, payload).await?;
            Ok(())
        }
        ProgressFrame::Error { class, message, .. } => {
            let source = voom_error_for_failure_class(class, message);
            fail_lease_and_return(control, lease_id, class, source).await
        }
        ProgressFrame::Progress { .. } => Err(VoomError::Internal(
            "progress frame cannot be terminal".to_owned(),
        )),
    }
}

fn map_protocol_error(err: &ProtocolError) -> VoomError {
    match err {
        ProtocolError::MalformedFrame { detail } => {
            VoomError::MalformedWorkerResult(detail.clone())
        }
        ProtocolError::WrongLeaseId { .. }
        | ProtocolError::OutOfOrderFrame { .. }
        | ProtocolError::UnexpectedFrameAfterTerminal
        | ProtocolError::InvalidPayload { .. } => VoomError::MalformedWorkerResult(err.to_string()),
        _ => VoomError::WorkerCrash(err.to_string()),
    }
}

fn map_dispatch_setup_protocol_error(err: &ProtocolError) -> VoomError {
    match err {
        ProtocolError::MalformedFrame { detail }
            if detail.contains("missing response/body separator")
                || detail.contains("response read") =>
        {
            VoomError::WorkerCrash(err.to_string())
        }
        ProtocolError::InvalidPayload { detail }
            if detail.contains("request:") || detail.contains("body:") =>
        {
            VoomError::WorkerCrash(err.to_string())
        }
        _ => map_protocol_error(err),
    }
}

fn voom_error_for_failure_class(class: FailureClass, message: String) -> VoomError {
    match class.into_error_code() {
        ErrorCode::WorkerTimeout => VoomError::WorkerTimeout(message),
        ErrorCode::WorkerCrash => VoomError::WorkerCrash(message),
        ErrorCode::NoEligibleWorker => VoomError::NoEligibleWorker(message),
        ErrorCode::ArtifactUnavailable => VoomError::ArtifactUnavailable(message),
        ErrorCode::ArtifactChecksumMismatch => VoomError::ArtifactChecksumMismatch(message),
        ErrorCode::ExternalSystemUnavailable => VoomError::ExternalSystemUnavailable(message),
        ErrorCode::ExternalSystemRateLimited => VoomError::ExternalSystemRateLimited(message),
        ErrorCode::VerificationFailure => VoomError::VerificationFailure(message),
        ErrorCode::BackupFailure => VoomError::BackupFailure(message),
        ErrorCode::CommitFailure => VoomError::CommitFailure(message),
        ErrorCode::PolicyParseError => VoomError::PolicyParseError(message),
        ErrorCode::PolicyValidationError => VoomError::PolicyValidationError(message),
        ErrorCode::MissingCapability => VoomError::MissingCapability(message),
        ErrorCode::MalformedWorkerResult => VoomError::MalformedWorkerResult(message),
        ErrorCode::UserCancellation => VoomError::UserCancellation(message),
        ErrorCode::StaleIdentityEvidence => VoomError::StaleIdentityEvidence(message),
        ErrorCode::ClosureResolutionIncomplete => VoomError::ClosureResolutionIncomplete(message),
        ErrorCode::BlockedByUseLease => VoomError::BlockedByUseLease(message),
        ErrorCode::ApprovalRequired => VoomError::ApprovalRequired(message),
        ErrorCode::PriorityPolicyConflict => VoomError::PriorityPolicyConflict(message),
        ErrorCode::AmbiguousWorkerSelection => VoomError::AmbiguousWorkerSelection(message),
        other => VoomError::Internal(format!(
            "unsupported worker failure code {other:?}: {message}"
        )),
    }
}

fn apply_chaos_payload_override(
    payload: &mut Value,
    operation: OperationKind,
    chaos: &WorkflowChaosOptions,
) -> Result<(), VoomError> {
    let Some(mode) = chaos.payload_mode_for(operation) else {
        return Ok(());
    };
    let Some(object) = payload.as_object_mut() else {
        return Err(VoomError::Config(format!(
            "workflow chaos payload for {operation:?} must be an object"
        )));
    };
    object.insert("mode".to_owned(), Value::String(mode.to_owned()));
    Ok(())
}

fn no_response_timeout(options: &WorkflowExecutorOptions) -> Duration {
    options
        .heartbeat_timeout
        .min(options.progress_idle_timeout)
        .max(Duration::from_millis(1))
}

fn validate_frame_lease(frame: &ProgressFrame, lease_id: LeaseId) -> Result<(), VoomError> {
    if frame.lease_id() == lease_id {
        Ok(())
    } else {
        Err(VoomError::MalformedWorkerResult(format!(
            "wrong lease id in frame: expected {lease_id}, got {}",
            frame.lease_id()
        )))
    }
}

fn duration_millis_u32(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

fn sleep_until(deadline: Instant) -> Pin<Box<tokio::time::Sleep>> {
    Box::pin(tokio::time::sleep_until(deadline))
}
