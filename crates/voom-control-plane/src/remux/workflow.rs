use serde_json::Value;
use voom_core::{OperationKind, VoomError};
use voom_worker_protocol::{RemuxRequest, RemuxResult};

use crate::cases::{begin_tx, commit_tx};
use crate::remux::commit::BundledRemuxResultProbeDispatcher;
use crate::remux::{
    ExecuteRemuxInput, RemuxDispatcher, execute_remux_with_deferred_success_event,
    success_event_recovery_report,
};
use crate::workflow::execution::leases::{
    fail_lease_and_return, failure_class_for_error, release_lease_with_retry,
    retry_on_database_locked,
};

use crate::workflow::execution::operation_adapters::{
    OperationAdapterContext, RuntimeDispatchContext, await_with_lease_heartbeats,
    workflow_idempotency_key,
};

pub(crate) async fn dispatch_control_plane_remux_context(
    context: OperationAdapterContext<'_>,
) -> Result<(), VoomError> {
    let input = match remux_input_for_workflow_ticket(context) {
        Ok(input) => input,
        Err(source) => {
            return fail_lease_and_return(
                context.control,
                context.lease_id,
                failure_class_for_error(&source),
                source,
            )
            .await;
        }
    };
    let success = match execute_remux_with_deferred_success_event(
        context.control,
        input,
        &RuntimeRemuxDispatcher {
            context: context.runtime_dispatch_context(),
        },
        &crate::artifact::verify::BundledVerifyArtifactDispatcher,
        &BundledRemuxResultProbeDispatcher,
    )
    .await
    {
        Ok(success) => success,
        Err(source) => {
            return fail_lease_and_return(
                context.control,
                context.lease_id,
                failure_class_for_error(&source),
                source,
            )
            .await;
        }
    };
    let result = serde_json::to_value(&success.report)
        .map_err(|err| VoomError::Internal(format!("encode remux report: {err}")))?;
    match release_remux_lease_with_retry(
        context.control,
        context.lease_id,
        result,
        &success.success_event,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(source) => {
            let recovery = success_event_recovery_report(&success, &source);
            let result = serde_json::to_value(&recovery).map_err(|err| {
                VoomError::Internal(format!("encode remux success-event recovery: {err}"))
            })?;
            release_lease_with_retry(context.control, context.lease_id, result).await
        }
    }
}

#[cfg(test)]
pub(crate) async fn dispatch_control_plane_remux(
    control: &crate::ControlPlane,
    runtime: &crate::workflow::execution::runtime::WorkerRuntime,
    ticket: &voom_store::repo::tickets::Ticket,
    _workflow_payload: &crate::workflow::plan::ticket_payload::WorkflowTicketPayload,
    lease_id: voom_core::LeaseId,
    payload: &Value,
    options: &crate::workflow::execution::executor::WorkflowExecutorOptions,
) -> Result<(), VoomError> {
    dispatch_control_plane_remux_context(OperationAdapterContext {
        control,
        runtime,
        ticket,
        lease_id,
        payload,
        options,
    })
    .await
}

fn remux_input_for_workflow_ticket(
    context: OperationAdapterContext<'_>,
) -> Result<ExecuteRemuxInput, VoomError> {
    let operation_payload =
        context.payload.get("remux").cloned().ok_or_else(|| {
            VoomError::Config("remux workflow payload missing `remux`".to_owned())
        })?;
    Ok(ExecuteRemuxInput {
        job_id: context.job_id("remux")?,
        ticket_id: context.ticket.id,
        lease_id: context.lease_id,
        source_file_version_id: context.source_file_version_id()?,
        source_location_id: context.source_location_id(),
        operation_payload,
        staging_root: context.options.remux_staging_root.clone(),
        target_dir: context.options.remux_target_dir.clone(),
    })
}

struct RuntimeRemuxDispatcher<'a> {
    context: RuntimeDispatchContext<'a>,
}

#[async_trait::async_trait]
impl RemuxDispatcher for RuntimeRemuxDispatcher<'_> {
    async fn dispatch_remux(&self, request: RemuxRequest) -> Result<RemuxResult, VoomError> {
        let mut progress = crate::remux::dispatch::NoopRemuxProgressSink;
        self.dispatch_remux_with_progress(request, &mut progress)
            .await
    }

    async fn dispatch_remux_with_progress(
        &self,
        request: RemuxRequest,
        progress: &mut dyn crate::remux::dispatch::RemuxProgressSink,
    ) -> Result<RemuxResult, VoomError> {
        let idempotency_key =
            workflow_idempotency_key(self.context.ticket_id, self.context.lease_id);
        await_with_lease_heartbeats(
            self.context,
            OperationKind::Remux,
            crate::remux::dispatch::dispatch_remux_with_client_context_and_progress(
                self.context.runtime.client.as_ref(),
                &self.context.runtime.credentials,
                &idempotency_key,
                self.context.lease_id,
                request,
                progress,
            ),
        )
        .await
    }
}

async fn release_remux_lease_with_retry(
    control: &crate::ControlPlane,
    lease_id: voom_core::LeaseId,
    payload: Value,
    success_event: &crate::remux::events::RemuxSucceededEvent,
) -> Result<(), VoomError> {
    retry_on_database_locked(|| {
        let payload = payload.clone();
        async move {
            let mut tx = begin_tx(&control.pool).await?;
            let now = control.clock().now();
            crate::remux::events::append_succeeded_in_tx(control, &mut tx, success_event, now)
                .await?;
            control
                .release_lease_in_tx(&mut tx, lease_id, payload, now)
                .await?;
            commit_tx(tx).await
        }
    })
    .await
}
