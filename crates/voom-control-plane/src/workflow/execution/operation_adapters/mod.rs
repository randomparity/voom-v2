use std::future::Future;

use serde_json::Value;
use voom_core::{LeaseId, OperationKind, VoomError};

use crate::ControlPlane;
#[cfg(test)]
use crate::workflow::execution::executor::WorkflowChaosOptions;
use crate::workflow::execution::executor::{WorkflowDispatchOptions, WorkflowTimingOptions};
use voom_store::repo::execution::tickets::Ticket;

mod policy_verify;

pub(super) async fn dispatch_control_plane_ticket(
    context: TicketDispatchContext<'_>,
) -> Option<Result<(), VoomError>> {
    // T8: the only control-plane-executed ticket family left is the bundled
    // policy verification (#528/#424 owns its retarget). Everything else falls
    // through to the generic worker-protocol dispatch.
    if !uses_bundled_policy_verification(context.operation, context.payload) {
        return None;
    }
    Some(
        await_with_lease_heartbeats(
            context.lease_heartbeat_context(),
            context.operation,
            policy_verify::dispatch_policy_verify_artifact(context),
        )
        .await,
    )
}

pub(super) fn uses_bundled_policy_verification(operation: OperationKind, payload: &Value) -> bool {
    operation == OperationKind::VerifyArtifact && payload.get("source_file_version_id").is_some()
}

#[derive(Clone, Copy)]
pub(super) struct TicketDispatchContext<'a> {
    pub(super) control: &'a ControlPlane,
    pub(super) worker_id: voom_core::WorkerId,
    pub(super) ticket: &'a Ticket,
    pub(super) operation: OperationKind,
    pub(super) lease_id: LeaseId,
    pub(super) payload: &'a Value,
    pub(super) options: &'a WorkflowDispatchOptions,
}

impl<'a> TicketDispatchContext<'a> {
    fn lease_heartbeat_context(self) -> LeaseHeartbeatContext<'a> {
        LeaseHeartbeatContext {
            control: self.control,
            lease_id: self.lease_id,
            timing: &self.options.timing,
            #[cfg(test)]
            chaos: &self.options.chaos,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LeaseHeartbeatContext<'a> {
    pub(crate) control: &'a ControlPlane,
    pub(crate) lease_id: LeaseId,
    pub(crate) timing: &'a WorkflowTimingOptions,
    #[cfg(test)]
    pub(crate) chaos: &'a WorkflowChaosOptions,
}

pub(crate) async fn await_with_lease_heartbeats<F, T>(
    context: LeaseHeartbeatContext<'_>,
    operation: OperationKind,
    future: F,
) -> Result<T, VoomError>
where
    F: Future<Output = Result<T, VoomError>>,
{
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + context.timing.heartbeat_interval,
        context.timing.heartbeat_interval,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(future);
    loop {
        #[cfg(test)]
        let heartbeat_tick = async {
            if context.chaos.suppresses_heartbeats_for(operation) {
                std::future::pending().await
            } else {
                heartbeat.tick().await
            }
        };
        #[cfg(not(test))]
        let heartbeat_tick = heartbeat.tick();
        tokio::select! {
            biased;
            result = &mut future => return result,
            _ = heartbeat_tick => {
                let result = tokio::select! {
                    biased;
                    result = &mut future => return result,
                    result = heartbeat_lease(context, operation) => result,
                };
                if let Err(source) = result {
                    return crate::workflow::execution::leases::fail_lease_and_return(
                        context.control,
                        context.lease_id,
                        crate::workflow::execution::leases::failure_class_for_error(&source),
                        source,
                    )
                    .await;
                }
            }
        }
    }
}

async fn heartbeat_lease(
    context: LeaseHeartbeatContext<'_>,
    operation: OperationKind,
) -> Result<(), VoomError> {
    #[cfg(not(test))]
    let _ = operation;
    #[cfg(test)]
    if context.chaos.fails_heartbeat_for(operation) {
        return Err(VoomError::Conflict(format!(
            "injected heartbeat failure for {operation:?}"
        )));
    }
    crate::workflow::execution::leases::heartbeat_lease_with_retry(
        context.control,
        context.lease_id,
        crate::workflow::execution::leases::time_duration(context.timing.lease_ttl)?,
    )
    .await
}

pub(super) fn required_u64(payload: &Value, field: &str) -> Result<u64, VoomError> {
    payload
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| VoomError::Config(format!("workflow payload missing `{field}`")))
}

pub(super) fn optional_u64(payload: &Value, field: &str) -> Option<u64> {
    payload.get(field).and_then(Value::as_u64)
}
