use std::collections::HashSet;

use serde_json::Value;
use voom_policy::{ComparisonOp, DefaultStrategy, TrackFilter, TrackTarget};
use voom_worker_protocol::RemuxTrackGroup;

const REMUX_PAYLOAD_TYPE: &str = "remux";
const REMUX_CONTAINER: &str = "mkv";

#[derive(Debug, Clone, PartialEq)]
pub struct RemuxOperationPayload {
    pub container: String,
    pub source_media_snapshot_id: Option<u64>,
    pub track_actions: Vec<RemuxTrackAction>,
    pub track_order: Vec<RemuxTrackGroup>,
    pub defaults: Vec<RemuxDefaultAction>,
}

impl RemuxOperationPayload {
    pub fn try_from_value(value: &Value) -> Result<Self, RemuxPayloadError> {
        let object = value
            .as_object()
            .ok_or_else(|| RemuxPayloadError::new("remux payload must be an object"))?;
        if object.get("type").and_then(Value::as_str) != Some(REMUX_PAYLOAD_TYPE) {
            return Err(RemuxPayloadError::new(
                "remux payload missing `type: remux`",
            ));
        }

        let container = object
            .get("container")
            .and_then(Value::as_str)
            .ok_or_else(|| RemuxPayloadError::new("remux payload missing `container`"))?;
        if container != REMUX_CONTAINER {
            return Err(RemuxPayloadError::new(
                "remux payload `container` must be mkv",
            ));
        }

        let source_media_snapshot_id = match object.get("source_media_snapshot_id") {
            Some(value) => {
                let id = value.as_u64().filter(|id| *id > 0).ok_or_else(|| {
                    RemuxPayloadError::new(
                        "remux payload `source_media_snapshot_id` must be a positive integer",
                    )
                })?;
                Some(id)
            }
            None => None,
        };

        let track_actions = match object.get("track_actions") {
            Some(value) => {
                let actions = value.as_array().ok_or_else(|| {
                    RemuxPayloadError::new("remux payload `track_actions` must be an array")
                })?;
                parse_track_actions(actions)?
            }
            None => Vec::new(),
        };
        let track_order = match object.get("track_order") {
            Some(value) => {
                let order = value.as_array().ok_or_else(|| {
                    RemuxPayloadError::new("remux payload `track_order` must be an array")
                })?;
                parse_track_order(order)?
            }
            None => default_track_order(),
        };
        let defaults = match object.get("defaults") {
            Some(value) => {
                let defaults = value.as_array().ok_or_else(|| {
                    RemuxPayloadError::new("remux payload `defaults` must be an array")
                })?;
                parse_defaults(defaults)?
            }
            None => Vec::new(),
        };

        Ok(Self {
            container: container.to_owned(),
            source_media_snapshot_id,
            track_actions,
            track_order,
            defaults,
        })
    }

    pub fn try_from_execution_value(value: &Value) -> Result<Self, RemuxPayloadError> {
        let payload = Self::try_from_value(value)?;
        if payload.source_media_snapshot_id.is_none() {
            return Err(RemuxPayloadError::new(
                "remux payload `source_media_snapshot_id` must be a positive integer",
            ));
        }
        Ok(payload)
    }

    #[must_use]
    pub fn into_value(self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert(
            "type".to_owned(),
            Value::String(REMUX_PAYLOAD_TYPE.to_owned()),
        );
        object.insert("container".to_owned(), Value::String(self.container));
        object.insert(
            "track_actions".to_owned(),
            Value::Array(
                self.track_actions
                    .into_iter()
                    .map(remux_track_action_value)
                    .collect(),
            ),
        );
        object.insert(
            "track_order".to_owned(),
            Value::Array(
                self.track_order
                    .into_iter()
                    .map(|group| Value::String(track_group_name(group).to_owned()))
                    .collect(),
            ),
        );
        object.insert(
            "defaults".to_owned(),
            Value::Array(
                self.defaults
                    .iter()
                    .map(remux_default_action_value)
                    .collect(),
            ),
        );
        if let Some(id) = self.source_media_snapshot_id {
            object.insert("source_media_snapshot_id".to_owned(), Value::from(id));
        }
        Value::Object(object)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemuxTrackAction {
    pub kind: RemuxTrackActionKind,
    pub target: TrackTarget,
    pub filter: Option<TrackFilter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemuxTrackActionKind {
    KeepTracks,
    RemoveTracks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemuxDefaultAction {
    pub target: TrackTarget,
    pub strategy: DefaultStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemuxPayloadError {
    detail: String,
}

impl RemuxPayloadError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for RemuxPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for RemuxPayloadError {}

fn parse_track_actions(actions: &[Value]) -> Result<Vec<RemuxTrackAction>, RemuxPayloadError> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| parse_track_action(index, action))
        .collect()
}

fn parse_track_action(index: usize, action: &Value) -> Result<RemuxTrackAction, RemuxPayloadError> {
    let object = action.as_object().ok_or_else(|| {
        RemuxPayloadError::new(format!("remux track_actions[{index}] must be an object"))
    })?;
    let raw_kind = required_object_string(object, "track_actions", index, "type")?;
    let kind = match raw_kind {
        "keep_tracks" => RemuxTrackActionKind::KeepTracks,
        "remove_tracks" => RemuxTrackActionKind::RemoveTracks,
        other => {
            return Err(RemuxPayloadError::new(format!(
                "remux track_actions[{index}] type `{other}` is unsupported"
            )));
        }
    };
    let target = parse_object_track_target(object, "track_actions", index, "target")?;
    if target == TrackTarget::Attachment {
        return Err(RemuxPayloadError::new(format!(
            "remux track_actions[{index}] target `attachment` is unsupported"
        )));
    }
    let filter = match object.get("filter") {
        Some(Value::Null) | None => None,
        Some(filter) => Some(serde_json::from_value(filter.clone()).map_err(|err| {
            RemuxPayloadError::new(format!(
                "remux track_actions[{index}] `filter` is invalid: {err}"
            ))
        })?),
    };

    Ok(RemuxTrackAction {
        kind,
        target,
        filter,
    })
}

fn parse_track_order(order: &[Value]) -> Result<Vec<RemuxTrackGroup>, RemuxPayloadError> {
    if order.is_empty() {
        return Err(RemuxPayloadError::new(
            "remux track_order must include at least one group",
        ));
    }
    let mut seen = Vec::with_capacity(order.len());
    let mut groups = Vec::with_capacity(order.len());
    for (index, value) in order.iter().enumerate() {
        let Some(group) = value.as_str() else {
            return Err(RemuxPayloadError::new(format!(
                "remux track_order[{index}] must be a string"
            )));
        };
        let parsed = match group {
            "video" => RemuxTrackGroup::Video,
            "audio" => RemuxTrackGroup::Audio,
            "subtitle" => RemuxTrackGroup::Subtitle,
            "attachment" => {
                return Err(RemuxPayloadError::new(format!(
                    "remux track_order[{index}] target `attachment` is unsupported"
                )));
            }
            other => {
                return Err(RemuxPayloadError::new(format!(
                    "remux track_order[{index}] has unsupported target `{other}`"
                )));
            }
        };
        if seen.contains(&parsed) {
            return Err(RemuxPayloadError::new(format!(
                "remux track_order[{index}] duplicates target `{}`",
                track_group_name(parsed)
            )));
        }
        seen.push(parsed);
        groups.push(parsed);
    }
    Ok(groups)
}

fn parse_defaults(defaults: &[Value]) -> Result<Vec<RemuxDefaultAction>, RemuxPayloadError> {
    defaults
        .iter()
        .enumerate()
        .map(|(index, default)| {
            let object = default.as_object().ok_or_else(|| {
                RemuxPayloadError::new(format!("remux defaults[{index}] must be an object"))
            })?;
            Ok(RemuxDefaultAction {
                target: parse_object_track_target(object, "defaults", index, "target")?,
                strategy: parse_object_default_strategy(object, "defaults", index, "strategy")?,
            })
        })
        .collect()
}

fn required_object_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    parent: &str,
    index: usize,
    field: &str,
) -> Result<&'a str, RemuxPayloadError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| RemuxPayloadError::new(format!("remux {parent}[{index}] missing `{field}`")))
}

fn parse_object_track_target(
    object: &serde_json::Map<String, Value>,
    parent: &str,
    index: usize,
    field: &str,
) -> Result<TrackTarget, RemuxPayloadError> {
    let value = object.get(field).ok_or_else(|| {
        RemuxPayloadError::new(format!("remux {parent}[{index}] missing `{field}`"))
    })?;
    serde_json::from_value(value.clone())
        .map_err(|_| RemuxPayloadError::new(format!("remux {parent}[{index}] missing `{field}`")))
}

fn parse_object_default_strategy(
    object: &serde_json::Map<String, Value>,
    parent: &str,
    index: usize,
    field: &str,
) -> Result<DefaultStrategy, RemuxPayloadError> {
    let value = object.get(field).ok_or_else(|| {
        RemuxPayloadError::new(format!("remux {parent}[{index}] missing `{field}`"))
    })?;
    serde_json::from_value(value.clone())
        .map_err(|_| RemuxPayloadError::new(format!("remux {parent}[{index}] missing `{field}`")))
}

pub(crate) fn default_track_order() -> Vec<RemuxTrackGroup> {
    vec![
        RemuxTrackGroup::Video,
        RemuxTrackGroup::Audio,
        RemuxTrackGroup::Subtitle,
    ]
}

fn track_group_name(group: RemuxTrackGroup) -> &'static str {
    match group {
        RemuxTrackGroup::Video => "video",
        RemuxTrackGroup::Audio => "audio",
        RemuxTrackGroup::Subtitle => "subtitle",
        RemuxTrackGroup::Attachment => "attachment",
    }
}

fn remux_track_action_value(action: RemuxTrackAction) -> Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "type".to_owned(),
        Value::String(track_action_kind_name(action.kind).to_owned()),
    );
    object.insert(
        "target".to_owned(),
        Value::String(track_target_name(action.target).to_owned()),
    );
    if let Some(filter) = action.filter {
        object.insert(
            "filter".to_owned(),
            serde_json::to_value(filter).unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
        );
    }
    Value::Object(object)
}

fn remux_default_action_value(action: &RemuxDefaultAction) -> Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "target".to_owned(),
        Value::String(track_target_name(action.target).to_owned()),
    );
    object.insert(
        "strategy".to_owned(),
        Value::String(default_strategy_name(action.strategy).to_owned()),
    );
    Value::Object(object)
}

fn track_action_kind_name(kind: RemuxTrackActionKind) -> &'static str {
    match kind {
        RemuxTrackActionKind::KeepTracks => "keep_tracks",
        RemuxTrackActionKind::RemoveTracks => "remove_tracks",
    }
}

fn track_target_name(target: TrackTarget) -> &'static str {
    match target {
        TrackTarget::Video => "video",
        TrackTarget::Audio => "audio",
        TrackTarget::Subtitle => "subtitle",
        TrackTarget::Attachment => "attachment",
    }
}

fn default_strategy_name(strategy: DefaultStrategy) -> &'static str {
    match strategy {
        DefaultStrategy::First => "first",
        DefaultStrategy::Best => "best",
        DefaultStrategy::None => "none",
        DefaultStrategy::Preserve => "preserve",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotStreamFact {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
    pub kind: TrackTarget,
    pub codec_name: Option<String>,
    pub language: Option<String>,
    pub channels: Option<u32>,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub filename: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemuxPlanningBlock {
    InsufficientSnapshotFacts,
    UnsupportedMediaShape,
}

pub fn stream_facts(
    snapshot: &voom_policy::MediaSnapshotInput,
) -> Result<Vec<SnapshotStreamFact>, RemuxPlanningBlock> {
    let streams = snapshot
        .stream_summary
        .get("streams")
        .and_then(Value::as_array)
        .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)?;
    let mut ids = HashSet::with_capacity(streams.len());
    let mut facts = Vec::with_capacity(streams.len());

    for stream in streams {
        let stream = stream
            .as_object()
            .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)?;
        let snapshot_stream_id = required_string(stream.get("id"))?;
        if !ids.insert(snapshot_stream_id.clone()) {
            return Err(RemuxPlanningBlock::InsufficientSnapshotFacts);
        }
        let provider_stream_index = stream
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)?;
        let kind = match required_string(stream.get("kind"))?.as_str() {
            "video" => TrackTarget::Video,
            "audio" => TrackTarget::Audio,
            "subtitle" => TrackTarget::Subtitle,
            "attachment" => TrackTarget::Attachment,
            _ => return Err(RemuxPlanningBlock::InsufficientSnapshotFacts),
        };

        facts.push(SnapshotStreamFact {
            snapshot_stream_id,
            provider_stream_index,
            kind,
            codec_name: optional_string(stream.get("codec_name")),
            language: optional_string(stream.get("language")),
            channels: stream
                .get("channels")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
            title: optional_string(stream.get("title")),
            mime_type: optional_string(stream.get("mime_type")),
            filename: optional_string(stream.get("filename")),
            is_default: disposition_flag(stream.get("disposition"), "default"),
            is_forced: disposition_flag(stream.get("disposition"), "forced"),
        });
    }

    Ok(facts)
}

pub fn evaluate_filter(
    filter: &TrackFilter,
    stream: &SnapshotStreamFact,
) -> Result<bool, RemuxPlanningBlock> {
    match filter {
        TrackFilter::LanguageIn { values } => {
            let language = stream
                .language
                .as_ref()
                .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(values.iter().any(|value| value == language))
        }
        TrackFilter::CodecIn { values } => {
            let codec_name = stream
                .codec_name
                .as_ref()
                .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(values.iter().any(|value| value == codec_name))
        }
        TrackFilter::Channels { op, value } => {
            let channels = stream
                .channels
                .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(compare_u64(u64::from(channels), *op, *value))
        }
        TrackFilter::Commentary | TrackFilter::TitleMatches { .. } => {
            Err(RemuxPlanningBlock::UnsupportedMediaShape)
        }
        TrackFilter::Forced => Ok(stream.is_forced),
        TrackFilter::Default => Ok(stream.is_default),
        TrackFilter::Font => Ok(stream.kind == TrackTarget::Attachment
            && stream
                .mime_type
                .as_deref()
                .is_some_and(|mime_type| mime_type.contains("font"))),
        TrackFilter::TitleContains { value } => {
            let title = stream
                .title
                .as_ref()
                .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(title.contains(value))
        }
        TrackFilter::Not { inner } => Ok(!evaluate_filter(inner, stream)?),
        TrackFilter::And { filters } => {
            let mut matched = true;
            for filter in filters {
                matched = evaluate_filter(filter, stream)? && matched;
            }
            Ok(matched)
        }
        TrackFilter::Or { filters } => {
            let mut insufficient = false;
            for filter in filters {
                match evaluate_filter(filter, stream) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(RemuxPlanningBlock::InsufficientSnapshotFacts) => insufficient = true,
                    Err(RemuxPlanningBlock::UnsupportedMediaShape) => {
                        return Err(RemuxPlanningBlock::UnsupportedMediaShape);
                    }
                }
            }
            if insufficient {
                Err(RemuxPlanningBlock::InsufficientSnapshotFacts)
            } else {
                Ok(false)
            }
        }
    }
}

fn required_string(value: Option<&Value>) -> Result<String, RemuxPlanningBlock> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn disposition_flag(disposition: Option<&Value>, key: &str) -> bool {
    disposition
        .and_then(Value::as_object)
        .and_then(|object| object.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn compare_u64(left: u64, op: ComparisonOp, right: u64) -> bool {
    match op {
        ComparisonOp::Eq => left == right,
        ComparisonOp::Ne => left != right,
        ComparisonOp::Lt => left < right,
        ComparisonOp::Lte => left <= right,
        ComparisonOp::Gt => left > right,
        ComparisonOp::Gte => left >= right,
        ComparisonOp::Contains | ComparisonOp::Matches => false,
    }
}

#[cfg(test)]
#[path = "remux_test.rs"]
mod tests;
