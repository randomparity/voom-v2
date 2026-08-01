use serde_json::to_value;
#[cfg(test)]
use voom_core::OperationKind;
use voom_core::{ErrorCode, FileLocationId, FileVersionId, VoomError};
use voom_store::repo::media::artifacts::ArtifactVerificationStatus;

use super::{TicketDispatchContext, optional_u64, required_u64};
use crate::artifact::verify::{
    BundledVerifyArtifactDispatcher, NoVerifyArtifactHooks, PolicyVerifyArtifactInput,
    verify_policy_artifact_with_dispatcher,
};
use crate::workflow::execution::leases::{
    fail_lease_and_return, failure_class_for_error, release_lease_with_retry,
};
use crate::workflow::ticket_results::{
    PolicyVerificationTicketResult, PolicyVerificationTicketStatus,
};

pub(super) async fn dispatch_policy_verify_artifact(
    context: TicketDispatchContext<'_>,
) -> Result<(), VoomError> {
    let source_file_version_id =
        FileVersionId(required_u64(context.payload, "source_file_version_id")?);
    let source_location_id =
        optional_u64(context.payload, "source_location_id").map(FileLocationId);
    let target = context
        .control
        .resolve_policy_artifact_target(source_file_version_id, source_location_id)
        .await?;
    let input = PolicyVerifyArtifactInput {
        target,
        worker_id: context.worker_id,
        ticket_id: context.ticket.id,
        lease_id: context.lease_id,
    };
    let report = verify_policy_artifact_with_dispatcher(
        context.control,
        &input,
        &BundledVerifyArtifactDispatcher,
        &NoVerifyArtifactHooks,
    )
    .await?;
    #[cfg(test)]
    context
        .options
        .chaos
        .hold_after_worker_result(OperationKind::VerifyArtifact)
        .await;
    if report.status == ArtifactVerificationStatus::Succeeded {
        let result = PolicyVerificationTicketResult {
            source_file_version_id: input.target.file_version_id,
            source_location_id: input.target.file_location_id,
            source_media_snapshot_id: input.target.media_snapshot_id,
            artifact_handle_id: report.artifact_handle_id,
            artifact_location_id: report.artifact_location_id,
            artifact_verification_id: report.verification_id,
            status: PolicyVerificationTicketStatus::Verified,
            path: report.path.to_string_lossy().into_owned(),
            expected_size_bytes: report.expected_size_bytes,
            expected_checksum: report.expected_checksum,
            observed_size_bytes: report.observed_size_bytes,
            observed_checksum: report.observed_checksum,
        };
        return release_lease_with_retry(
            context.control,
            context.lease_id,
            to_value(result).map_err(|error| {
                VoomError::Internal(format!("policy verification ticket result encode: {error}"))
            })?,
        )
        .await;
    }

    let source = verification_error(&report);
    fail_lease_and_return(
        context.control,
        context.lease_id,
        failure_class_for_error(&source),
        source,
    )
    .await
}

fn verification_error(report: &crate::artifact::VerifyArtifactReport) -> VoomError {
    let message = report
        .message
        .clone()
        .unwrap_or_else(|| "policy artifact verification failed".to_owned());
    match report.error_code {
        Some(ErrorCode::ArtifactChecksumMismatch) => VoomError::ArtifactChecksumMismatch(message),
        Some(ErrorCode::ArtifactUnavailable) => VoomError::ArtifactUnavailable(message),
        Some(ErrorCode::MalformedWorkerResult) => VoomError::MalformedWorkerResult(message),
        Some(ErrorCode::WorkerTimeout) => VoomError::WorkerTimeout(message),
        Some(ErrorCode::WorkerCrash) => VoomError::WorkerCrash(message),
        _ => VoomError::VerificationFailure(message),
    }
}
