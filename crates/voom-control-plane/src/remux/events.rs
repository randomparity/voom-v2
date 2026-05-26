use std::path::Path;

use voom_core::{
    ArtifactHandleId, ArtifactLocationId, ErrorCode, FailureClass, FileLocationId, VoomError,
};
use voom_events::payload::{
    ArtifactRemuxFailedPayload, ArtifactRemuxStartedPayload, ArtifactRemuxStreamPayload,
    ArtifactRemuxSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_worker_protocol::{RemuxResult, RemuxSelection, RemuxStreamRef, RemuxTrackGroup};

use super::ExecuteRemuxInput;
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};

pub async fn record_started(
    cp: &ControlPlane,
    input: &ExecuteRemuxInput,
    source_location_id: FileLocationId,
    selection: &RemuxSelection,
    staging_path: &Path,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    append_event(
        &cp.events,
        &mut tx,
        SubjectType::FileVersion,
        Some(input.source_file_version_id.0),
        now,
        Event::ArtifactRemuxStarted(ArtifactRemuxStartedPayload {
            job_id: input.job_id.0,
            ticket_id: input.ticket_id.0,
            lease_id: Some(input.lease_id.0),
            source_file_version_id: input.source_file_version_id.0,
            source_file_location_id: source_location_id.0,
            staging_path: staging_path.display().to_string(),
            selected_streams: stream_payloads(&selection.keep_streams),
            default_streams: stream_payloads(&selection.default_streams),
            clear_default_streams: stream_payloads(&selection.clear_default_streams),
            track_order: selection
                .track_order
                .iter()
                .copied()
                .map(track_group_name)
                .map(str::to_owned)
                .collect(),
            provider: Some("mkvtoolnix".to_owned()),
            provider_version: None,
        }),
    )
    .await?;
    commit_tx(tx).await
}

#[derive(Debug)]
pub struct RemuxSucceededEventInput<'a> {
    pub input: &'a ExecuteRemuxInput,
    pub source_location_id: FileLocationId,
    pub selection: &'a RemuxSelection,
    pub staging_path: &'a Path,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub result: &'a RemuxResult,
}

pub async fn record_succeeded(
    cp: &ControlPlane,
    event: RemuxSucceededEventInput<'_>,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    append_event(
        &cp.events,
        &mut tx,
        SubjectType::ArtifactHandle,
        Some(event.artifact_handle_id.0),
        now,
        Event::ArtifactRemuxSucceeded(ArtifactRemuxSucceededPayload {
            job_id: event.input.job_id.0,
            ticket_id: event.input.ticket_id.0,
            lease_id: Some(event.input.lease_id.0),
            source_file_version_id: event.input.source_file_version_id.0,
            source_file_location_id: event.source_location_id.0,
            artifact_handle_id: event.artifact_handle_id.0,
            artifact_location_id: event.artifact_location_id.0,
            staging_path: event.staging_path.display().to_string(),
            selected_streams: stream_payloads(&event.selection.keep_streams),
            default_streams: stream_payloads(&event.selection.default_streams),
            clear_default_streams: stream_payloads(&event.selection.clear_default_streams),
            kept_snapshot_stream_ids: event.result.kept_snapshot_stream_ids.clone(),
            default_snapshot_stream_ids: event.result.default_snapshot_stream_ids.clone(),
            output_container: event.result.output_container.clone(),
            provider: event.result.provider.clone(),
            provider_version: event.result.provider_version.clone(),
        }),
    )
    .await?;
    commit_tx(tx).await
}

pub async fn record_failed(
    cp: &ControlPlane,
    input: &ExecuteRemuxInput,
    source_location_id: FileLocationId,
    selection: &RemuxSelection,
    staging_path: &Path,
    result: Option<&RemuxResult>,
    error: &VoomError,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    append_event(
        &cp.events,
        &mut tx,
        SubjectType::FileVersion,
        Some(input.source_file_version_id.0),
        now,
        Event::ArtifactRemuxFailed(ArtifactRemuxFailedPayload {
            job_id: input.job_id.0,
            ticket_id: input.ticket_id.0,
            lease_id: Some(input.lease_id.0),
            source_file_version_id: input.source_file_version_id.0,
            source_file_location_id: Some(source_location_id.0),
            staging_path: Some(staging_path.display().to_string()),
            selected_streams: stream_payloads(&selection.keep_streams),
            default_streams: stream_payloads(&selection.default_streams),
            clear_default_streams: stream_payloads(&selection.clear_default_streams),
            failure_class: failure_class_for_error(error),
            error_code: error.code().to_owned(),
            message: error.to_string(),
            provider: result.map(|result| result.provider.clone()),
            provider_version: result.map(|result| result.provider_version.clone()),
        }),
    )
    .await?;
    commit_tx(tx).await
}

fn stream_payloads(streams: &[RemuxStreamRef]) -> Vec<ArtifactRemuxStreamPayload> {
    streams
        .iter()
        .map(|stream| ArtifactRemuxStreamPayload {
            snapshot_stream_id: stream.snapshot_stream_id.clone(),
            provider_stream_index: stream.provider_stream_index,
        })
        .collect()
}

fn track_group_name(group: RemuxTrackGroup) -> &'static str {
    match group {
        RemuxTrackGroup::Video => "video",
        RemuxTrackGroup::Audio => "audio",
        RemuxTrackGroup::Subtitle => "subtitle",
        RemuxTrackGroup::Attachment => "attachment",
    }
}

fn failure_class_for_error(source: &VoomError) -> FailureClass {
    match source.error_code() {
        ErrorCode::WorkerTimeout => FailureClass::WorkerTimeout,
        ErrorCode::NoEligibleWorker => FailureClass::NoEligibleWorker,
        ErrorCode::ArtifactUnavailable => FailureClass::ArtifactUnavailable,
        ErrorCode::ArtifactChecksumMismatch => FailureClass::ArtifactChecksumMismatch,
        ErrorCode::ExternalSystemUnavailable => FailureClass::ExternalSystemUnavailable,
        ErrorCode::ExternalSystemRateLimited => FailureClass::ExternalSystemRateLimited,
        ErrorCode::VerificationFailure => FailureClass::VerificationFailure,
        ErrorCode::BackupFailure => FailureClass::BackupFailure,
        ErrorCode::CommitFailure => FailureClass::CommitFailure,
        ErrorCode::PolicyParseError => FailureClass::PolicyParseError,
        ErrorCode::PolicyValidationError => FailureClass::PolicyValidationError,
        ErrorCode::MissingCapability => FailureClass::MissingCapability,
        ErrorCode::MalformedWorkerResult => FailureClass::MalformedWorkerResult,
        ErrorCode::UserCancellation => FailureClass::UserCancellation,
        ErrorCode::StaleIdentityEvidence => FailureClass::StaleIdentityEvidence,
        ErrorCode::ClosureResolutionIncomplete => FailureClass::ClosureResolutionIncomplete,
        ErrorCode::BlockedByUseLease => FailureClass::BlockedByActiveUseLease,
        ErrorCode::ApprovalRequired => FailureClass::ApprovalRequired,
        ErrorCode::PriorityPolicyConflict => FailureClass::PriorityPolicyConflict,
        ErrorCode::AmbiguousWorkerSelection => FailureClass::AmbiguousWorkerSelection,
        _ => FailureClass::WorkerCrash,
    }
}

#[cfg(test)]
#[path = "events_test.rs"]
mod tests;
