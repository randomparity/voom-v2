use serde_json::Value;
use sqlx::Row;
use voom_core::{BundleId, FileVersionId, VoomError};
use voom_worker_protocol::{
    ExtractAudioRequest, ExtractAudioResult, OperationKind, TranscodeAudioRequest,
    TranscodeAudioResult,
};

use crate::audio::{
    ExecuteExtractAudioInput, ExecuteTranscodeAudioInput, ExtractAudioDispatcher,
    TranscodeAudioDispatcher, execute_extract_audio_with_dispatchers,
    execute_transcode_audio_with_dispatchers,
};
use crate::workflow::leases::{
    fail_lease_and_return, failure_class_for_error, release_lease_with_retry,
};

use super::{
    OperationAdapterContext, RuntimeDispatchContext, await_with_lease_heartbeats,
    workflow_idempotency_key,
};

pub(super) async fn dispatch_control_plane_transcode_audio(
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
    let report = match execute_transcode_audio_with_dispatchers(
        context.control,
        input,
        &RuntimeTranscodeAudioDispatcher {
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
        .map_err(|err| VoomError::Internal(format!("encode transcode audio report: {err}")))?;
    release_lease_with_retry(context.control, context.lease_id, result).await
}

pub(super) async fn dispatch_control_plane_extract_audio(
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
    Ok(ExecuteTranscodeAudioInput {
        job_id: context.job_id("transcode audio")?,
        ticket_id: context.ticket.id,
        lease_id: context.lease_id,
        source_file_version_id: context.source_file_version_id()?,
        source_location_id: context.source_location_id(),
        operation_payload,
        staging_root: context.options.audio_staging_root.clone(),
        target_dir: context.options.audio_target_dir.clone(),
    })
}

async fn extract_audio_input_for_workflow_ticket(
    context: OperationAdapterContext<'_>,
) -> Result<ExecuteExtractAudioInput, VoomError> {
    let operation_payload = audio_payload(context.payload, "extract audio")?;
    let source_file_version_id = context.source_file_version_id()?;
    Ok(ExecuteExtractAudioInput {
        job_id: context.job_id("extract audio")?,
        ticket_id: context.ticket.id,
        lease_id: context.lease_id,
        source_file_version_id,
        source_location_id: context.source_location_id(),
        source_bundle_id: source_bundle_id_for_file_version(context, source_file_version_id)
            .await?,
        operation_payload,
        staging_root: context.options.audio_staging_root.clone(),
        target_dir: context.options.audio_target_dir.clone(),
    })
}

fn audio_payload(payload: &Value, operation: &str) -> Result<Value, VoomError> {
    payload
        .get("audio")
        .cloned()
        .ok_or_else(|| VoomError::Config(format!("{operation} workflow payload missing `audio`")))
}

async fn source_bundle_id_for_file_version(
    context: OperationAdapterContext<'_>,
    source_file_version_id: FileVersionId,
) -> Result<BundleId, VoomError> {
    let row = sqlx::query(
        "SELECT abm.bundle_id \
         FROM file_versions fv \
         JOIN asset_bundle_members abm ON abm.file_asset_id = fv.file_asset_id \
         WHERE fv.id = ?",
    )
    .bind(i64::try_from(source_file_version_id.0).unwrap_or(i64::MAX))
    .fetch_optional(&context.control.pool)
    .await
    .map_err(|e| VoomError::Database(format!("audio source bundle lookup: {e}")))?;
    let row = row.ok_or_else(|| {
        VoomError::Config(format!(
            "file_version {source_file_version_id} is not a bundle member"
        ))
    })?;
    let bundle_id: i64 = row
        .try_get("bundle_id")
        .map_err(|e| VoomError::Database(format!("audio source bundle id: {e}")))?;
    Ok(BundleId(u64::try_from(bundle_id).unwrap_or(0)))
}

struct RuntimeTranscodeAudioDispatcher<'a> {
    context: RuntimeDispatchContext<'a>,
}

#[async_trait::async_trait]
impl TranscodeAudioDispatcher for RuntimeTranscodeAudioDispatcher<'_> {
    async fn dispatch_transcode_audio(
        &self,
        request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        let idempotency_key =
            workflow_idempotency_key(self.context.ticket_id, self.context.lease_id);
        await_with_lease_heartbeats(
            self.context,
            OperationKind::TranscodeAudio,
            crate::audio::dispatch::dispatch_transcode_audio_with_client_context(
                self.context.runtime.client.as_ref(),
                &self.context.runtime.credentials,
                self.context.lease_id,
                &idempotency_key,
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
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        let idempotency_key =
            workflow_idempotency_key(self.context.ticket_id, self.context.lease_id);
        await_with_lease_heartbeats(
            self.context,
            OperationKind::ExtractAudio,
            crate::audio::dispatch::dispatch_extract_audio_with_client_context(
                self.context.runtime.client.as_ref(),
                &self.context.runtime.credentials,
                self.context.lease_id,
                &idempotency_key,
                request,
            ),
        )
        .await
    }
}
