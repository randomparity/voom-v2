use std::collections::{BTreeSet, HashSet};

use serde::Deserialize;
use serde_json::{Value, json};
use voom_core::VoomError;
use voom_plan::remux::{RemuxPlanningBlock, SnapshotStreamFact, evaluate_filter, stream_facts};
use voom_policy::{DefaultStrategy, MediaSnapshotInput, TargetRef, TrackTarget};
use voom_store::repo::identity::MediaSnapshot;
use voom_worker_protocol::{RemuxSelection, RemuxStreamRef, RemuxTrackGroup};

pub fn selection_from_payload_and_snapshot(
    payload: &Value,
    snapshot: &MediaSnapshot,
) -> Result<RemuxSelection, VoomError> {
    let payload = RemuxPayload::parse(payload)?;
    if !voom_worker_protocol::is_supported_remux_container(&payload.container) {
        return Err(VoomError::Config(format!(
            "remux container {} is unsupported",
            payload.container
        )));
    }
    let snapshot_input = media_snapshot_input(snapshot);
    let facts = stream_facts(&snapshot_input).map_err(remux_block_error)?;
    if !facts.iter().any(|stream| stream.kind == TrackTarget::Video) {
        return Err(VoomError::Config(
            "remux selection requires at least one video stream".to_owned(),
        ));
    }

    let mut keep_ids = facts
        .iter()
        .map(|stream| stream.snapshot_stream_id.clone())
        .collect::<BTreeSet<_>>();
    for action in &payload.track_actions {
        if action.target == TrackTarget::Video {
            return Err(VoomError::Config(
                "video track policy is unsupported".to_owned(),
            ));
        }
        let matching_ids = matching_stream_ids(&facts, action.target, action.filter.as_ref())?;
        match action.kind {
            TrackActionKind::KeepTracks => {
                remove_target(&facts, action.target, &mut keep_ids);
                keep_ids.extend(matching_ids);
            }
            TrackActionKind::RemoveTracks => {
                for id in matching_ids {
                    keep_ids.remove(&id);
                }
            }
        }
    }

    for stream in facts
        .iter()
        .filter(|stream| stream.kind == TrackTarget::Video)
    {
        keep_ids.insert(stream.snapshot_stream_id.clone());
    }

    let keep_streams = facts
        .iter()
        .filter(|stream| keep_ids.contains(&stream.snapshot_stream_id))
        .map(stream_ref)
        .collect::<Vec<_>>();
    let (default_streams, clear_default_streams) =
        default_refs(&payload.defaults, &facts, &keep_ids)?;

    Ok(RemuxSelection {
        keep_streams,
        default_streams,
        clear_default_streams,
        track_order: payload.track_order,
    })
}

fn matching_stream_ids(
    facts: &[SnapshotStreamFact],
    target: TrackTarget,
    filter: Option<&voom_policy::TrackFilter>,
) -> Result<Vec<String>, VoomError> {
    let mut ids = Vec::new();
    for stream in facts.iter().filter(|stream| stream.kind == target) {
        let matched = match filter {
            Some(filter) => evaluate_filter(filter, stream).map_err(remux_block_error)?,
            None => true,
        };
        if matched {
            ids.push(stream.snapshot_stream_id.clone());
        }
    }
    Ok(ids)
}

fn remove_target(
    facts: &[SnapshotStreamFact],
    target: TrackTarget,
    keep_ids: &mut BTreeSet<String>,
) {
    for stream in facts.iter().filter(|stream| stream.kind == target) {
        keep_ids.remove(&stream.snapshot_stream_id);
    }
}

fn default_refs(
    defaults: &[DefaultAction],
    facts: &[SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
) -> Result<(Vec<RemuxStreamRef>, Vec<RemuxStreamRef>), VoomError> {
    let mut default_streams = Vec::new();
    let mut clear_default_streams = Vec::new();
    for action in defaults {
        if matches!(action.strategy, DefaultStrategy::Best) {
            return Err(VoomError::Config(
                "default strategy best is unsupported".to_owned(),
            ));
        }
        let kept_target = facts
            .iter()
            .filter(|stream| {
                stream.kind == action.target && keep_ids.contains(&stream.snapshot_stream_id)
            })
            .collect::<Vec<_>>();
        match action.strategy {
            DefaultStrategy::First => {
                let Some(first) = kept_target
                    .iter()
                    .min_by_key(|stream| stream.provider_stream_index)
                else {
                    continue;
                };
                default_streams.push(stream_ref(first));
                clear_default_streams.extend(
                    kept_target
                        .iter()
                        .filter(|stream| stream.snapshot_stream_id != first.snapshot_stream_id)
                        .map(|stream| stream_ref(stream)),
                );
            }
            DefaultStrategy::None => {
                clear_default_streams.extend(kept_target.into_iter().map(stream_ref));
            }
            DefaultStrategy::Preserve | DefaultStrategy::Best => {}
        }
    }
    Ok((
        dedupe_refs(default_streams),
        dedupe_refs(clear_default_streams),
    ))
}

fn dedupe_refs(streams: Vec<RemuxStreamRef>) -> Vec<RemuxStreamRef> {
    let mut seen = HashSet::new();
    streams
        .into_iter()
        .filter(|stream| seen.insert(stream.snapshot_stream_id.clone()))
        .collect()
}

fn stream_ref(stream: &SnapshotStreamFact) -> RemuxStreamRef {
    RemuxStreamRef {
        snapshot_stream_id: stream.snapshot_stream_id.clone(),
        provider_stream_index: stream.provider_stream_index,
    }
}

fn media_snapshot_input(snapshot: &MediaSnapshot) -> MediaSnapshotInput {
    let streams = snapshot
        .payload
        .get("streams")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let video_stream_count = streams.as_array().map_or(0, |streams| {
        streams
            .iter()
            .filter(|stream| stream.get("kind").and_then(Value::as_str) == Some("video"))
            .count()
    });
    MediaSnapshotInput {
        ordinal: 1,
        target: TargetRef::FileVersion {
            id: snapshot.file_version_id,
        },
        container: snapshot
            .payload
            .get("container")
            .and_then(Value::as_str)
            .map(str::to_owned),
        stream_summary: json!({
            "video_stream_count": video_stream_count,
            "streams": streams,
        }),
        video_codec: snapshot
            .payload
            .get("video_codec")
            .and_then(Value::as_str)
            .map(str::to_owned),
        width: None,
        height: None,
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: Some(snapshot.id),
    }
}

fn remux_block_error(block: RemuxPlanningBlock) -> VoomError {
    match block {
        RemuxPlanningBlock::InsufficientSnapshotFacts => {
            VoomError::Config("remux snapshot has insufficient stream facts".to_owned())
        }
        RemuxPlanningBlock::UnsupportedMediaShape => {
            VoomError::Config("remux selector is unsupported for this media shape".to_owned())
        }
    }
}

#[derive(Debug, Clone)]
struct RemuxPayload {
    container: String,
    track_actions: Vec<TrackAction>,
    track_order: Vec<RemuxTrackGroup>,
    defaults: Vec<DefaultAction>,
}

impl RemuxPayload {
    fn parse(value: &Value) -> Result<Self, VoomError> {
        let raw = RawRemuxPayload::deserialize(value).map_err(|err| {
            VoomError::Config(format!("remux operation payload is invalid: {err}"))
        })?;
        if raw.r#type != "remux" {
            return Err(VoomError::Config(
                "remux operation payload missing type remux".to_owned(),
            ));
        }
        Ok(Self {
            container: raw.container,
            track_actions: raw
                .track_actions
                .into_iter()
                .map(TrackAction::from_raw)
                .collect::<Result<Vec<_>, _>>()?,
            track_order: raw.track_order.into_iter().map(track_group).collect(),
            defaults: raw.defaults,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawRemuxPayload {
    r#type: String,
    container: String,
    #[serde(default)]
    track_actions: Vec<RawTrackAction>,
    #[serde(default = "default_track_order")]
    track_order: Vec<TrackTarget>,
    #[serde(default)]
    defaults: Vec<DefaultAction>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTrackAction {
    r#type: String,
    target: TrackTarget,
    filter: Option<voom_policy::TrackFilter>,
}

#[derive(Debug, Clone)]
struct TrackAction {
    kind: TrackActionKind,
    target: TrackTarget,
    filter: Option<voom_policy::TrackFilter>,
}

impl TrackAction {
    fn from_raw(raw: RawTrackAction) -> Result<Self, VoomError> {
        let kind = match raw.r#type.as_str() {
            "keep_tracks" => TrackActionKind::KeepTracks,
            "remove_tracks" => TrackActionKind::RemoveTracks,
            other => {
                return Err(VoomError::Config(format!(
                    "remux track action {other} is unsupported"
                )));
            }
        };
        Ok(Self {
            kind,
            target: raw.target,
            filter: raw.filter,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackActionKind {
    KeepTracks,
    RemoveTracks,
}

#[derive(Debug, Clone, Deserialize)]
struct DefaultAction {
    target: TrackTarget,
    strategy: DefaultStrategy,
}

fn default_track_order() -> Vec<TrackTarget> {
    vec![
        TrackTarget::Video,
        TrackTarget::Audio,
        TrackTarget::Subtitle,
    ]
}

fn track_group(target: TrackTarget) -> RemuxTrackGroup {
    match target {
        TrackTarget::Video => RemuxTrackGroup::Video,
        TrackTarget::Audio => RemuxTrackGroup::Audio,
        TrackTarget::Subtitle => RemuxTrackGroup::Subtitle,
        TrackTarget::Attachment => RemuxTrackGroup::Attachment,
    }
}

#[cfg(test)]
#[path = "selection_test.rs"]
mod tests;
