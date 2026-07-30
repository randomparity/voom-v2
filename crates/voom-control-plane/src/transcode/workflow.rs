use serde_json::Value;
#[cfg(test)]
use voom_core::OperationKind;
use voom_core::VoomError;
use voom_worker_protocol::{TranscodeVideoRequest, TranscodeVideoResult, VideoHardwareAssignment};

use crate::cases::policy::compliance::committed_source_dir;
use crate::transcode::{
    ExecuteTranscodeVideoInput, TranscodeVideoDispatcher, execute_transcode_video_with_dispatchers,
};
use crate::workflow::execution::leases::{
    fail_lease_and_return, failure_class_for_error, release_lease_with_retry,
};

use crate::workflow::execution::operation_adapters::{
    OperationAdapterContext, RuntimeDispatchContext, workflow_idempotency_key,
};

pub(crate) async fn dispatch_control_plane_transcode(
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
    let source_file_version_id = context.source_file_version_id()?;
    let hardware_assignment = context
        .payload
        .get("hardware_assignment")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            VoomError::Config(format!(
                "transcode ticket {} hardware_assignment malformed: {error}",
                context.ticket.id
            ))
        })?;
    let input = ExecuteTranscodeVideoInput {
        job_id: context.job_id("transcode")?,
        ticket_id: context.ticket.id,
        lease_id: context.lease_id,
        source_file_version_id,
        source_location_id: context.source_location_id(),
        staging_root: context.artifact_roots.staging_root.clone(),
        target_dir: committed_source_dir(
            &context.artifact_roots.target_dir,
            source_file_version_id,
        ),
        resolved: crate::transcode::resolve::ResolvedProfile {
            profile: resolved_profile,
            output_container,
        },
        backup_root: context.backup_root.map(std::path::Path::to_path_buf),
    };
    let report = match execute_transcode_video_with_dispatchers(
        context.control,
        input,
        &RuntimeTranscodeDispatcher {
            context: context.runtime_dispatch_context(),
            hardware_assignment,
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
    hardware_assignment: Option<VideoHardwareAssignment>,
}

#[async_trait::async_trait]
impl TranscodeVideoDispatcher for RuntimeTranscodeDispatcher<'_> {
    async fn dispatch_transcode_video(
        &self,
        mut request: TranscodeVideoRequest,
    ) -> Result<TranscodeVideoResult, VoomError> {
        request.hardware_assignment = self.hardware_assignment.clone();
        let idempotency_key =
            workflow_idempotency_key(self.context.ticket_id, self.context.lease_id);
        let result = crate::transcode::dispatch::dispatch_transcode_video_with_client(
            self.context.runtime.client.as_ref(),
            &self.context.runtime.credentials,
            &idempotency_key,
            request,
        )
        .await?;
        #[cfg(test)]
        self.context
            .chaos
            .hold_after_worker_result(OperationKind::TranscodeVideo)
            .await;
        Ok(result)
    }
}
