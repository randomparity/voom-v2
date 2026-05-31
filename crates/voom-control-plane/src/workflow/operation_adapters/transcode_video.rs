use serde_json::Value;
use voom_core::VoomError;
use voom_worker_protocol::{OperationKind, TranscodeVideoRequest, TranscodeVideoResult};

use crate::transcode::{
    ExecuteTranscodeVideoInput, TranscodeVideoDispatcher, execute_transcode_video_with_dispatchers,
};
use crate::workflow::leases::{
    fail_lease_and_return, failure_class_for_error, release_lease_with_retry,
};

use super::{
    OperationAdapterContext, RuntimeDispatchContext, await_with_lease_heartbeats,
    workflow_idempotency_key,
};

pub(super) async fn dispatch_control_plane_transcode(
    context: OperationAdapterContext<'_>,
) -> Result<(), VoomError> {
    let resolved_profile: voom_core::TranscodeVideoProfile = serde_json::from_value(
        context
            .payload
            .get("resolved_profile")
            .ok_or_else(|| {
                VoomError::Config(format!(
                    "transcode ticket {} missing resolved_profile",
                    context.ticket.id
                ))
            })?
            .clone(),
    )
    .map_err(|err| {
        VoomError::Config(format!(
            "transcode ticket {} resolved_profile malformed: {err}",
            context.ticket.id
        ))
    })?;
    let output_container = context
        .payload
        .get("container")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VoomError::Config(format!(
                "transcode ticket {} missing container",
                context.ticket.id
            ))
        })?
        .to_owned();
    let input = ExecuteTranscodeVideoInput {
        job_id: context.job_id("transcode")?,
        ticket_id: context.ticket.id,
        lease_id: context.lease_id,
        source_file_version_id: context.source_file_version_id()?,
        source_location_id: context.source_location_id(),
        staging_root: context.options.transcode_staging_root.clone(),
        target_dir: context.options.transcode_target_dir.clone(),
        resolved: crate::transcode::resolve::ResolvedProfile {
            profile: resolved_profile,
            output_container,
        },
    };
    let report = match execute_transcode_video_with_dispatchers(
        context.control,
        input,
        &RuntimeTranscodeDispatcher {
            context: context.runtime_dispatch_context(),
        },
        &crate::artifact::verify::BundledVerifyArtifactDispatcher,
        &crate::transcode::commit::BundledTranscodeResultProbeDispatcher,
    )
    .await
    {
        Ok(report) => report,
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
    let result = serde_json::to_value(report)
        .map_err(|err| VoomError::Internal(format!("encode transcode report: {err}")))?;
    release_lease_with_retry(context.control, context.lease_id, result).await
}

struct RuntimeTranscodeDispatcher<'a> {
    context: RuntimeDispatchContext<'a>,
}

#[async_trait::async_trait]
impl TranscodeVideoDispatcher for RuntimeTranscodeDispatcher<'_> {
    async fn dispatch_transcode_video(
        &self,
        request: TranscodeVideoRequest,
    ) -> Result<TranscodeVideoResult, VoomError> {
        let idempotency_key =
            workflow_idempotency_key(self.context.ticket_id, self.context.lease_id);
        await_with_lease_heartbeats(
            self.context,
            OperationKind::TranscodeVideo,
            crate::transcode::dispatch::dispatch_transcode_video_with_client(
                self.context.runtime.client.as_ref(),
                &self.context.runtime.credentials,
                &idempotency_key,
                request,
            ),
        )
        .await
    }
}
