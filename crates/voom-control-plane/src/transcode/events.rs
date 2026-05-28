use std::path::Path;

use voom_core::{ArtifactHandleId, ArtifactLocationId, FileLocationId, VoomError};
use voom_events::payload::{ArtifactTranscodeStartedPayload, ArtifactTranscodeSucceededPayload};
use voom_events::{Event, SubjectType};
use voom_worker_protocol::TranscodeVideoResult;

use super::ExecuteTranscodeVideoInput;
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};

pub async fn record_started(
    cp: &ControlPlane,
    input: &ExecuteTranscodeVideoInput,
    source_location_id: FileLocationId,
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
        Event::ArtifactTranscodeStarted(ArtifactTranscodeStartedPayload {
            job_id: input.job_id.0,
            ticket_id: input.ticket_id.0,
            lease_id: Some(input.lease_id.0),
            source_file_version_id: input.source_file_version_id.0,
            source_file_location_id: source_location_id.0,
            staging_path: staging_path.display().to_string(),
            provider: Some("ffmpeg".to_owned()),
            provider_version: None,
        }),
    )
    .await?;
    commit_tx(tx).await
}

pub async fn record_succeeded(
    cp: &ControlPlane,
    input: &ExecuteTranscodeVideoInput,
    source_location_id: FileLocationId,
    artifact_handle_id: ArtifactHandleId,
    artifact_location_id: ArtifactLocationId,
    staging_path: &Path,
    result: &TranscodeVideoResult,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    append_event(
        &cp.events,
        &mut tx,
        SubjectType::ArtifactHandle,
        Some(artifact_handle_id.0),
        now,
        Event::ArtifactTranscodeSucceeded(ArtifactTranscodeSucceededPayload {
            job_id: input.job_id.0,
            ticket_id: input.ticket_id.0,
            lease_id: Some(input.lease_id.0),
            source_file_version_id: input.source_file_version_id.0,
            source_file_location_id: source_location_id.0,
            artifact_handle_id: artifact_handle_id.0,
            artifact_location_id: artifact_location_id.0,
            staging_path: staging_path.display().to_string(),
            output_container: result.output_container.clone(),
            output_video_codec: result.output_video_codec.clone(),
            provider: result.provider.clone(),
            provider_version: result.provider_version.clone(),
        }),
    )
    .await?;
    commit_tx(tx).await
}
