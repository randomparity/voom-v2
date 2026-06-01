use serde_json::Value;
use voom_policy::TrackFilter;

pub const AUDIO_TRANSCODE_CONTAINER: &str = "mkv";
pub const AUDIO_EXTRACT_CONTAINER: &str = "ogg";
pub const AUDIO_EXTRACT_CODEC: &str = "opus";

#[derive(Debug, Clone, PartialEq)]
pub struct AudioOperationPayload {
    pub operation_type: AudioOperationType,
    pub target_codec: String,
    pub container: String,
    pub source_media_snapshot_id: Option<u64>,
    pub filter: Option<TrackFilter>,
}

impl AudioOperationPayload {
    #[must_use]
    pub fn into_value(self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert(
            "type".to_owned(),
            Value::String(self.operation_type.as_str().to_owned()),
        );
        object.insert("target_codec".to_owned(), Value::String(self.target_codec));
        object.insert("container".to_owned(), Value::String(self.container));
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
        Value::Object(object)
    }

    pub fn try_from_execution_value(value: &Value) -> Result<Self, AudioPayloadError> {
        let object = value
            .as_object()
            .ok_or_else(|| AudioPayloadError::new("audio payload must be an object"))?;
        let operation_type = match object.get("type").and_then(Value::as_str) {
            Some("transcode_audio") => AudioOperationType::TranscodeAudio,
            Some("extract_audio") => AudioOperationType::ExtractAudio,
            Some(other) => {
                return Err(AudioPayloadError::new(format!(
                    "audio payload type `{other}` is unsupported"
                )));
            }
            None => return Err(AudioPayloadError::new("audio payload missing `type`")),
        };
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

        Ok(Self {
            operation_type,
            target_codec: target_codec.to_owned(),
            container: container.to_owned(),
            source_media_snapshot_id: Some(source_media_snapshot_id),
            filter,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOperationType {
    TranscodeAudio,
    ExtractAudio,
}

impl AudioOperationType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TranscodeAudio => "transcode_audio",
            Self::ExtractAudio => "extract_audio",
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
