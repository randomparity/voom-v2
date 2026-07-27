use std::path::{Path, PathBuf};

use voom_core::VoomError;
use voom_worker_protocol::{
    AudioExpectedFacts, AudioObservedFacts, EXTRACT_AUDIO_CODEC, EXTRACT_AUDIO_CONTAINER,
    ExtractAudioInput, ExtractAudioOutput, ExtractAudioOutputDescriptor, ExtractAudioRequest,
    ExtractAudioResult, TRANSCODE_AUDIO_CONTAINER, TranscodeAudioInput, TranscodeAudioOutput,
    TranscodeAudioRequest, TranscodeAudioResult, TranscodeAudioSettings,
    validate_extract_audio_result,
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
            add_track: selection.add_track,
            target_channels: selection.target_channels,
        },
    }
}

pub fn extract_audio_request_for(
    selected: &SelectedSource,
    selection: &ExtractAudioSelectionPlan,
    staging_root: &Path,
    staging_paths: &[PathBuf],
) -> Result<ExtractAudioRequest, VoomError> {
    if selection.outputs.len() != staging_paths.len() {
        return Err(VoomError::Config(format!(
            "audio extraction selection/path count mismatch: {} selections, {} paths",
            selection.outputs.len(),
            staging_paths.len()
        )));
    }
    let descriptors = selection
        .outputs
        .iter()
        .zip(staging_paths)
        .map(
            |(selected_output, staging_path)| ExtractAudioOutputDescriptor {
                output_id: selected_output.output_id.clone().unwrap_or_default(),
                selection: selected_output.stream.clone(),
                output: extract_output(staging_root, staging_path),
            },
        )
        .collect::<Vec<_>>();
    let Some(first) = descriptors.first() else {
        return Err(VoomError::Config(
            "audio extraction request must contain at least one output".to_owned(),
        ));
    };
    let outputs = selection.operation_id.as_ref().map(|_| descriptors.clone());
    Ok(ExtractAudioRequest {
        input: ExtractAudioInput {
            path: selected.canonical_path.to_string_lossy().into_owned(),
            expected: expected_facts(selected),
        },
        output: first.output.clone(),
        selection: first.selection.clone(),
        outputs,
    })
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
    if result.output_audio_codecs.len() != selection.selected_streams.len()
        || result
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
    let mut provider_indexes = std::collections::HashSet::new();
    for (expected, actual) in selection
        .selected_streams
        .iter()
        .zip(&result.selected_output_streams)
    {
        let expected_channels = if selection.add_track {
            selection.target_channels
        } else {
            expected.source.channels.map(u64::from)
        };
        if actual.language != expected.source.language
            || actual.title != expected.source.title
            || actual.default != Some(expected.source.default)
            || actual.channels != expected_channels
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
            || !provider_indexes.insert(actual.output_provider_stream_index)
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
    request: &ExtractAudioRequest,
    result: &ExtractAudioResult,
) -> Result<(), VoomError> {
    validate_extract_audio_result(request, result)
        .map_err(|error| VoomError::MalformedWorkerResult(error.to_string()))?;
    validate_input_facts(selected, &result.input_pre, &result.input_post)?;
    let actual_outputs = match &result.outputs {
        Some(outputs) => outputs
            .iter()
            .map(|output| (&output.output_language, &output.output_title))
            .collect::<Vec<_>>(),
        None => vec![(&result.output_language, &result.output_title)],
    };
    for (expected, (language, title)) in selection.outputs.iter().zip(actual_outputs) {
        if expected.source.language.is_some() && language != &expected.source.language {
            return Err(VoomError::MalformedWorkerResult(
                "audio extract output language does not match source snapshot".to_owned(),
            ));
        }
        if expected.source.title.is_some() && title != &expected.source.title {
            return Err(VoomError::MalformedWorkerResult(
                "audio extract output title does not match source snapshot".to_owned(),
            ));
        }
    }
    Ok(())
}

pub async fn require_transcode_output_file_matches_result(
    staging_path: &Path,
    result: &TranscodeAudioResult,
) -> Result<(), VoomError> {
    require_output_file_matches_result(staging_path, &result.output).await
}

pub async fn require_extract_output_files_match_result(
    staging_paths: &[PathBuf],
    result: &ExtractAudioResult,
) -> Result<(), VoomError> {
    let observed = match &result.outputs {
        Some(outputs) => outputs.iter().map(|output| &output.output).collect(),
        None => vec![&result.output],
    };
    if staging_paths.len() != observed.len() {
        return Err(VoomError::MalformedWorkerResult(
            "audio extract staged path/result count mismatch".to_owned(),
        ));
    }
    for (path, facts) in staging_paths.iter().zip(observed) {
        require_output_file_matches_result(path, facts).await?;
    }
    Ok(())
}

pub fn extract_result_output_facts(result: &ExtractAudioResult) -> Vec<&AudioObservedFacts> {
    match &result.outputs {
        Some(outputs) => outputs.iter().map(|output| &output.output).collect(),
        None => vec![&result.output],
    }
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

fn extract_output(staging_root: &Path, staging_path: &Path) -> ExtractAudioOutput {
    ExtractAudioOutput {
        staging_root: staging_root.to_string_lossy().into_owned(),
        path: staging_path.to_string_lossy().into_owned(),
        container: EXTRACT_AUDIO_CONTAINER.to_owned(),
        audio_codec: EXTRACT_AUDIO_CODEC.to_owned(),
        overwrite: false,
    }
}

#[cfg(test)]
#[path = "worker_contract_test.rs"]
mod tests;
