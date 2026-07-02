use std::path::Path;

use voom_core::VoomError;
use voom_worker_protocol::{
    AudioExpectedFacts, AudioObservedFacts, EXTRACT_AUDIO_CODEC, EXTRACT_AUDIO_CONTAINER,
    ExtractAudioInput, ExtractAudioOutput, ExtractAudioRequest, ExtractAudioResult,
    TRANSCODE_AUDIO_CONTAINER, TranscodeAudioInput, TranscodeAudioOutput, TranscodeAudioRequest,
    TranscodeAudioResult, TranscodeAudioSettings,
};

use super::selection::{ExtractAudioSelectionPlan, TranscodeAudioSelectionPlan};
use super::source::SelectedSource;
use crate::artifact::fs::observe_regular_file;

pub fn transcode_audio_request_for(
    selected: &SelectedSource,
    selection: &TranscodeAudioSelectionPlan,
    staging_root: &Path,
    staging_path: &Path,
) -> TranscodeAudioRequest {
    TranscodeAudioRequest {
        input: TranscodeAudioInput {
            path: selected.canonical_path.to_string_lossy().into_owned(),
            expected: expected_facts(selected),
        },
        output: TranscodeAudioOutput {
            staging_root: staging_root.to_string_lossy().into_owned(),
            path: staging_path.to_string_lossy().into_owned(),
            container: TRANSCODE_AUDIO_CONTAINER.to_owned(),
            overwrite: false,
        },
        selection: selection.selection.clone(),
        audio: TranscodeAudioSettings {
            target_codec: selection.target_codec.clone(),
            profile: "default".to_owned(),
            // Transcode replaces streams in place. `synthesize audio` (ADR 0026)
            // sets these to add a downmixed companion; that execute-path wiring
            // is a follow-up (see PR / issue #276).
            add_track: false,
            target_channels: None,
        },
    }
}

pub fn extract_audio_request_for(
    selected: &SelectedSource,
    selection: &ExtractAudioSelectionPlan,
    staging_root: &Path,
    staging_path: &Path,
) -> ExtractAudioRequest {
    ExtractAudioRequest {
        input: ExtractAudioInput {
            path: selected.canonical_path.to_string_lossy().into_owned(),
            expected: expected_facts(selected),
        },
        output: ExtractAudioOutput {
            staging_root: staging_root.to_string_lossy().into_owned(),
            path: staging_path.to_string_lossy().into_owned(),
            container: EXTRACT_AUDIO_CONTAINER.to_owned(),
            audio_codec: EXTRACT_AUDIO_CODEC.to_owned(),
            overwrite: false,
        },
        selection: selection.stream.clone(),
    }
}

pub async fn revalidate_source_file(selected: &SelectedSource) -> Result<(), VoomError> {
    let facts = observe_regular_file(&selected.canonical_path).await?;
    if facts.size_bytes != selected.version.size_bytes
        || facts.content_hash != selected.version.content_hash
    {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "audio source facts do not match selected file_version at {}",
            selected.location.value
        )));
    }
    Ok(())
}

pub fn validate_transcode_result(
    selected: &SelectedSource,
    selection: &TranscodeAudioSelectionPlan,
    result: &TranscodeAudioResult,
) -> Result<(), VoomError> {
    validate_input_facts(selected, &result.input_pre, &result.input_post)?;
    if result.output_container != TRANSCODE_AUDIO_CONTAINER {
        return Err(VoomError::MalformedWorkerResult(format!(
            "audio transcode result expected mkv, got {}",
            result.output_container
        )));
    }
    let selected_ids = selection
        .selection
        .selected_streams
        .iter()
        .map(|stream| stream.snapshot_stream_id.as_str());
    if !result
        .selected_snapshot_stream_ids
        .iter()
        .map(String::as_str)
        .eq(selected_ids)
    {
        return Err(VoomError::MalformedWorkerResult(
            "audio transcode selected stream ids do not match request".to_owned(),
        ));
    }
    if result.selected_output_streams.len() != selection.selected_streams.len()
        || !result
            .selected_output_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .eq(selection
                .selected_streams
                .iter()
                .map(|stream| stream.stream.snapshot_stream_id.as_str()))
    {
        return Err(VoomError::MalformedWorkerResult(
            "audio transcode selected output stream ordering does not match request".to_owned(),
        ));
    }
    if result
        .output_audio_codecs
        .iter()
        .any(|codec| codec != &selection.target_codec)
        || result
            .selected_output_streams
            .iter()
            .any(|stream| stream.codec != selection.target_codec)
    {
        return Err(VoomError::MalformedWorkerResult(
            "audio transcode output codec does not match request".to_owned(),
        ));
    }
    for (expected, actual) in selection
        .selected_streams
        .iter()
        .zip(&result.selected_output_streams)
    {
        if actual.language != expected.source.language
            || actual.title != expected.source.title
            || actual.default != Some(expected.source.default)
            || actual.channels != expected.source.channels.map(u64::from)
            || actual
                .disposition
                .as_ref()
                .and_then(|disposition| disposition.default)
                != Some(expected.source.disposition.default)
            || actual
                .disposition
                .as_ref()
                .and_then(|disposition| disposition.forced)
                != Some(expected.source.disposition.forced)
            || actual
                .disposition
                .as_ref()
                .and_then(|disposition| disposition.commentary)
                != expected.source.disposition.commentary
        {
            return Err(VoomError::MalformedWorkerResult(
                "audio transcode preserved stream facts do not match source snapshot".to_owned(),
            ));
        }
    }
    Ok(())
}

pub fn validate_extract_result(
    selected: &SelectedSource,
    selection: &ExtractAudioSelectionPlan,
    result: &ExtractAudioResult,
) -> Result<(), VoomError> {
    validate_input_facts(selected, &result.input_pre, &result.input_post)?;
    if result.output_container != EXTRACT_AUDIO_CONTAINER
        || result.output_audio_codec != EXTRACT_AUDIO_CODEC
    {
        return Err(VoomError::MalformedWorkerResult(format!(
            "audio extract result expected ogg/opus, got {}/{}",
            result.output_container, result.output_audio_codec
        )));
    }
    if result.selected_snapshot_stream_id != selection.stream.snapshot_stream_id {
        return Err(VoomError::MalformedWorkerResult(
            "audio extract selected stream id does not match request".to_owned(),
        ));
    }
    if selection.source.language.is_some() && result.output_language != selection.source.language {
        return Err(VoomError::MalformedWorkerResult(
            "audio extract output language does not match source snapshot".to_owned(),
        ));
    }
    if selection.source.title.is_some() && result.output_title != selection.source.title {
        return Err(VoomError::MalformedWorkerResult(
            "audio extract output title does not match source snapshot".to_owned(),
        ));
    }
    Ok(())
}

pub async fn require_transcode_output_file_matches_result(
    staging_path: &Path,
    result: &TranscodeAudioResult,
) -> Result<(), VoomError> {
    require_output_file_matches_result(staging_path, &result.output).await
}

pub async fn require_extract_output_file_matches_result(
    staging_path: &Path,
    result: &ExtractAudioResult,
) -> Result<(), VoomError> {
    require_output_file_matches_result(staging_path, &result.output).await
}

async fn require_output_file_matches_result(
    staging_path: &Path,
    result: &AudioObservedFacts,
) -> Result<(), VoomError> {
    let facts = observe_regular_file(staging_path).await?;
    if facts.size_bytes != result.size_bytes || facts.content_hash != result.content_hash {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "audio output facts do not match staged file {}",
            staging_path.display()
        )));
    }
    Ok(())
}

fn validate_input_facts(
    selected: &SelectedSource,
    input_pre: &AudioObservedFacts,
    input_post: &AudioObservedFacts,
) -> Result<(), VoomError> {
    if input_pre != input_post {
        return Err(VoomError::ArtifactChecksumMismatch(
            "audio source changed during worker execution".to_owned(),
        ));
    }
    if input_pre.size_bytes != selected.version.size_bytes
        || input_pre.content_hash != selected.version.content_hash
    {
        return Err(VoomError::ArtifactChecksumMismatch(
            "audio source facts do not match selected file_version".to_owned(),
        ));
    }
    Ok(())
}

fn expected_facts(selected: &SelectedSource) -> AudioExpectedFacts {
    AudioExpectedFacts {
        size_bytes: selected.version.size_bytes,
        content_hash: selected.version.content_hash.clone(),
        modified_at: None,
        local_file_key: None,
    }
}

#[cfg(test)]
#[path = "worker_contract_test.rs"]
mod tests;
