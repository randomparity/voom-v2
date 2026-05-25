use serde_json::json;
use std::path::Path;

use voom_core::{ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, VoomError};
use voom_events::payload::ArtifactStagedPayload;
use voom_events::{Event, SubjectType};
use voom_store::repo::artifacts::{ArtifactRepo, NewArtifactHandle, NewArtifactLocation};
use voom_worker_protocol::TranscodeVideoResult;

use super::ExecuteTranscodeVideoInput;
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedTranscodeArtifact {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
}

pub async fn record_staged_transcode(
    cp: &ControlPlane,
    input: &ExecuteTranscodeVideoInput,
    source_file_location_id: FileLocationId,
    staging_path: &Path,
    result: &TranscodeVideoResult,
) -> Result<StagedTranscodeArtifact, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let handle = cp
        .artifacts
        .create_handle_in_tx(
            &mut tx,
            NewArtifactHandle {
                size_bytes: Some(i64::try_from(result.output.size_bytes).map_err(|err| {
                    VoomError::Internal(format!(
                        "transcode output size exceeds SQLite integer: {err}"
                    ))
                })?),
                checksum: Some(result.output.content_hash.clone()),
                privacy_class: "internal".to_owned(),
                durability_class: "staging".to_owned(),
                allowed_access_modes: vec!["local_path".to_owned()],
                mutability: "immutable".to_owned(),
                source_lineage: Some(json!({
                    "operation": "transcode_video",
                    "source_file_version_id": input.source_file_version_id.0,
                    "source_file_location_id": source_file_location_id.0,
                })),
                file_version_id: Some(input.source_file_version_id),
                created_at: now,
            },
        )
        .await?;
    let location = cp
        .artifacts
        .record_location_in_tx(
            &mut tx,
            NewArtifactLocation {
                artifact_handle_id: handle.id,
                kind: "staging".to_owned(),
                value: staging_path.display().to_string(),
                observed_at: now,
            },
        )
        .await?;
    append_event(
        &cp.events,
        &mut tx,
        SubjectType::ArtifactHandle,
        Some(handle.id.0),
        now,
        Event::ArtifactStaged(ArtifactStagedPayload {
            artifact_handle_id: handle.id.0,
            artifact_location_id: location.id.0,
            source_file_version_id: input.source_file_version_id.0,
            source_file_location_id: Some(source_file_location_id.0),
            staging_path: location.value.clone(),
            size_bytes: result.output.size_bytes,
            checksum: result.output.content_hash.clone(),
        }),
    )
    .await?;
    commit_tx(tx).await?;
    Ok(StagedTranscodeArtifact {
        artifact_handle_id: handle.id,
        artifact_location_id: location.id,
    })
}

pub async fn record_result_snapshot(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    result: &TranscodeVideoResult,
) -> Result<voom_store::repo::identity::MediaSnapshot, VoomError> {
    let payload = json!({
        "container": result.output_container,
        "video_codec": result.output_video_codec,
        "stream_summary": {"video": 1, "audio": null, "subtitle": null},
        "source": "transcode_video_result"
    });
    cp.record_media_snapshot(file_version_id, None, payload, cp.clock().now())
        .await
}
