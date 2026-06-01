use std::path::Path;

use voom_core::{ArtifactHandleId, ArtifactLocationId, FailureClass, FileLocationId, VoomError};
use voom_events::payload::{
    ArtifactAudioDispositionPayload, ArtifactAudioExtractFailedPayload,
    ArtifactAudioExtractStartedPayload, ArtifactAudioExtractSucceededPayload,
    ArtifactAudioOutputStreamPayload, ArtifactAudioStreamPayload,
    ArtifactAudioTranscodeFailedPayload, ArtifactAudioTranscodeStartedPayload,
    ArtifactAudioTranscodeSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_plan::audio::AudioBundleRole;
use voom_worker_protocol::{
    AudioDispositionFact, AudioOutputStreamFact, AudioStreamRef, ExtractAudioResult,
    TranscodeAudioResult,
};

use super::selection::{ExtractAudioSelectionPlan, TranscodeAudioSelectionPlan};
use super::{ExecuteExtractAudioInput, ExecuteTranscodeAudioInput};
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};

pub async fn record_transcode_started(
    cp: &ControlPlane,
    input: &ExecuteTranscodeAudioInput,
    source_location_id: FileLocationId,
    source_media_snapshot_id: u64,
    staging_path: &Path,
    selection: &TranscodeAudioSelectionPlan,
) -> Result<(), VoomError> {
    let payload = transcode_started_payload(
        input,
        source_location_id,
        source_media_snapshot_id,
        staging_path.display().to_string(),
        selection,
    );
    append_audio_event(
        cp,
        SubjectType::FileVersion,
        Some(input.source_file_version_id.0),
        Event::ArtifactAudioTranscodeStarted(payload),
    )
    .await
}

#[derive(Debug)]
pub struct TranscodeSucceededEventInput<'a> {
    pub input: &'a ExecuteTranscodeAudioInput,
    pub source_location_id: FileLocationId,
    pub source_media_snapshot_id: u64,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub selected_streams: Vec<ArtifactAudioStreamPayload>,
    pub result: &'a TranscodeAudioResult,
}

pub async fn record_transcode_succeeded(
    cp: &ControlPlane,
    event: TranscodeSucceededEventInput<'_>,
) -> Result<(), VoomError> {
    let staging_path = event
        .result
        .output
        .local_file_key
        .clone()
        .unwrap_or_default();
    let payload = transcode_succeeded_payload(
        event.input,
        TranscodeSucceededContext {
            source_location: event.source_location_id,
            source_media_snapshot: event.source_media_snapshot_id,
            artifact_handle: event.artifact_handle_id,
            artifact_location: event.artifact_location_id,
        },
        staging_path,
        event.selected_streams,
        event.result,
    );
    append_audio_event(
        cp,
        SubjectType::ArtifactHandle,
        Some(event.artifact_handle_id.0),
        Event::ArtifactAudioTranscodeSucceeded(payload),
    )
    .await
}

#[derive(Debug)]
pub struct TranscodeFailedEventInput<'a> {
    pub input: &'a ExecuteTranscodeAudioInput,
    pub source_location_id: Option<FileLocationId>,
    pub source_media_snapshot_id: Option<u64>,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_location_id: Option<ArtifactLocationId>,
    pub staging_path: Option<&'a Path>,
    pub selected_streams: Vec<ArtifactAudioStreamPayload>,
    pub result: Option<&'a TranscodeAudioResult>,
    pub error: &'a VoomError,
}

pub async fn record_transcode_failed(
    cp: &ControlPlane,
    event: TranscodeFailedEventInput<'_>,
) -> Result<(), VoomError> {
    let subject_type = if event.artifact_handle_id.is_some() {
        SubjectType::ArtifactHandle
    } else {
        SubjectType::FileVersion
    };
    let subject_id = event
        .artifact_handle_id
        .map(|id| id.0)
        .or(Some(event.input.source_file_version_id.0));
    append_audio_event(
        cp,
        subject_type,
        subject_id,
        Event::ArtifactAudioTranscodeFailed(transcode_failed_payload(
            TranscodeFailedEventPayloadInput {
                input: event.input,
                source_location_id: event.source_location_id,
                source_media_snapshot_id: event.source_media_snapshot_id,
                artifact_handle_id: event.artifact_handle_id,
                artifact_location_id: event.artifact_location_id,
                staging_path: event.staging_path.map(|path| path.display().to_string()),
                selected_streams: event.selected_streams,
                result: event.result,
                error: event.error,
            },
        )),
    )
    .await
}

pub async fn record_extract_started(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    source_media_snapshot_id: u64,
    staging_path: &Path,
    selection: &ExtractAudioSelectionPlan,
) -> Result<(), VoomError> {
    let payload = extract_started_payload(
        input,
        source_location_id,
        source_media_snapshot_id,
        staging_path.display().to_string(),
        selection,
    );
    append_audio_event(
        cp,
        SubjectType::FileVersion,
        Some(input.source_file_version_id.0),
        Event::ArtifactAudioExtractStarted(payload),
    )
    .await
}

#[derive(Debug)]
pub struct ExtractSucceededEventInput<'a> {
    pub input: &'a ExecuteExtractAudioInput,
    pub source_location_id: FileLocationId,
    pub source_media_snapshot_id: u64,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub selection: &'a ExtractAudioSelectionPlan,
    pub result: &'a ExtractAudioResult,
}

pub async fn record_extract_succeeded(
    cp: &ControlPlane,
    event: ExtractSucceededEventInput<'_>,
) -> Result<(), VoomError> {
    let staging_path = event
        .result
        .output
        .local_file_key
        .clone()
        .unwrap_or_default();
    append_audio_event(
        cp,
        SubjectType::ArtifactHandle,
        Some(event.artifact_handle_id.0),
        Event::ArtifactAudioExtractSucceeded(ArtifactAudioExtractSucceededPayload {
            job_id: event.input.job_id.0,
            ticket_id: event.input.ticket_id.0,
            lease_id: Some(event.input.lease_id.0),
            source_file_version_id: event.input.source_file_version_id.0,
            source_file_location_id: event.source_location_id.0,
            source_media_snapshot_id: event.source_media_snapshot_id,
            source_bundle_id: event.input.source_bundle_id.0,
            artifact_handle_id: event.artifact_handle_id.0,
            artifact_location_id: event.artifact_location_id.0,
            staging_path,
            selected_stream: stream_payload(&event.selection.stream),
            selected_snapshot_stream_id: event.result.selected_snapshot_stream_id.clone(),
            role: role_name(event.selection.role).to_owned(),
            output_container: event.result.output_container.clone(),
            output_audio_codec: event.result.output_audio_codec.clone(),
            provider: event.result.provider.clone(),
            provider_version: event.result.provider_version.clone(),
        }),
    )
    .await
}

#[derive(Debug)]
pub struct ExtractFailedEventInput<'a> {
    pub input: &'a ExecuteExtractAudioInput,
    pub source_location_id: Option<FileLocationId>,
    pub source_media_snapshot_id: Option<u64>,
    pub selection: Option<&'a ExtractAudioSelectionPlan>,
    pub staging_path: Option<&'a Path>,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_location_id: Option<ArtifactLocationId>,
    pub result: Option<&'a ExtractAudioResult>,
    pub error: &'a VoomError,
}

pub async fn record_extract_failed(
    cp: &ControlPlane,
    event: ExtractFailedEventInput<'_>,
) -> Result<(), VoomError> {
    let payload = extract_failed_payload(ExtractFailedEventPayloadInput {
        input: event.input,
        source_location_id: event.source_location_id,
        source_media_snapshot_id: event.source_media_snapshot_id,
        artifact_handle_id: event.artifact_handle_id,
        artifact_location_id: event.artifact_location_id,
        staging_path: event.staging_path.map(|path| path.display().to_string()),
        selected_stream: event
            .selection
            .map(|selection| stream_payload(&selection.stream)),
        role: event
            .selection
            .map(|selection| role_name(selection.role).to_owned()),
        result: event.result,
        error: event.error,
    });
    let subject_type = if event.artifact_handle_id.is_some() {
        SubjectType::ArtifactHandle
    } else {
        SubjectType::FileVersion
    };
    let subject_id = event
        .artifact_handle_id
        .map(|id| id.0)
        .or(Some(event.input.source_file_version_id.0));
    append_audio_event(
        cp,
        subject_type,
        subject_id,
        Event::ArtifactAudioExtractFailed(payload),
    )
    .await
}

fn transcode_started_payload(
    input: &ExecuteTranscodeAudioInput,
    source_location_id: FileLocationId,
    source_media_snapshot_id: u64,
    staging_path: String,
    selection: &TranscodeAudioSelectionPlan,
) -> ArtifactAudioTranscodeStartedPayload {
    ArtifactAudioTranscodeStartedPayload {
        job_id: input.job_id.0,
        ticket_id: input.ticket_id.0,
        lease_id: Some(input.lease_id.0),
        source_file_version_id: input.source_file_version_id.0,
        source_file_location_id: source_location_id.0,
        source_media_snapshot_id,
        staging_path,
        selected_streams: stream_payloads(&selection.selection.selected_streams),
        target_codec: selection.target_codec.clone(),
        output_container: selection.container.clone(),
        provider: Some("ffmpeg".to_owned()),
        provider_version: None,
    }
}

fn transcode_succeeded_payload(
    input: &ExecuteTranscodeAudioInput,
    context: TranscodeSucceededContext,
    staging_path: String,
    selected_streams: Vec<ArtifactAudioStreamPayload>,
    result: &TranscodeAudioResult,
) -> ArtifactAudioTranscodeSucceededPayload {
    ArtifactAudioTranscodeSucceededPayload {
        job_id: input.job_id.0,
        ticket_id: input.ticket_id.0,
        lease_id: Some(input.lease_id.0),
        source_file_version_id: input.source_file_version_id.0,
        source_file_location_id: context.source_location.0,
        source_media_snapshot_id: context.source_media_snapshot,
        artifact_handle_id: context.artifact_handle.0,
        artifact_location_id: context.artifact_location.0,
        staging_path,
        selected_streams,
        selected_snapshot_stream_ids: result.selected_snapshot_stream_ids.clone(),
        selected_output_streams: output_stream_payloads(&result.selected_output_streams),
        output_container: result.output_container.clone(),
        output_audio_codecs: result.output_audio_codecs.clone(),
        provider: result.provider.clone(),
        provider_version: result.provider_version.clone(),
    }
}

struct TranscodeFailedEventPayloadInput<'a> {
    input: &'a ExecuteTranscodeAudioInput,
    source_location_id: Option<FileLocationId>,
    source_media_snapshot_id: Option<u64>,
    artifact_handle_id: Option<ArtifactHandleId>,
    artifact_location_id: Option<ArtifactLocationId>,
    staging_path: Option<String>,
    selected_streams: Vec<ArtifactAudioStreamPayload>,
    result: Option<&'a TranscodeAudioResult>,
    error: &'a VoomError,
}

fn transcode_failed_payload(
    event: TranscodeFailedEventPayloadInput<'_>,
) -> ArtifactAudioTranscodeFailedPayload {
    ArtifactAudioTranscodeFailedPayload {
        job_id: event.input.job_id.0,
        ticket_id: event.input.ticket_id.0,
        lease_id: Some(event.input.lease_id.0),
        source_file_version_id: event.input.source_file_version_id.0,
        source_file_location_id: event.source_location_id.map(|id| id.0),
        source_media_snapshot_id: event.source_media_snapshot_id,
        artifact_handle_id: event.artifact_handle_id.map(|id| id.0),
        artifact_location_id: event.artifact_location_id.map(|id| id.0),
        staging_path: event.staging_path,
        selected_streams: event.selected_streams,
        selected_output_streams: event
            .result
            .map(|result| output_stream_payloads(&result.selected_output_streams))
            .unwrap_or_default(),
        failure_class: failure_class_for_error(event.error),
        error_code: event.error.code().to_owned(),
        message: event.error.to_string(),
        provider: event.result.map(|result| result.provider.clone()),
        provider_version: event.result.map(|result| result.provider_version.clone()),
    }
}

#[derive(Debug, Clone, Copy)]
struct TranscodeSucceededContext {
    source_location: FileLocationId,
    source_media_snapshot: u64,
    artifact_handle: ArtifactHandleId,
    artifact_location: ArtifactLocationId,
}

fn extract_started_payload(
    input: &ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    source_media_snapshot_id: u64,
    staging_path: String,
    selection: &ExtractAudioSelectionPlan,
) -> ArtifactAudioExtractStartedPayload {
    ArtifactAudioExtractStartedPayload {
        job_id: input.job_id.0,
        ticket_id: input.ticket_id.0,
        lease_id: Some(input.lease_id.0),
        source_file_version_id: input.source_file_version_id.0,
        source_file_location_id: source_location_id.0,
        source_media_snapshot_id,
        source_bundle_id: input.source_bundle_id.0,
        staging_path,
        selected_stream: stream_payload(&selection.stream),
        role: role_name(selection.role).to_owned(),
        target_codec: selection.target_codec.clone(),
        output_container: selection.container.clone(),
        provider: Some("ffmpeg".to_owned()),
        provider_version: None,
    }
}

struct ExtractFailedEventPayloadInput<'a> {
    input: &'a ExecuteExtractAudioInput,
    source_location_id: Option<FileLocationId>,
    source_media_snapshot_id: Option<u64>,
    artifact_handle_id: Option<ArtifactHandleId>,
    artifact_location_id: Option<ArtifactLocationId>,
    staging_path: Option<String>,
    selected_stream: Option<ArtifactAudioStreamPayload>,
    role: Option<String>,
    result: Option<&'a ExtractAudioResult>,
    error: &'a VoomError,
}

fn extract_failed_payload(
    event: ExtractFailedEventPayloadInput<'_>,
) -> ArtifactAudioExtractFailedPayload {
    ArtifactAudioExtractFailedPayload {
        job_id: event.input.job_id.0,
        ticket_id: event.input.ticket_id.0,
        lease_id: Some(event.input.lease_id.0),
        source_file_version_id: event.input.source_file_version_id.0,
        source_file_location_id: event.source_location_id.map(|id| id.0),
        source_media_snapshot_id: event.source_media_snapshot_id,
        source_bundle_id: event.input.source_bundle_id.0,
        artifact_handle_id: event.artifact_handle_id.map(|id| id.0),
        artifact_location_id: event.artifact_location_id.map(|id| id.0),
        staging_path: event.staging_path,
        selected_stream: event.selected_stream,
        role: event.role,
        failure_class: failure_class_for_error(event.error),
        error_code: event.error.code().to_owned(),
        message: event.error.to_string(),
        provider: event.result.map(|result| result.provider.clone()),
        provider_version: event.result.map(|result| result.provider_version.clone()),
    }
}

async fn append_audio_event(
    cp: &ControlPlane,
    subject_type: SubjectType,
    subject_id: Option<u64>,
    payload: Event,
) -> Result<(), VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    append_event(&cp.events, &mut tx, subject_type, subject_id, now, payload).await?;
    commit_tx(tx).await
}

pub(crate) fn stream_payloads(streams: &[AudioStreamRef]) -> Vec<ArtifactAudioStreamPayload> {
    streams.iter().map(stream_payload).collect()
}

fn stream_payload(stream: &AudioStreamRef) -> ArtifactAudioStreamPayload {
    ArtifactAudioStreamPayload {
        snapshot_stream_id: stream.snapshot_stream_id.clone(),
        provider_stream_index: stream.provider_stream_index,
    }
}

fn output_stream_payloads(
    streams: &[AudioOutputStreamFact],
) -> Vec<ArtifactAudioOutputStreamPayload> {
    streams.iter().map(output_stream_payload).collect()
}

fn output_stream_payload(stream: &AudioOutputStreamFact) -> ArtifactAudioOutputStreamPayload {
    ArtifactAudioOutputStreamPayload {
        snapshot_stream_id: stream.snapshot_stream_id.clone(),
        output_provider_stream_index: stream.output_provider_stream_index,
        codec: stream.codec.clone(),
        language: stream.language.clone(),
        title: stream.title.clone(),
        default: stream.default,
        disposition: stream.disposition.as_ref().map(disposition_payload),
        channels: stream.channels,
    }
}

fn disposition_payload(disposition: &AudioDispositionFact) -> ArtifactAudioDispositionPayload {
    ArtifactAudioDispositionPayload {
        default: disposition.default,
        forced: disposition.forced,
        commentary: disposition.commentary,
    }
}

fn role_name(role: AudioBundleRole) -> &'static str {
    match role {
        AudioBundleRole::CommentaryAudio => "commentary_audio",
        AudioBundleRole::ExternalAudio => "external_audio",
    }
}

fn failure_class_for_error(source: &VoomError) -> FailureClass {
    FailureClass::from_error_code(source.error_code()).unwrap_or(FailureClass::WorkerCrash)
}

#[cfg(test)]
#[path = "events_test.rs"]
mod tests;
