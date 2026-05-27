use std::path::Path;

use voom_core::{ArtifactHandleId, ArtifactLocationId, FailureClass, FileLocationId, VoomError};
use voom_events::payload::{
    ArtifactRemuxFailedPayload, ArtifactRemuxProgressPayload, ArtifactRemuxStartedPayload,
    ArtifactRemuxStreamPayload, ArtifactRemuxSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_worker_protocol::{
    PercentBps, RemuxResult, RemuxSelection, RemuxStreamRef, RemuxTrackGroup,
};

use super::ExecuteRemuxInput;
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};
use sqlx::{Sqlite, Transaction};

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
            provider: None,
            provider_version: None,
        }),
    )
    .await?;
    commit_tx(tx).await
}

#[derive(Debug)]
pub struct RemuxProgressEventInput<'a> {
    pub input: &'a ExecuteRemuxInput,
    pub source_location_id: FileLocationId,
    pub selection: &'a RemuxSelection,
    pub staging_path: &'a Path,
    pub percent: Option<PercentBps>,
    pub message: Option<String>,
}

pub async fn record_progress(
    cp: &ControlPlane,
    event: RemuxProgressEventInput<'_>,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    append_event(
        &cp.events,
        &mut tx,
        SubjectType::FileVersion,
        Some(event.input.source_file_version_id.0),
        now,
        Event::ArtifactRemuxProgress(ArtifactRemuxProgressPayload {
            job_id: event.input.job_id.0,
            ticket_id: event.input.ticket_id.0,
            lease_id: Some(event.input.lease_id.0),
            source_file_version_id: event.input.source_file_version_id.0,
            source_file_location_id: event.source_location_id.0,
            staging_path: event.staging_path.display().to_string(),
            selected_streams: stream_payloads(&event.selection.keep_streams),
            default_streams: stream_payloads(&event.selection.default_streams),
            clear_default_streams: stream_payloads(&event.selection.clear_default_streams),
            percent_bps: event.percent.map(u16::from),
            message: event.message,
            provider: None,
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

#[derive(Debug, Clone)]
pub(crate) struct RemuxSucceededEvent {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactRemuxStreamPayload>,
    pub default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub clear_default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub kept_snapshot_stream_ids: Vec<String>,
    pub default_snapshot_stream_ids: Vec<String>,
    pub output_container: String,
    pub provider: String,
    pub provider_version: String,
}

impl RemuxSucceededEvent {
    pub(crate) fn from_input(event: &RemuxSucceededEventInput<'_>) -> Self {
        Self {
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
        }
    }
}

pub async fn record_succeeded(
    cp: &ControlPlane,
    event: RemuxSucceededEventInput<'_>,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let event = RemuxSucceededEvent::from_input(&event);
    append_succeeded_in_tx(cp, &mut tx, &event, now).await?;
    commit_tx(tx).await
}

pub(crate) async fn append_succeeded_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    event: &RemuxSucceededEvent,
    now: time::OffsetDateTime,
) -> Result<(), VoomError> {
    append_event(
        &cp.events,
        tx,
        SubjectType::ArtifactHandle,
        Some(event.artifact_handle_id),
        now,
        Event::ArtifactRemuxSucceeded(ArtifactRemuxSucceededPayload {
            job_id: event.job_id,
            ticket_id: event.ticket_id,
            lease_id: event.lease_id,
            source_file_version_id: event.source_file_version_id,
            source_file_location_id: event.source_file_location_id,
            artifact_handle_id: event.artifact_handle_id,
            artifact_location_id: event.artifact_location_id,
            staging_path: event.staging_path.clone(),
            selected_streams: event.selected_streams.clone(),
            default_streams: event.default_streams.clone(),
            clear_default_streams: event.clear_default_streams.clone(),
            kept_snapshot_stream_ids: event.kept_snapshot_stream_ids.clone(),
            default_snapshot_stream_ids: event.default_snapshot_stream_ids.clone(),
            output_container: event.output_container.clone(),
            provider: event.provider.clone(),
            provider_version: event.provider_version.clone(),
        }),
    )
    .await
}

#[derive(Debug)]
pub struct RemuxFailedEventInput<'a> {
    pub input: &'a ExecuteRemuxInput,
    pub source_location_id: Option<FileLocationId>,
    pub selection: Option<&'a RemuxSelection>,
    pub staging_path: Option<&'a Path>,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_location_id: Option<ArtifactLocationId>,
    pub result: Option<&'a RemuxResult>,
    pub error: &'a VoomError,
}

pub async fn record_failed(
    cp: &ControlPlane,
    event: RemuxFailedEventInput<'_>,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let subject_type = if event.artifact_handle_id.is_some() {
        SubjectType::ArtifactHandle
    } else {
        SubjectType::FileVersion
    };
    let subject_id = event
        .artifact_handle_id
        .map(|id| id.0)
        .or(Some(event.input.source_file_version_id.0));
    append_event(
        &cp.events,
        &mut tx,
        subject_type,
        subject_id,
        now,
        Event::ArtifactRemuxFailed(ArtifactRemuxFailedPayload {
            job_id: event.input.job_id.0,
            ticket_id: event.input.ticket_id.0,
            lease_id: Some(event.input.lease_id.0),
            source_file_version_id: event.input.source_file_version_id.0,
            source_file_location_id: event.source_location_id.map(|id| id.0),
            artifact_handle_id: event.artifact_handle_id.map(|id| id.0),
            artifact_location_id: event.artifact_location_id.map(|id| id.0),
            staging_path: event.path_string(),
            selected_streams: event.selection.map_or_else(Vec::new, |selection| {
                stream_payloads(&selection.keep_streams)
            }),
            default_streams: event.selection.map_or_else(Vec::new, |selection| {
                stream_payloads(&selection.default_streams)
            }),
            clear_default_streams: event.selection.map_or_else(Vec::new, |selection| {
                stream_payloads(&selection.clear_default_streams)
            }),
            failure_class: failure_class_for_error(event.error),
            error_code: event.error.code().to_owned(),
            message: event.error.to_string(),
            provider: event.result.map(|result| result.provider.clone()),
            provider_version: event.result.map(|result| result.provider_version.clone()),
        }),
    )
    .await?;
    commit_tx(tx).await
}

impl RemuxFailedEventInput<'_> {
    fn path_string(&self) -> Option<String> {
        self.staging_path.map(|path| path.display().to_string())
    }
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
    FailureClass::from_error_code(source.error_code()).unwrap_or(FailureClass::WorkerCrash)
}

#[cfg(test)]
#[path = "events_test.rs"]
mod tests;
