use std::collections::HashSet;

use serde_json::Value;
use voom_policy::{ComparisonOp, MediaSnapshotInput, TrackFilter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAudioStreamFact {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub channels: Option<u32>,
    pub default: bool,
    pub disposition: AudioDispositionFact,
    pub commentary: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDispositionFact {
    pub default: bool,
    pub forced: bool,
    pub commentary: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBundleRole {
    CommentaryAudio,
    ExternalAudio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlanningBlock {
    InsufficientSnapshotFacts,
    UnsupportedSelector,
    ZeroMatches,
    MultipleMatches,
    NoVideo,
    UnsupportedMediaShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlanShape {
    NoOp,
    Planned,
    Blocked(AudioPlanningBlock),
}

pub fn stream_facts(
    snapshot: &MediaSnapshotInput,
) -> Result<Vec<SnapshotAudioStreamFact>, AudioPlanningBlock> {
    let streams = snapshot
        .stream_summary
        .get("streams")
        .and_then(Value::as_array)
        .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
    let mut ids = HashSet::with_capacity(streams.len());
    let mut facts = Vec::new();

    for stream in streams {
        let stream = stream
            .as_object()
            .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
        let kind = required_string(stream.get("kind"))?;
        if kind != "audio" {
            continue;
        }
        let snapshot_stream_id = required_string(stream.get("id"))?;
        if !ids.insert(snapshot_stream_id.clone()) {
            return Err(AudioPlanningBlock::InsufficientSnapshotFacts);
        }
        let provider_stream_index = stream
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
        let disposition = audio_disposition(stream.get("disposition"));

        facts.push(SnapshotAudioStreamFact {
            snapshot_stream_id,
            provider_stream_index,
            codec: optional_string(stream.get("codec_name")),
            language: optional_string(stream.get("language")),
            title: optional_string(stream.get("title")),
            channels: stream
                .get("channels")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
            default: disposition.default,
            commentary: disposition.commentary,
            disposition,
        });
    }

    Ok(facts)
}

pub fn evaluate_audio_filter(
    filter: &TrackFilter,
    stream: &SnapshotAudioStreamFact,
) -> Result<bool, AudioPlanningBlock> {
    if audio_filter_has_unsupported_selector(filter) {
        return Err(AudioPlanningBlock::UnsupportedSelector);
    }
    evaluate_supported_audio_filter(filter, stream)
}

fn evaluate_supported_audio_filter(
    filter: &TrackFilter,
    stream: &SnapshotAudioStreamFact,
) -> Result<bool, AudioPlanningBlock> {
    match filter {
        TrackFilter::LanguageIn { values } => {
            let language = stream
                .language
                .as_ref()
                .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(values.iter().any(|value| value == language))
        }
        TrackFilter::CodecIn { values } => {
            let codec = stream
                .codec
                .as_ref()
                .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(values.iter().any(|value| value == codec))
        }
        TrackFilter::Channels { op, value } => {
            let channels = stream
                .channels
                .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(compare_u64(u64::from(channels), *op, *value))
        }
        TrackFilter::Commentary => stream
            .commentary
            .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts),
        TrackFilter::Forced => Ok(stream.disposition.forced),
        TrackFilter::Default => Ok(stream.default),
        TrackFilter::TitleContains { value } => {
            let title = stream
                .title
                .as_ref()
                .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(title.contains(value))
        }
        TrackFilter::Not { inner } => Ok(!evaluate_supported_audio_filter(inner, stream)?),
        TrackFilter::And { filters } => {
            let mut insufficient = false;
            for filter in filters {
                match evaluate_supported_audio_filter(filter, stream) {
                    Ok(true) => {}
                    Ok(false) => return Ok(false),
                    Err(AudioPlanningBlock::InsufficientSnapshotFacts) => insufficient = true,
                    Err(err) => return Err(err),
                }
            }
            if insufficient {
                Err(AudioPlanningBlock::InsufficientSnapshotFacts)
            } else {
                Ok(true)
            }
        }
        TrackFilter::Or { filters } => {
            let mut insufficient = false;
            for filter in filters {
                match evaluate_supported_audio_filter(filter, stream) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(AudioPlanningBlock::InsufficientSnapshotFacts) => insufficient = true,
                    Err(err) => return Err(err),
                }
            }
            if insufficient {
                Err(AudioPlanningBlock::InsufficientSnapshotFacts)
            } else {
                Ok(false)
            }
        }
        TrackFilter::Font | TrackFilter::TitleMatches { .. } => {
            Err(AudioPlanningBlock::UnsupportedSelector)
        }
    }
}

fn audio_filter_has_unsupported_selector(filter: &TrackFilter) -> bool {
    match filter {
        TrackFilter::Font | TrackFilter::TitleMatches { .. } => true,
        TrackFilter::Not { inner } => audio_filter_has_unsupported_selector(inner),
        TrackFilter::And { filters } | TrackFilter::Or { filters } => {
            filters.iter().any(audio_filter_has_unsupported_selector)
        }
        TrackFilter::LanguageIn { .. }
        | TrackFilter::CodecIn { .. }
        | TrackFilter::Channels { .. }
        | TrackFilter::Commentary
        | TrackFilter::Forced
        | TrackFilter::Default
        | TrackFilter::TitleContains { .. } => false,
    }
}

#[must_use]
pub fn transcode_audio_shape(
    snapshot: &MediaSnapshotInput,
    target_codec: &str,
    container: &str,
    filter: Option<&TrackFilter>,
) -> AudioPlanShape {
    let selected = match selected_audio_streams(snapshot, filter) {
        Ok(selected) => selected,
        Err(block) => return AudioPlanShape::Blocked(block),
    };
    if selected.is_empty() {
        return AudioPlanShape::Blocked(AudioPlanningBlock::ZeroMatches);
    }
    let Some(current_container) = snapshot.container.as_deref() else {
        return AudioPlanShape::Blocked(AudioPlanningBlock::InsufficientSnapshotFacts);
    };
    if selected.iter().any(|stream| stream.codec.is_none()) {
        return AudioPlanShape::Blocked(AudioPlanningBlock::InsufficientSnapshotFacts);
    }
    if selected
        .iter()
        .any(|stream| !has_transcode_preservation_facts(stream))
    {
        return AudioPlanShape::Blocked(AudioPlanningBlock::InsufficientSnapshotFacts);
    }

    if current_container.eq_ignore_ascii_case(container)
        && selected
            .iter()
            .all(|stream| stream.codec.as_deref() == Some(target_codec))
    {
        AudioPlanShape::NoOp
    } else {
        AudioPlanShape::Planned
    }
}

#[must_use]
pub fn extract_audio_shape(
    snapshot: &MediaSnapshotInput,
    filter: Option<&TrackFilter>,
) -> AudioPlanShape {
    let selected = match selected_audio_streams(snapshot, filter) {
        Ok(selected) => selected,
        Err(block) => return AudioPlanShape::Blocked(block),
    };
    match selected.len() {
        0 => AudioPlanShape::Blocked(AudioPlanningBlock::ZeroMatches),
        1 => match extraction_role(&selected[0]) {
            Ok(AudioBundleRole::CommentaryAudio | AudioBundleRole::ExternalAudio) => {
                AudioPlanShape::Planned
            }
            Err(block) => AudioPlanShape::Blocked(block),
        },
        _ => AudioPlanShape::Blocked(AudioPlanningBlock::MultipleMatches),
    }
}

pub fn extraction_role(
    stream: &SnapshotAudioStreamFact,
) -> Result<AudioBundleRole, AudioPlanningBlock> {
    match stream.commentary {
        Some(true) => Ok(AudioBundleRole::CommentaryAudio),
        Some(false) => Ok(AudioBundleRole::ExternalAudio),
        None => Err(AudioPlanningBlock::InsufficientSnapshotFacts),
    }
}

pub fn selected_audio_streams(
    snapshot: &MediaSnapshotInput,
    filter: Option<&TrackFilter>,
) -> Result<Vec<SnapshotAudioStreamFact>, AudioPlanningBlock> {
    if video_stream_count(snapshot)? == 0 {
        return Err(AudioPlanningBlock::NoVideo);
    }
    let facts = stream_facts(snapshot)?;
    let mut selected = Vec::new();
    for stream in facts {
        let matches = match filter {
            Some(filter) => evaluate_audio_filter(filter, &stream)?,
            None => true,
        };
        if matches {
            selected.push(stream);
        }
    }
    Ok(selected)
}

/// Returns whether a selected audio stream carries the facts required to
/// preserve its metadata across a transcode (language, title, channels, and a
/// known commentary disposition). Audio transcode planning and the
/// control-plane runtime selection share this rule.
#[must_use]
pub fn has_transcode_preservation_facts(stream: &SnapshotAudioStreamFact) -> bool {
    stream.language.is_some()
        && stream.title.is_some()
        && stream.channels.is_some()
        && stream.disposition.commentary.is_some()
}

fn video_stream_count(snapshot: &MediaSnapshotInput) -> Result<u64, AudioPlanningBlock> {
    snapshot
        .stream_summary
        .get("video_stream_count")
        .and_then(Value::as_u64)
        .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)
}

fn required_string(value: Option<&Value>) -> Result<String, AudioPlanningBlock> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn audio_disposition(disposition: Option<&Value>) -> AudioDispositionFact {
    AudioDispositionFact {
        default: disposition_flag(disposition, "default"),
        forced: disposition_flag(disposition, "forced"),
        commentary: disposition
            .and_then(Value::as_object)
            .and_then(|object| object.get("commentary").or_else(|| object.get("comment")))
            .and_then(Value::as_bool),
    }
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
#[path = "selection_test.rs"]
mod tests;
