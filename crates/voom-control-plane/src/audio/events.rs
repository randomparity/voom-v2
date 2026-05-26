use std::path::Path;

use voom_core::{
    ArtifactHandleId, ArtifactLocationId, ErrorCode, FailureClass, FileLocationId, VoomError,
};
use voom_events::payload::{
    ArtifactAudioExtractFailedPayload, ArtifactAudioExtractProgressPayload,
    ArtifactAudioExtractStartedPayload, ArtifactAudioExtractSucceededPayload,
    ArtifactAudioStreamPayload, ArtifactAudioTranscodeFailedPayload,
    ArtifactAudioTranscodeProgressPayload, ArtifactAudioTranscodeStartedPayload,
    ArtifactAudioTranscodeSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_plan::audio::AudioBundleRole;
use voom_worker_protocol::{AudioStreamRef, ExtractAudioResult, PercentBps, TranscodeAudioResult};

use super::selection::{ExtractAudioSelectionPlan, TranscodeAudioSelectionPlan};
use super::{ExecuteExtractAudioInput, ExecuteTranscodeAudioInput};
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};

pub async fn record_transcode_started(
    cp: &ControlPlane,
    input: &ExecuteTranscodeAudioInput,
    source_location_id: FileLocationId,
    staging_path: &Path,
) -> Result<(), VoomError> {
    let snapshot = super::source::read_media_snapshot(
        cp,
        input.source_file_version_id,
        &input.operation_payload,
    )
    .await?;
    let selection = super::selection::transcode_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )?;
    let payload = transcode_started_payload(
        input,
        source_location_id,
        snapshot.id.0,
        staging_path.display().to_string(),
        &selection,
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
pub struct TranscodeProgressEventInput<'a> {
    pub input: &'a ExecuteTranscodeAudioInput,
    pub source_location_id: FileLocationId,
    pub source_media_snapshot_id: u64,
    pub selection: &'a TranscodeAudioSelectionPlan,
    pub staging_path: &'a Path,
    pub percent: Option<PercentBps>,
    pub message: Option<String>,
}

pub async fn record_transcode_progress(
    cp: &ControlPlane,
    event: TranscodeProgressEventInput<'_>,
) -> Result<(), VoomError> {
    append_audio_event(
        cp,
        SubjectType::FileVersion,
        Some(event.input.source_file_version_id.0),
        Event::ArtifactAudioTranscodeProgress(ArtifactAudioTranscodeProgressPayload {
            job_id: event.input.job_id.0,
            ticket_id: event.input.ticket_id.0,
            lease_id: Some(event.input.lease_id.0),
            source_file_version_id: event.input.source_file_version_id.0,
            source_file_location_id: event.source_location_id.0,
            source_media_snapshot_id: event.source_media_snapshot_id,
            staging_path: event.staging_path.display().to_string(),
            selected_streams: stream_payloads(&event.selection.selection.selected_streams),
            percent_bps: event.percent.map(u16::from),
            message: event.message,
            provider: Some("ffmpeg".to_owned()),
            provider_version: None,
        }),
    )
    .await
}

pub async fn record_transcode_succeeded(
    cp: &ControlPlane,
    input: &ExecuteTranscodeAudioInput,
    source_location_id: FileLocationId,
    artifact_handle_id: ArtifactHandleId,
    artifact_location_id: ArtifactLocationId,
    result: &TranscodeAudioResult,
) -> Result<(), VoomError> {
    let snapshot = super::source::read_media_snapshot(
        cp,
        input.source_file_version_id,
        &input.operation_payload,
    )
    .await?;
    let selection = super::selection::transcode_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )?;
    let staging_path = result.output.local_file_key.clone().unwrap_or_default();
    let payload = transcode_succeeded_payload(
        input,
        TranscodeSucceededContext {
            source_location: source_location_id,
            source_media_snapshot: snapshot.id.0,
            artifact_handle: artifact_handle_id,
            artifact_location: artifact_location_id,
        },
        staging_path,
        stream_payloads(&selection.selection.selected_streams),
        result,
    );
    append_audio_event(
        cp,
        SubjectType::ArtifactHandle,
        Some(artifact_handle_id.0),
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
        Event::ArtifactAudioTranscodeFailed(ArtifactAudioTranscodeFailedPayload {
            job_id: event.input.job_id.0,
            ticket_id: event.input.ticket_id.0,
            lease_id: Some(event.input.lease_id.0),
            source_file_version_id: event.input.source_file_version_id.0,
            source_file_location_id: event.source_location_id.map(|id| id.0),
            source_media_snapshot_id: event.source_media_snapshot_id,
            artifact_handle_id: event.artifact_handle_id.map(|id| id.0),
            artifact_location_id: event.artifact_location_id.map(|id| id.0),
            staging_path: event.staging_path.map(|path| path.display().to_string()),
            selected_streams: event.selected_streams,
            failure_class: failure_class_for_error(event.error),
            error_code: event.error.code().to_owned(),
            message: event.error.to_string(),
            provider: event.result.map(|result| result.provider.clone()),
            provider_version: event.result.map(|result| result.provider_version.clone()),
        }),
    )
    .await
}

pub async fn record_extract_started(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    staging_path: &Path,
) -> Result<(), VoomError> {
    let snapshot = super::source::read_media_snapshot(
        cp,
        input.source_file_version_id,
        &input.operation_payload,
    )
    .await?;
    let selection = super::selection::extract_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )?;
    let payload = extract_started_payload(
        input,
        source_location_id,
        snapshot.id.0,
        staging_path.display().to_string(),
        &selection,
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
pub struct ExtractProgressEventInput<'a> {
    pub input: &'a ExecuteExtractAudioInput,
    pub source_location_id: FileLocationId,
    pub source_media_snapshot_id: u64,
    pub selection: &'a ExtractAudioSelectionPlan,
    pub staging_path: &'a Path,
    pub percent: Option<PercentBps>,
    pub message: Option<String>,
}

pub async fn record_extract_progress(
    cp: &ControlPlane,
    event: ExtractProgressEventInput<'_>,
) -> Result<(), VoomError> {
    append_audio_event(
        cp,
        SubjectType::FileVersion,
        Some(event.input.source_file_version_id.0),
        Event::ArtifactAudioExtractProgress(ArtifactAudioExtractProgressPayload {
            job_id: event.input.job_id.0,
            ticket_id: event.input.ticket_id.0,
            lease_id: Some(event.input.lease_id.0),
            source_file_version_id: event.input.source_file_version_id.0,
            source_file_location_id: event.source_location_id.0,
            source_media_snapshot_id: event.source_media_snapshot_id,
            source_bundle_id: event.input.source_bundle_id.0,
            staging_path: event.staging_path.display().to_string(),
            selected_stream: stream_payload(&event.selection.stream),
            percent_bps: event.percent.map(u16::from),
            message: event.message,
            provider: Some("ffmpeg".to_owned()),
            provider_version: None,
        }),
    )
    .await
}

pub async fn record_extract_succeeded(
    cp: &ControlPlane,
    input: &ExecuteExtractAudioInput,
    source_location_id: FileLocationId,
    artifact_handle_id: ArtifactHandleId,
    artifact_location_id: ArtifactLocationId,
    result: &ExtractAudioResult,
) -> Result<(), VoomError> {
    let snapshot = super::source::read_media_snapshot(
        cp,
        input.source_file_version_id,
        &input.operation_payload,
    )
    .await?;
    let selection = super::selection::extract_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )?;
    let staging_path = result.output.local_file_key.clone().unwrap_or_default();
    append_audio_event(
        cp,
        SubjectType::ArtifactHandle,
        Some(artifact_handle_id.0),
        Event::ArtifactAudioExtractSucceeded(ArtifactAudioExtractSucceededPayload {
            job_id: input.job_id.0,
            ticket_id: input.ticket_id.0,
            lease_id: Some(input.lease_id.0),
            source_file_version_id: input.source_file_version_id.0,
            source_file_location_id: source_location_id.0,
            source_media_snapshot_id: snapshot.id.0,
            source_bundle_id: input.source_bundle_id.0,
            artifact_handle_id: artifact_handle_id.0,
            artifact_location_id: artifact_location_id.0,
            staging_path,
            selected_stream: stream_payload(&selection.stream),
            selected_snapshot_stream_id: result.selected_snapshot_stream_id.clone(),
            role: role_name(selection.role).to_owned(),
            output_container: result.output_container.clone(),
            output_audio_codec: result.output_audio_codec.clone(),
            provider: result.provider.clone(),
            provider_version: result.provider_version.clone(),
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
        output_container: result.output_container.clone(),
        output_audio_codecs: result.output_audio_codecs.clone(),
        provider: result.provider.clone(),
        provider_version: result.provider_version.clone(),
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

fn stream_payloads(streams: &[AudioStreamRef]) -> Vec<ArtifactAudioStreamPayload> {
    streams.iter().map(stream_payload).collect()
}

fn stream_payload(stream: &AudioStreamRef) -> ArtifactAudioStreamPayload {
    ArtifactAudioStreamPayload {
        snapshot_stream_id: stream.snapshot_stream_id.clone(),
        provider_stream_index: stream.provider_stream_index,
    }
}

fn role_name(role: AudioBundleRole) -> &'static str {
    match role {
        AudioBundleRole::CommentaryAudio => "commentary_audio",
        AudioBundleRole::ExternalAudio => "external_audio",
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
