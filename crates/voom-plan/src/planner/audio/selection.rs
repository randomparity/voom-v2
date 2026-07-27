use std::collections::{HashMap, HashSet};

use serde_json::Value;
use voom_policy::{ComparisonOp, MediaSnapshotInput, TrackFilter};

use super::payload::{
    ExtractAudioOutputDescriptor, SynthesizeAudioCompanionDescriptor, extract_output_id,
    synthesis_companion_id,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioBundleRole {
    CommentaryAudio,
    ExternalAudio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlanningBlock {
    InsufficientSnapshotFacts,
    UnsupportedSelector,
    ZeroMatches,
    NoVideo,
    UnsupportedMediaShape,
    /// `synthesize audio` target channel count is not a downmix (>= the source
    /// stream's channel count, or zero). Synthesis only adds a *downmixed*
    /// companion. See ADR 0026 (#276).
    SynthesisNotDownmix,
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
    let mut provider_indexes = HashSet::with_capacity(streams.len());
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
        if !provider_indexes.insert(provider_stream_index) {
            return Err(AudioPlanningBlock::InsufficientSnapshotFacts);
        }
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
        TrackFilter::LanguageIn(voom_policy::compiled::LanguageInTrackFilter { values }) => {
            // A missing language tag matches as `und` (ISO 639-2 undetermined)
            // rather than blocking planning (ADR 0021, issue #272).
            let language = stream.language.as_deref().unwrap_or("und");
            Ok(values.iter().any(|value| value == language))
        }
        TrackFilter::CodecIn(voom_policy::compiled::CodecInTrackFilter { values }) => {
            let codec = stream
                .codec
                .as_ref()
                .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(values.iter().any(|value| value == codec))
        }
        TrackFilter::Channels(voom_policy::compiled::ChannelsTrackFilter { op, value }) => {
            let channels = stream
                .channels
                .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(compare_u64(u64::from(channels), *op, *value))
        }
        TrackFilter::Commentary(voom_policy::compiled::CommentaryTrackFilter {}) => stream
            .commentary
            .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts),
        TrackFilter::Forced(voom_policy::compiled::ForcedTrackFilter {}) => {
            Ok(stream.disposition.forced)
        }
        TrackFilter::Default(voom_policy::compiled::DefaultTrackFilter {}) => Ok(stream.default),
        TrackFilter::TitleContains(voom_policy::compiled::TitleContainsTrackFilter { value }) => {
            let title = stream
                .title
                .as_ref()
                .ok_or(AudioPlanningBlock::InsufficientSnapshotFacts)?;
            Ok(title.contains(value))
        }
        TrackFilter::Not(voom_policy::compiled::NotTrackFilter { inner }) => {
            Ok(!evaluate_supported_audio_filter(inner, stream)?)
        }
        TrackFilter::And(voom_policy::compiled::AndTrackFilter { filters }) => {
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
        TrackFilter::Or(voom_policy::compiled::OrTrackFilter { filters }) => {
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
        TrackFilter::Font(voom_policy::compiled::FontTrackFilter {})
        | TrackFilter::TitleMatches(voom_policy::compiled::TitleMatchesTrackFilter { .. }) => {
            Err(AudioPlanningBlock::UnsupportedSelector)
        }
    }
}

fn audio_filter_has_unsupported_selector(filter: &TrackFilter) -> bool {
    match filter {
        TrackFilter::Font(voom_policy::compiled::FontTrackFilter {})
        | TrackFilter::TitleMatches(voom_policy::compiled::TitleMatchesTrackFilter { .. }) => true,
        TrackFilter::Not(voom_policy::compiled::NotTrackFilter { inner }) => {
            audio_filter_has_unsupported_selector(inner)
        }
        TrackFilter::And(voom_policy::compiled::AndTrackFilter { filters })
        | TrackFilter::Or(voom_policy::compiled::OrTrackFilter { filters }) => {
            filters.iter().any(audio_filter_has_unsupported_selector)
        }
        TrackFilter::LanguageIn(voom_policy::compiled::LanguageInTrackFilter { .. })
        | TrackFilter::CodecIn(voom_policy::compiled::CodecInTrackFilter { .. })
        | TrackFilter::Channels(voom_policy::compiled::ChannelsTrackFilter { .. })
        | TrackFilter::Commentary(voom_policy::compiled::CommentaryTrackFilter {})
        | TrackFilter::Forced(voom_policy::compiled::ForcedTrackFilter {})
        | TrackFilter::Default(voom_policy::compiled::DefaultTrackFilter {})
        | TrackFilter::TitleContains(voom_policy::compiled::TitleContainsTrackFilter { .. }) => {
            false
        }
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

/// Plan shape for `synthesize audio` (ADR 0026, #276). Synthesis *adds* a
/// downmixed companion per selected source stream, so it is never a `NoOp`. It
/// is blocked when the filter matches nothing, when a selected source has no
/// known channel count, or when the target channel count is not a downmix
/// (zero, or ≥ the source's channels).
#[must_use]
pub fn synthesize_audio_shape(
    snapshot: &MediaSnapshotInput,
    target_channels: u64,
    filter: Option<&TrackFilter>,
) -> AudioPlanShape {
    let selected = match selected_audio_streams(snapshot, filter) {
        Ok(selected) => selected,
        Err(block) => return AudioPlanShape::Blocked(block),
    };
    if selected.is_empty() {
        return AudioPlanShape::Blocked(AudioPlanningBlock::ZeroMatches);
    }
    for stream in &selected {
        let Some(channels) = stream.channels else {
            return AudioPlanShape::Blocked(AudioPlanningBlock::InsufficientSnapshotFacts);
        };
        if target_channels == 0 || target_channels >= u64::from(channels) {
            return AudioPlanShape::Blocked(AudioPlanningBlock::SynthesisNotDownmix);
        }
    }
    AudioPlanShape::Planned
}

pub fn synthesize_audio_companions(
    snapshot: &MediaSnapshotInput,
    filter: Option<&TrackFilter>,
    operation_id: &str,
) -> Result<Vec<SynthesizeAudioCompanionDescriptor>, AudioPlanningBlock> {
    let mut selected = selected_audio_streams(snapshot, filter)?;
    if selected.is_empty() {
        return Err(AudioPlanningBlock::ZeroMatches);
    }
    selected.sort_by_key(|stream| stream.provider_stream_index);
    Ok(selected
        .into_iter()
        .map(|stream| {
            let companion_id = synthesis_companion_id(operation_id, &stream.snapshot_stream_id);
            SynthesizeAudioCompanionDescriptor {
                result_snapshot_stream_id: companion_id.clone(),
                companion_id,
                source_snapshot_stream_id: stream.snapshot_stream_id,
                source_provider_stream_index: stream.provider_stream_index,
            }
        })
        .collect())
}

pub fn extract_audio_outputs(
    snapshot: &MediaSnapshotInput,
    filter: Option<&TrackFilter>,
    operation_id: &str,
    target_codec: &str,
) -> Result<Vec<ExtractAudioOutputDescriptor>, AudioPlanningBlock> {
    let mut selected = selected_audio_streams(snapshot, filter)?;
    if selected.is_empty() {
        return Err(AudioPlanningBlock::ZeroMatches);
    }
    selected.sort_by_key(|stream| stream.provider_stream_index);
    let mut outputs = Vec::with_capacity(selected.len());
    for stream in selected {
        let bundle_role = extraction_role(&stream)?;
        outputs.push(ExtractAudioOutputDescriptor {
            output_id: extract_output_id(operation_id, &stream.snapshot_stream_id),
            source_snapshot_stream_id: stream.snapshot_stream_id,
            source_provider_stream_index: stream.provider_stream_index,
            name_suffix: String::new(),
            bundle_role,
        });
    }
    let names = extract_output_name_suffixes(&outputs, target_codec)?;
    for (output, name_suffix) in outputs.iter_mut().zip(names) {
        output.name_suffix = name_suffix;
    }
    Ok(outputs)
}

fn extract_output_name_suffixes(
    outputs: &[ExtractAudioOutputDescriptor],
    target_codec: &str,
) -> Result<Vec<String>, AudioPlanningBlock> {
    let bases = outputs
        .iter()
        .map(|output| sanitize_component(&output.source_snapshot_stream_id))
        .collect::<Vec<_>>();
    let mut names = bases
        .iter()
        .map(|base| format!("{base}.{target_codec}.ogg"))
        .collect::<Vec<_>>();
    let mut suffixed = vec![false; outputs.len()];

    loop {
        let collisions = collision_indexes(&names);
        if collisions.is_empty() {
            return Ok(names);
        }
        let mut changed = false;
        for index in collisions {
            if suffixed[index] {
                continue;
            }
            let output_hash = outputs[index]
                .output_id
                .strip_prefix("extract_output_")
                .ok_or(AudioPlanningBlock::UnsupportedMediaShape)?;
            names[index] = format!("{}-{output_hash}.{target_codec}.ogg", bases[index]);
            suffixed[index] = true;
            changed = true;
        }
        if !changed {
            return Err(AudioPlanningBlock::UnsupportedMediaShape);
        }
    }
}

fn collision_indexes(names: &[String]) -> Vec<usize> {
    let mut counts = HashMap::with_capacity(names.len());
    for name in names {
        *counts.entry(name.to_ascii_lowercase()).or_insert(0_usize) += 1;
    }
    names
        .iter()
        .enumerate()
        .filter_map(|(index, name)| (counts[&name.to_ascii_lowercase()] > 1).then_some(index))
        .collect()
}

fn sanitize_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "stream".to_owned()
    } else {
        sanitized
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
