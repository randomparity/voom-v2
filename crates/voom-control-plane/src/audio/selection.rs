use serde_json::Value;
use voom_core::VoomError;
use voom_plan::audio::{
    AUDIO_EXTRACT_CODEC, AUDIO_EXTRACT_CONTAINER, AUDIO_TRANSCODE_CONTAINER, AudioBundleRole,
    AudioOperationPayload, AudioOperationType, AudioPlanningBlock, SnapshotAudioStreamFact,
    extract_audio_outputs, extraction_role, selected_audio_streams,
};
use voom_store::repo::identity::MediaSnapshot;
use voom_worker_protocol::{AudioStreamRef, TranscodeAudioSelection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedAudioStream {
    pub stream: AudioStreamRef,
    pub source: SnapshotAudioStreamFact,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeAudioSelectionPlan {
    pub selection: TranscodeAudioSelection,
    pub selected_streams: Vec<SelectedAudioStream>,
    pub target_codec: String,
    pub container: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractAudioSelectionPlan {
    pub operation_id: Option<String>,
    pub output_id: Option<String>,
    pub name_suffix: Option<String>,
    pub output_count: usize,
    pub stream: AudioStreamRef,
    pub source: SnapshotAudioStreamFact,
    pub role: AudioBundleRole,
    pub target_codec: String,
    pub container: String,
}

struct ResolvedExtractSelection {
    operation_id: Option<String>,
    output_id: Option<String>,
    name_suffix: Option<String>,
    output_count: usize,
    source: SnapshotAudioStreamFact,
    role: AudioBundleRole,
}

pub fn transcode_selection_from_payload_and_snapshot(
    payload: &Value,
    snapshot: &MediaSnapshot,
) -> Result<TranscodeAudioSelectionPlan, VoomError> {
    let payload = parse_payload(payload)?;
    if payload.operation_type == AudioOperationType::SynthesizeAudio {
        // Synthesis compiles and plans (ADR 0026) but the execute path that
        // builds a downmix worker request and registers the derived track's
        // lineage is not wired yet. Fail loud and clearly rather than silently
        // running the source through the replace-in-place transcode path.
        return Err(VoomError::Config(
            "synthesize_audio execution is not yet supported".to_owned(),
        ));
    }
    if payload.operation_type != AudioOperationType::TranscodeAudio {
        return Err(VoomError::Config(
            "audio transcode payload type must be transcode_audio".to_owned(),
        ));
    }
    if payload.container != AUDIO_TRANSCODE_CONTAINER {
        return Err(VoomError::Config(format!(
            "audio transcode container {} is unsupported",
            payload.container
        )));
    }
    let snapshot_input = crate::media_snapshot::planning_input(1, snapshot);
    let selected = selected_audio_streams(&snapshot_input, payload.filter.as_ref())
        .map_err(audio_block_error)?;
    if selected.is_empty() {
        return Err(audio_block_error(AudioPlanningBlock::ZeroMatches));
    }
    let selected_streams = selected
        .into_iter()
        .map(|source| SelectedAudioStream {
            stream: stream_ref(&source),
            source,
        })
        .collect::<Vec<_>>();
    Ok(TranscodeAudioSelectionPlan {
        selection: TranscodeAudioSelection {
            selected_streams: selected_streams
                .iter()
                .map(|selected| selected.stream.clone())
                .collect(),
        },
        selected_streams,
        target_codec: payload.target_codec,
        container: payload.container,
    })
}

pub fn extract_selection_from_payload_and_snapshot(
    payload: &Value,
    snapshot: &MediaSnapshot,
) -> Result<ExtractAudioSelectionPlan, VoomError> {
    let payload = parse_payload(payload)?;
    if payload.operation_type != AudioOperationType::ExtractAudio {
        return Err(VoomError::Config(
            "audio extract payload type must be extract_audio".to_owned(),
        ));
    }
    if payload.container != AUDIO_EXTRACT_CONTAINER || payload.target_codec != AUDIO_EXTRACT_CODEC {
        return Err(VoomError::Config(format!(
            "audio extract expected ogg/opus, got {}/{}",
            payload.container, payload.target_codec
        )));
    }
    let snapshot_input = crate::media_snapshot::planning_input(1, snapshot);
    let selected = selected_audio_streams(&snapshot_input, payload.filter.as_ref())
        .map_err(audio_block_error)?;
    if selected.is_empty() {
        return Err(audio_block_error(AudioPlanningBlock::ZeroMatches));
    }
    let resolved = match (&payload.operation_id, &payload.outputs) {
        (None, None) => legacy_extract_selection(&selected)?,
        (Some(operation_id), Some(outputs)) => {
            let expected = extract_audio_outputs(
                &snapshot_input,
                payload.filter.as_ref(),
                operation_id,
                &payload.target_codec,
            )
            .map_err(audio_block_error)?;
            if outputs != &expected {
                return Err(VoomError::Config(
                    "audio extract outputs do not match the pinned source snapshot".to_owned(),
                ));
            }
            let Some(first) = expected.first() else {
                return Err(VoomError::Config(
                    "audio extract outputs must not be empty".to_owned(),
                ));
            };
            let source = selected
                .into_iter()
                .find(|source| {
                    source.snapshot_stream_id == first.source_snapshot_stream_id
                        && source.provider_stream_index == first.source_provider_stream_index
                })
                .ok_or_else(|| {
                    VoomError::Config(
                        "audio extract first output does not identify a selected source stream"
                            .to_owned(),
                    )
                })?;
            ResolvedExtractSelection {
                operation_id: Some(operation_id.clone()),
                output_id: Some(first.output_id.clone()),
                name_suffix: Some(first.name_suffix.clone()),
                output_count: expected.len(),
                source,
                role: first.bundle_role,
            }
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(VoomError::Config(
                "audio extract operation_id and outputs must be present together".to_owned(),
            ));
        }
    };
    Ok(ExtractAudioSelectionPlan {
        operation_id: resolved.operation_id,
        output_id: resolved.output_id,
        name_suffix: resolved.name_suffix,
        output_count: resolved.output_count,
        stream: stream_ref(&resolved.source),
        source: resolved.source,
        role: resolved.role,
        target_codec: payload.target_codec,
        container: payload.container,
    })
}

fn legacy_extract_selection(
    selected: &[SnapshotAudioStreamFact],
) -> Result<ResolvedExtractSelection, VoomError> {
    let [source] = selected else {
        return Err(VoomError::Config(
            "legacy audio extract payload matched multiple streams; regenerate the plan to carry \
             stable output descriptors"
                .to_owned(),
        ));
    };
    let role = extraction_role(source).map_err(audio_block_error)?;
    Ok(ResolvedExtractSelection {
        operation_id: None,
        output_id: None,
        name_suffix: None,
        output_count: 1,
        source: source.clone(),
        role,
    })
}

fn parse_payload(payload: &Value) -> Result<AudioOperationPayload, VoomError> {
    AudioOperationPayload::try_from_execution_value(payload)
        .map_err(|err| VoomError::Config(format!("audio operation payload is invalid: {err}")))
}

fn stream_ref(stream: &SnapshotAudioStreamFact) -> AudioStreamRef {
    AudioStreamRef {
        snapshot_stream_id: stream.snapshot_stream_id.clone(),
        provider_stream_index: stream.provider_stream_index,
    }
}

fn audio_block_error(block: AudioPlanningBlock) -> VoomError {
    match block {
        AudioPlanningBlock::InsufficientSnapshotFacts => {
            VoomError::Config("audio snapshot has insufficient stream facts".to_owned())
        }
        AudioPlanningBlock::UnsupportedSelector => {
            VoomError::Config("audio selector is unsupported".to_owned())
        }
        AudioPlanningBlock::ZeroMatches => {
            VoomError::Config("audio selector matched zero streams".to_owned())
        }
        AudioPlanningBlock::NoVideo => {
            VoomError::Config("audio selection requires at least one video stream".to_owned())
        }
        AudioPlanningBlock::UnsupportedMediaShape => {
            VoomError::Config("audio selector is unsupported for this media shape".to_owned())
        }
        AudioPlanningBlock::SynthesisNotDownmix => VoomError::Config(
            "synthesize audio target channel count must be fewer than the source (a downmix)"
                .to_owned(),
        ),
    }
}

#[cfg(test)]
#[path = "selection_test.rs"]
mod tests;
