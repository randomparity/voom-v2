use serde_json::Value;
use voom_core::VoomError;
use voom_plan::planner::audio::{
    AUDIO_EXTRACT_CODEC, AUDIO_EXTRACT_CONTAINER, AUDIO_TRANSCODE_CONTAINER, AudioBundleRole,
    AudioOperationPayload, AudioOperationType, AudioPlanningBlock, SnapshotAudioStreamFact,
    extract_audio_outputs, extraction_role, selected_audio_streams, synthesize_audio_companions,
    synthesize_audio_shape,
};
use voom_store::repo::media::identity::MediaSnapshot;
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
    pub operation_id: Option<String>,
    pub add_track: bool,
    pub target_channels: Option<u64>,
    pub compatibility: Option<voom_worker_protocol::AudioCompatibilitySettings>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractAudioSelectionPlan {
    pub operation_id: Option<String>,
    pub outputs: Vec<ExtractAudioSelectionOutput>,
    pub target_codec: String,
    pub container: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractAudioSelectionOutput {
    pub output_id: Option<String>,
    pub name_suffix: Option<String>,
    pub stream: AudioStreamRef,
    pub source: SnapshotAudioStreamFact,
    pub role: AudioBundleRole,
}

struct ResolvedExtractSelection {
    operation_id: Option<String>,
    outputs: Vec<ExtractAudioSelectionOutput>,
}

pub fn transcode_selection_from_payload_and_snapshot(
    payload: &Value,
    snapshot: &MediaSnapshot,
) -> Result<TranscodeAudioSelectionPlan, VoomError> {
    let payload = parse_payload(payload)?;
    if !matches!(
        payload.operation_type,
        AudioOperationType::TranscodeAudio
            | AudioOperationType::SynthesizeAudio
            | AudioOperationType::EnsureAudioCompatibility
    ) {
        return Err(VoomError::Config(
            "audio transcode payload type must be transcode_audio or synthesize_audio".to_owned(),
        ));
    }
    if payload.container != AUDIO_TRANSCODE_CONTAINER {
        return Err(VoomError::Config(format!(
            "audio transcode container {} is unsupported",
            payload.container
        )));
    }
    let snapshot_input = crate::media_snapshot::planning_input(1, snapshot);
    if payload.operation_type == AudioOperationType::EnsureAudioCompatibility {
        if payload.source_media_snapshot_id != Some(snapshot.id.0) {
            return Err(VoomError::Config(
                "compatibility payload pins a different snapshot".to_owned(),
            ));
        }
        return compatibility_selection(payload, &snapshot_input);
    }
    let selected = selected_audio_streams(&snapshot_input, payload.filter.as_ref())
        .map_err(audio_block_error)?;
    if selected.is_empty() {
        return Err(audio_block_error(AudioPlanningBlock::ZeroMatches));
    }
    if payload.operation_type == AudioOperationType::SynthesizeAudio {
        return synthesis_selection(payload, &snapshot_input, &selected);
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
        operation_id: None,
        add_track: false,
        target_channels: None,
        compatibility: None,
    })
}

fn synthesis_selection(
    payload: AudioOperationPayload,
    snapshot: &voom_policy::MediaSnapshotInput,
    selected: &[SnapshotAudioStreamFact],
) -> Result<TranscodeAudioSelectionPlan, VoomError> {
    let Some(operation_id) = payload.operation_id.as_deref() else {
        return Err(VoomError::Config(
            "synthesize_audio operation_id is required".to_owned(),
        ));
    };
    let Some(target_channels) = payload.target_channels else {
        return Err(VoomError::Config(
            "synthesize_audio target_channels is required".to_owned(),
        ));
    };
    if let voom_plan::planner::audio::AudioPlanShape::Blocked(block) =
        synthesize_audio_shape(snapshot, target_channels, payload.filter.as_ref())
    {
        return Err(audio_block_error(block));
    }
    let expected = synthesize_audio_companions(snapshot, payload.filter.as_ref(), operation_id)
        .map_err(audio_block_error)?;
    if payload.companions.as_ref() != Some(&expected) {
        return Err(VoomError::Config(
            "synthesize_audio companions do not match the pinned source snapshot".to_owned(),
        ));
    }
    let selected_streams = resolve_synthesis_streams(&expected, selected)?;
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
        operation_id: Some(operation_id.to_owned()),
        add_track: true,
        target_channels: Some(target_channels),
        compatibility: None,
    })
}

fn compatibility_selection(
    payload: AudioOperationPayload,
    snapshot: &voom_policy::MediaSnapshotInput,
) -> Result<TranscodeAudioSelectionPlan, VoomError> {
    use voom_plan::planner::audio::{
        AudioCompatibilityDescriptor, AudioCompatibilityError, resolve_audio_compatibility,
        synthesis_companion_id,
    };
    let resolution = resolve_audio_compatibility(snapshot).map_err(|error| match error {
        AudioCompatibilityError::InvalidFacts(reason)
        | AudioCompatibilityError::InvalidAssociation(reason) => {
            VoomError::database(format!("persisted compatibility audio facts: {reason}"))
        }
        other => VoomError::Config(format!("compatibility audio facts: {other:?}")),
    })?;
    if resolution.resolved_snapshot_stream_id.is_some() {
        return Err(VoomError::Config(
            "compatibility audio already exists; regenerate the plan".to_owned(),
        ));
    }
    let operation_id = payload
        .operation_id
        .as_deref()
        .ok_or_else(|| VoomError::Config("compatibility operation id missing".to_owned()))?;
    let result_id = synthesis_companion_id(operation_id, &resolution.source_snapshot_stream_id);
    let expected = AudioCompatibilityDescriptor {
        retained_streams: resolution.retained_streams,
        source_snapshot_stream_id: resolution.source_snapshot_stream_id,
        source_provider_stream_index: resolution.source_provider_stream_index,
        source_channels: resolution.source_channels,
        source_channel_layout: resolution.source_channel_layout,
        target_channel_layout: resolution.target.channel_layout,
        result_snapshot_stream_id: result_id.clone(),
    };
    if payload.compatibility.as_ref() != Some(&expected)
        || payload.target_codec != resolution.target.codec
        || payload.target_channels != Some(u64::from(resolution.target.channels))
    {
        return Err(VoomError::Config(
            "compatibility pins differ from the pinned snapshot resolution".to_owned(),
        ));
    }
    let source = voom_plan::planner::audio::stream_facts(snapshot)
        .map_err(audio_block_error)?
        .into_iter()
        .find(|source| {
            source.snapshot_stream_id == expected.source_snapshot_stream_id
                && source.provider_stream_index == expected.source_provider_stream_index
        })
        .ok_or_else(|| VoomError::Config("compatibility source disappeared".to_owned()))?;
    let stream = AudioStreamRef {
        snapshot_stream_id: result_id,
        provider_stream_index: expected.source_provider_stream_index,
    };
    let settings = voom_worker_protocol::AudioCompatibilitySettings {
        retained_streams: expected
            .retained_streams
            .into_iter()
            .map(|stream| voom_worker_protocol::CompatibilityStreamIdentity {
                snapshot_stream_id: stream.snapshot_stream_id,
                provider_stream_index: stream.provider_stream_index,
            })
            .collect(),
        source_snapshot_stream_id: expected.source_snapshot_stream_id,
        source_channels: expected.source_channels,
        source_channel_layout: expected.source_channel_layout,
        target_channel_layout: expected.target_channel_layout,
    };
    Ok(TranscodeAudioSelectionPlan {
        selection: TranscodeAudioSelection {
            selected_streams: vec![stream.clone()],
        },
        selected_streams: vec![SelectedAudioStream { stream, source }],
        target_codec: payload.target_codec,
        container: payload.container,
        operation_id: payload.operation_id,
        add_track: true,
        target_channels: payload.target_channels,
        compatibility: Some(settings),
    })
}

fn resolve_synthesis_streams(
    descriptors: &[voom_plan::planner::audio::SynthesizeAudioCompanionDescriptor],
    selected: &[SnapshotAudioStreamFact],
) -> Result<Vec<SelectedAudioStream>, VoomError> {
    descriptors
        .iter()
        .map(|descriptor| {
            let source = selected
                .iter()
                .find(|source| {
                    source.snapshot_stream_id == descriptor.source_snapshot_stream_id
                        && source.provider_stream_index == descriptor.source_provider_stream_index
                })
                .ok_or_else(|| {
                    VoomError::Config(format!(
                        "synthesize_audio companion {} has no selected source",
                        descriptor.companion_id
                    ))
                })?
                .clone();
            Ok(SelectedAudioStream {
                stream: AudioStreamRef {
                    snapshot_stream_id: descriptor.result_snapshot_stream_id.clone(),
                    provider_stream_index: descriptor.source_provider_stream_index,
                },
                source,
            })
        })
        .collect()
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
            if expected.is_empty() {
                return Err(VoomError::Config(
                    "audio extract outputs must not be empty".to_owned(),
                ));
            }
            let resolved_outputs = expected
                .iter()
                .map(|descriptor| {
                    let source = selected
                        .iter()
                        .find(|source| {
                            source.snapshot_stream_id == descriptor.source_snapshot_stream_id
                                && source.provider_stream_index
                                    == descriptor.source_provider_stream_index
                        })
                        .ok_or_else(|| {
                            VoomError::Config(format!(
                                "audio extract output {} does not identify a selected source stream",
                                descriptor.output_id
                            ))
                        })?
                        .clone();
                    Ok(ExtractAudioSelectionOutput {
                        output_id: Some(descriptor.output_id.clone()),
                        name_suffix: Some(descriptor.name_suffix.clone()),
                        stream: stream_ref(&source),
                        source,
                        role: descriptor.bundle_role,
                    })
                })
                .collect::<Result<Vec<_>, VoomError>>()?;
            ResolvedExtractSelection {
                operation_id: Some(operation_id.clone()),
                outputs: resolved_outputs,
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
        outputs: resolved.outputs,
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
        outputs: vec![ExtractAudioSelectionOutput {
            output_id: None,
            name_suffix: None,
            stream: stream_ref(source),
            source: source.clone(),
            role,
        }],
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
