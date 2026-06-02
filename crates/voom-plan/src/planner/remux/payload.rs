use serde::de::DeserializeOwned;
use serde_json::Value;
use voom_core::RemuxTrackGroup;
use voom_policy::{DefaultStrategy, TrackFilter, TrackTarget};

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
    parse_object_enum_field(object, parent, index, field)
}

fn parse_object_default_strategy(
    object: &serde_json::Map<String, Value>,
    parent: &str,
    index: usize,
    field: &str,
) -> Result<DefaultStrategy, RemuxPayloadError> {
    parse_object_enum_field(object, parent, index, field)
}

fn parse_object_enum_field<T>(
    object: &serde_json::Map<String, Value>,
    parent: &str,
    index: usize,
    field: &str,
) -> Result<T, RemuxPayloadError>
where
    T: DeserializeOwned,
{
    let value = object.get(field).ok_or_else(|| {
        RemuxPayloadError::new(format!("remux {parent}[{index}] missing `{field}`"))
    })?;
    serde_json::from_value(value.clone()).map_err(|err| {
        RemuxPayloadError::new(format!("remux {parent}[{index}] invalid `{field}`: {err}"))
    })
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

#[cfg(test)]
#[path = "payload_test.rs"]
mod tests;
