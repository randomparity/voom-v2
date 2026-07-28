use serde_json::Value;
use voom_core::VoomError;
use voom_worker_protocol::{
    ExtractAudioRequest, ExtractAudioResult, OperationKind, TranscodeAudioRequest,
    TranscodeAudioResult,
};

use crate::audio::{
    ExecuteExtractAudioInput, ExecuteTranscodeAudioInput, ExtractAudioDispatcher,
    FirstExtractPlanInput, TranscodeAudioDispatcher, execute_extract_audio_with_dispatchers,
    execute_transcode_audio_with_dispatchers,
};
use crate::cases::policy::compliance::committed_source_dir;
use crate::workflow::execution::leases::{
    fail_lease_and_return, failure_class_for_error, release_lease_with_retry,
};

use crate::workflow::execution::operation_adapters::{
    OperationAdapterContext, RuntimeDispatchContext, await_with_lease_heartbeats,
};

pub(crate) async fn dispatch_control_plane_transcode_audio(
    context: OperationAdapterContext<'_>,
) -> Result<(), VoomError> {
    let input = match transcode_audio_input_for_workflow_ticket(context) {
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
    let report = match Box::pin(execute_transcode_audio_with_dispatchers(
        context.control,
        input,
        &RuntimeTranscodeAudioDispatcher {
            context: context.runtime_dispatch_context(),
        },
        &crate::artifact::verify::BundledVerifyArtifactDispatcher,
        &crate::audio::commit::BundledAudioResultProbeDispatcher,
    ))
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
        .map_err(|err| VoomError::Internal(format!("encode transcode audio report: {err}")))?;
    release_lease_with_retry(context.control, context.lease_id, result).await
}

pub(crate) async fn dispatch_control_plane_extract_audio(
    context: OperationAdapterContext<'_>,
) -> Result<(), VoomError> {
    let input = match extract_audio_input_for_workflow_ticket(context).await {
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
    let report = match execute_extract_audio_with_dispatchers(
        context.control,
        input,
        &RuntimeExtractAudioDispatcher {
            context: context.runtime_dispatch_context(),
        },
        &crate::artifact::verify::BundledVerifyArtifactDispatcher,
        &crate::audio::commit::BundledAudioResultProbeDispatcher,
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
        .map_err(|err| VoomError::Internal(format!("encode extract audio report: {err}")))?;
    release_lease_with_retry(context.control, context.lease_id, result).await
}

fn transcode_audio_input_for_workflow_ticket(
    context: OperationAdapterContext<'_>,
) -> Result<ExecuteTranscodeAudioInput, VoomError> {
    let operation_payload = audio_payload(context.payload, "transcode audio")?;
    let source_file_version_id = context.source_file_version_id()?;
    Ok(ExecuteTranscodeAudioInput {
        job_id: context.job_id("transcode audio")?,
        ticket_id: context.ticket.id,
        lease_id: context.lease_id,
        source_file_version_id,
        source_location_id: context.source_location_id(),
        operation_payload,
        staging_root: context.artifact_roots.staging_root.clone(),
        target_dir: committed_source_dir(
            &context.artifact_roots.target_dir,
            source_file_version_id,
        ),
        backup_root: context.backup_root.map(std::path::Path::to_path_buf),
    })
}

async fn extract_audio_input_for_workflow_ticket(
    context: OperationAdapterContext<'_>,
) -> Result<ExecuteExtractAudioInput, VoomError> {
    let operation_payload = audio_payload(context.payload, "extract audio")?;
    let source_file_version_id = context.source_file_version_id()?;
    let target_dir =
        committed_source_dir(&context.artifact_roots.target_dir, source_file_version_id);
    let source_location_id = context.source_location_id();
    let source_bundle_id = context
        .control
        .find_primary_bundle_for_file_version(source_file_version_id)
        .await?;
    let source_bundle_id = match source_bundle_id {
        Some(source_bundle_id) => source_bundle_id,
        None => {
            crate::audio::plan_first_extract_with_bundle(
                context.control,
                FirstExtractPlanInput {
                    source_file_version_id,
                    source_location_id,
                    operation_payload: operation_payload.clone(),
                    target_dir: target_dir.clone(),
                },
            )
            .await?
        }
    };
    Ok(ExecuteExtractAudioInput {
        job_id: context.job_id("extract audio")?,
        ticket_id: context.ticket.id,
        lease_id: context.lease_id,
        source_file_version_id,
        source_location_id,
        source_bundle_id,
        operation_payload,
        staging_root: context.artifact_roots.staging_root.clone(),
        target_dir,
        backup_root: context.backup_root.map(std::path::Path::to_path_buf),
    })
}

fn audio_payload(payload: &Value, operation: &str) -> Result<Value, VoomError> {
    payload
        .get("audio")
        .cloned()
        .ok_or_else(|| VoomError::Config(format!("{operation} workflow payload missing `audio`")))
}

struct RuntimeTranscodeAudioDispatcher<'a> {
    context: RuntimeDispatchContext<'a>,
}

#[async_trait::async_trait]
impl TranscodeAudioDispatcher for RuntimeTranscodeAudioDispatcher<'_> {
    async fn dispatch_transcode_audio(
        &self,
        dispatch_lease_id: voom_core::LeaseId,
        idempotency_key: &str,
        request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        await_with_lease_heartbeats(
            self.context,
            OperationKind::TranscodeAudio,
            crate::audio::dispatch::dispatch_transcode_audio_with_client_context(
                self.context.runtime.client.as_ref(),
                &self.context.runtime.credentials,
                dispatch_lease_id,
                idempotency_key,
                request,
            ),
        )
        .await
    }
}

struct RuntimeExtractAudioDispatcher<'a> {
    context: RuntimeDispatchContext<'a>,
}

#[async_trait::async_trait]
impl ExtractAudioDispatcher for RuntimeExtractAudioDispatcher<'_> {
    async fn dispatch_extract_audio(
        &self,
        idempotency_key: &str,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        await_with_lease_heartbeats(
            self.context,
            OperationKind::ExtractAudio,
            crate::audio::dispatch::dispatch_extract_audio_with_client_context(
                self.context.runtime.client.as_ref(),
                &self.context.runtime.credentials,
                self.context.lease_id,
                idempotency_key,
                request,
            ),
        )
        .await
    }
}
