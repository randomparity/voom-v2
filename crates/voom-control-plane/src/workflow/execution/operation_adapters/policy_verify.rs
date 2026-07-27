use serde_json::{Value, json};
use voom_core::{
    ErrorCode, FileLocationId, FileVersionId, LeaseId, OperationKind, VoomError, WorkerId,
};
use voom_store::repo::artifacts::ArtifactVerificationStatus;
use voom_store::repo::tickets::Ticket;

use super::{LeaseHeartbeatContext, await_with_lease_heartbeats_without_runtime};
use crate::ControlPlane;
use crate::artifact::verify::{
    BundledVerifyArtifactDispatcher, NoVerifyArtifactHooks, PolicyVerifyArtifactInput,
    verify_policy_artifact_with_dispatcher,
};
use crate::workflow::execution::executor::{WorkflowChaosOptions, WorkflowTimingOptions};
use crate::workflow::execution::leases::{
    fail_lease_and_return, failure_class_for_error, release_lease_with_retry,
};

pub(super) async fn dispatch_policy_verify_artifact(
    control: &ControlPlane,
    ticket: &Ticket,
    worker_id: WorkerId,
    lease_id: LeaseId,
    payload: &Value,
    timing: &WorkflowTimingOptions,
    chaos: &WorkflowChaosOptions,
) -> Result<(), VoomError> {
    let source_file_version_id = FileVersionId(required_u64(payload, "source_file_version_id")?);
    let source_location_id = optional_u64(payload, "source_location_id").map(FileLocationId);
    let target = control
        .resolve_policy_artifact_target(source_file_version_id, source_location_id)
        .await?;
    let input = PolicyVerifyArtifactInput {
        target,
        worker_id,
        ticket_id: ticket.id,
        lease_id,
    };
    let report = await_with_lease_heartbeats_without_runtime(
        LeaseHeartbeatContext {
            control,
            lease_id,
            timing,
            chaos,
        },
        OperationKind::VerifyArtifact,
        verify_policy_artifact_with_dispatcher(
            control,
            &input,
            &BundledVerifyArtifactDispatcher,
            &NoVerifyArtifactHooks,
        ),
    )
    .await?;
    if report.status == ArtifactVerificationStatus::Succeeded {
        return release_lease_with_retry(
            control,
            lease_id,
            json!({
                "source_file_version_id": input.target.file_version_id,
                "source_location_id": input.target.file_location_id,
                "source_media_snapshot_id": input.target.media_snapshot_id,
                "artifact_handle_id": report.artifact_handle_id,
                "artifact_location_id": report.artifact_location_id,
                "artifact_verification_id": report.verification_id,
                "status": "verified",
                "expected_size_bytes": report.expected_size_bytes,
                "expected_checksum": report.expected_checksum,
                "observed_size_bytes": report.observed_size_bytes,
                "observed_checksum": report.observed_checksum,
            }),
        )
        .await;
    }

    let source = verification_error(&report);
    fail_lease_and_return(control, lease_id, failure_class_for_error(&source), source).await
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

fn required_u64(payload: &Value, field: &str) -> Result<u64, VoomError> {
    payload
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| VoomError::Config(format!("workflow payload missing `{field}`")))
}

fn optional_u64(payload: &Value, field: &str) -> Option<u64> {
    payload.get(field).and_then(Value::as_u64)
}
