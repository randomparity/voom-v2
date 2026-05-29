use serde_json::Value;
use voom_core::VoomError;
use voom_plan::audio::{
    AUDIO_EXTRACT_CODEC, AUDIO_EXTRACT_CONTAINER, AUDIO_TRANSCODE_CONTAINER, AudioBundleRole,
    AudioOperationPayload, AudioOperationType, AudioPlanningBlock, SnapshotAudioStreamFact,
    extraction_role, has_transcode_preservation_facts, selected_audio_streams,
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
    pub stream: AudioStreamRef,
    pub source: SnapshotAudioStreamFact,
    pub role: AudioBundleRole,
    pub target_codec: String,
    pub container: String,
}

pub fn transcode_selection_from_payload_and_snapshot(
    payload: &Value,
    snapshot: &MediaSnapshot,
) -> Result<TranscodeAudioSelectionPlan, VoomError> {
    let payload = parse_payload(payload)?;
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
    let snapshot_input = crate::media_snapshot::planning_input(snapshot);
    let selected = selected_audio_streams(&snapshot_input, payload.filter.as_ref())
        .map_err(audio_block_error)?;
    if selected.is_empty() {
        return Err(audio_block_error(AudioPlanningBlock::ZeroMatches));
    }
    if !selected.iter().all(has_transcode_preservation_facts) {
        return Err(audio_block_error(
            AudioPlanningBlock::InsufficientSnapshotFacts,
        ));
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
    let snapshot_input = crate::media_snapshot::planning_input(snapshot);
    let selected = selected_audio_streams(&snapshot_input, payload.filter.as_ref())
        .map_err(audio_block_error)?;
    let [source] = selected.as_slice() else {
        return Err(audio_block_error(if selected.is_empty() {
            AudioPlanningBlock::ZeroMatches
        } else {
            AudioPlanningBlock::MultipleMatches
        }));
    };
    let role = extraction_role(source).map_err(audio_block_error)?;
    Ok(ExtractAudioSelectionPlan {
        stream: stream_ref(source),
        source: source.clone(),
        role,
        target_codec: payload.target_codec,
        container: payload.container,
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
        AudioPlanningBlock::MultipleMatches => {
            VoomError::Config("audio selector matched multiple streams".to_owned())
        }
        AudioPlanningBlock::NoVideo => {
            VoomError::Config("audio selection requires at least one video stream".to_owned())
        }
        AudioPlanningBlock::UnsupportedMediaShape => {
            VoomError::Config("audio selector is unsupported for this media shape".to_owned())
        }
    }
}

#[cfg(test)]
#[path = "selection_test.rs"]
mod tests;
