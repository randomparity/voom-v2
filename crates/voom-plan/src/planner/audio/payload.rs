use serde_json::Value;
use voom_policy::TrackFilter;

use super::selection::AudioBundleRole;

pub const AUDIO_TRANSCODE_CONTAINER: &str = "mkv";
pub const AUDIO_EXTRACT_CONTAINER: &str = "ogg";
pub const AUDIO_EXTRACT_CODEC: &str = "opus";
const EXTRACT_OUTPUT_ID_DOMAIN: &str = "voom.extract_audio.output.v1";
const SYNTHESIS_COMPANION_ID_DOMAIN: &str = "voom.synthesize_audio.companion.v1";
const STABLE_ID_HEX_LENGTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractAudioOutputDescriptor {
    pub output_id: String,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub name_suffix: String,
    pub bundle_role: AudioBundleRole,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynthesizeAudioCompanionDescriptor {
    pub companion_id: String,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub result_snapshot_stream_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioOperationPayload {
    pub operation_type: AudioOperationType,
    pub operation_id: Option<String>,
    pub target_codec: String,
    pub container: String,
    pub source_media_snapshot_id: Option<u64>,
    pub filter: Option<TrackFilter>,
    pub outputs: Option<Vec<ExtractAudioOutputDescriptor>>,
    pub companions: Option<Vec<SynthesizeAudioCompanionDescriptor>>,
    /// Target channel count for a `synthesize_audio` downmix (ADR 0026, #276).
    /// `None` for transcode/extract, which preserve the source channel count.
    pub target_channels: Option<u64>,
}

impl AudioOperationPayload {
    #[must_use]
    pub fn into_value(self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert(
            "type".to_owned(),
            Value::String(self.operation_type.as_str().to_owned()),
        );
        if let Some(operation_id) = self.operation_id {
            object.insert("operation_id".to_owned(), Value::String(operation_id));
        }
        object.insert("target_codec".to_owned(), Value::String(self.target_codec));
        object.insert("container".to_owned(), Value::String(self.container));
        if let Some(target_channels) = self.target_channels {
            object.insert("target_channels".to_owned(), Value::from(target_channels));
        }
        if let Some(source_media_snapshot_id) = self.source_media_snapshot_id {
            object.insert(
                "source_media_snapshot_id".to_owned(),
                Value::from(source_media_snapshot_id),
            );
        }
        if let Some(filter) = self.filter {
            object.insert(
                "filter".to_owned(),
                serde_json::to_value(filter).unwrap_or(Value::Null),
            );
        }
        if let Some(outputs) = self.outputs {
            object.insert(
                "outputs".to_owned(),
                serde_json::to_value(outputs).unwrap_or(Value::Null),
            );
        }
        if let Some(companions) = self.companions {
            object.insert(
                "companions".to_owned(),
                serde_json::to_value(companions).unwrap_or(Value::Null),
            );
        }
        Value::Object(object)
    }

    pub fn try_from_execution_value(value: &Value) -> Result<Self, AudioPayloadError> {
        let object = value
            .as_object()
            .ok_or_else(|| AudioPayloadError::new("audio payload must be an object"))?;
        let operation_type = match object.get("type").and_then(Value::as_str) {
            Some("transcode_audio") => AudioOperationType::TranscodeAudio,
            Some("extract_audio") => AudioOperationType::ExtractAudio,
            Some("synthesize_audio") => AudioOperationType::SynthesizeAudio,
            Some(other) => {
                return Err(AudioPayloadError::new(format!(
                    "audio payload type `{other}` is unsupported"
                )));
            }
            None => return Err(AudioPayloadError::new("audio payload missing `type`")),
        };
        let operation_id = optional_non_empty_string(object.get("operation_id"), "operation_id")?;
        let target_codec = object
            .get("target_codec")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| AudioPayloadError::new("audio payload missing `target_codec`"))?;
        let container = object
            .get("container")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| AudioPayloadError::new("audio payload missing `container`"))?;
        let source_media_snapshot_id = object
            .get("source_media_snapshot_id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or_else(|| {
                AudioPayloadError::new(
                    "audio payload `source_media_snapshot_id` must be a positive integer",
                )
            })?;
        let filter = match object.get("filter") {
            Some(Value::Null) | None => None,
            Some(filter) => Some(serde_json::from_value(filter.clone()).map_err(|err| {
                AudioPayloadError::new(format!("audio payload `filter` is invalid: {err}"))
            })?),
        };
        let target_channels = match object.get("target_channels") {
            Some(Value::Null) | None => None,
            Some(value) => Some(value.as_u64().filter(|count| *count > 0).ok_or_else(|| {
                AudioPayloadError::new("audio payload `target_channels` must be a positive integer")
            })?),
        };
        let outputs = match object.get("outputs") {
            None => None,
            Some(Value::Array(_)) => Some(
                serde_json::from_value(object["outputs"].clone()).map_err(|err| {
                    AudioPayloadError::new(format!("audio payload `outputs` is invalid: {err}"))
                })?,
            ),
            Some(_) => {
                return Err(AudioPayloadError::new(
                    "audio payload `outputs` must be an array",
                ));
            }
        };
        let companions = parse_companions(object.get("companions"))?;
        if operation_type == AudioOperationType::SynthesizeAudio && target_channels.is_none() {
            return Err(AudioPayloadError::new(
                "synthesize_audio payload requires `target_channels`",
            ));
        }
        validate_synthesis_fields(
            operation_type,
            operation_id.as_deref(),
            companions.as_deref(),
        )?;

        Ok(Self {
            operation_type,
            operation_id,
            target_codec: target_codec.to_owned(),
            container: container.to_owned(),
            source_media_snapshot_id: Some(source_media_snapshot_id),
            filter,
            outputs,
            companions,
            target_channels,
        })
    }
}

#[must_use]
pub fn extract_output_id(operation_id: &str, snapshot_stream_id: &str) -> String {
    let preimage = format!("{EXTRACT_OUTPUT_ID_DOMAIN}\0{operation_id}\0{snapshot_stream_id}");
    let hash = blake3::hash(preimage.as_bytes()).to_hex().to_string();
    format!("extract_output_{}", &hash[..STABLE_ID_HEX_LENGTH])
}

#[must_use]
pub fn synthesis_companion_id(operation_id: &str, snapshot_stream_id: &str) -> String {
    let preimage = format!("{SYNTHESIS_COMPANION_ID_DOMAIN}\0{operation_id}\0{snapshot_stream_id}");
    let hash = blake3::hash(preimage.as_bytes()).to_hex().to_string();
    format!("synth_companion_{}", &hash[..STABLE_ID_HEX_LENGTH])
}

fn parse_companions(
    value: Option<&Value>,
) -> Result<Option<Vec<SynthesizeAudioCompanionDescriptor>>, AudioPayloadError> {
    match value {
        None => Ok(None),
        Some(Value::Array(_)) => serde_json::from_value(value.cloned().unwrap_or(Value::Null))
            .map(Some)
            .map_err(|err| {
                AudioPayloadError::new(format!("audio payload `companions` is invalid: {err}"))
            }),
        Some(_) => Err(AudioPayloadError::new(
            "audio payload `companions` must be an array",
        )),
    }
}

fn validate_synthesis_fields(
    operation_type: AudioOperationType,
    operation_id: Option<&str>,
    companions: Option<&[SynthesizeAudioCompanionDescriptor]>,
) -> Result<(), AudioPayloadError> {
    if operation_type != AudioOperationType::SynthesizeAudio {
        if companions.is_some() {
            return Err(AudioPayloadError::new(
                "audio payload `companions` is valid only for synthesize_audio",
            ));
        }
        return Ok(());
    }
    let (Some(operation_id), Some(companions)) = (operation_id, companions) else {
        return Err(AudioPayloadError::new(
            "synthesize_audio payload requires `operation_id` and `companions`",
        ));
    };
    if companions.is_empty() {
        return Err(AudioPayloadError::new(
            "synthesize_audio payload `companions` must not be empty",
        ));
    }
    validate_companions(operation_id, companions)
}

fn validate_companions(
    operation_id: &str,
    companions: &[SynthesizeAudioCompanionDescriptor],
) -> Result<(), AudioPayloadError> {
    let mut source_ids = std::collections::HashSet::with_capacity(companions.len());
    let mut provider_indexes = std::collections::HashSet::with_capacity(companions.len());
    let mut result_ids = std::collections::HashSet::with_capacity(companions.len());
    let mut previous_index = None;
    for companion in companions {
        let expected_id =
            synthesis_companion_id(operation_id, &companion.source_snapshot_stream_id);
        let valid = !companion.source_snapshot_stream_id.trim().is_empty()
            && companion.companion_id == expected_id
            && companion.result_snapshot_stream_id == companion.companion_id
            && source_ids.insert(&companion.source_snapshot_stream_id)
            && provider_indexes.insert(companion.source_provider_stream_index)
            && result_ids.insert(&companion.result_snapshot_stream_id)
            && previous_index.is_none_or(|index| index < companion.source_provider_stream_index);
        if !valid {
            return Err(AudioPayloadError::new(
                "synthesize_audio payload contains invalid or unordered companion descriptors",
            ));
        }
        previous_index = Some(companion.source_provider_stream_index);
    }
    Ok(())
}

fn optional_non_empty_string(
    value: Option<&Value>,
    field: &str,
) -> Result<Option<String>, AudioPayloadError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(AudioPayloadError::new(format!(
            "audio payload `{field}` must be a non-empty string"
        ))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOperationType {
    TranscodeAudio,
    ExtractAudio,
    /// Add a downmixed companion track derived from the source (ADR 0026, #276).
    SynthesizeAudio,
}

impl AudioOperationType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TranscodeAudio => "transcode_audio",
            Self::ExtractAudio => "extract_audio",
            Self::SynthesizeAudio => "synthesize_audio",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPayloadError {
    detail: String,
}

impl AudioPayloadError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for AudioPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for AudioPayloadError {}

#[cfg(test)]
#[path = "payload_test.rs"]
mod tests;
