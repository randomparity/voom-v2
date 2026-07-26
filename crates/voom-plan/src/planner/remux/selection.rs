use std::collections::{BTreeSet, HashSet};

use serde_json::Value;
use voom_core::is_font_attachment_mime_type;
use voom_policy::{ComparisonOp, MediaSnapshotInput, TrackFilter, TrackTarget};

use super::{RemuxTrackAction, RemuxTrackActionKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotFact<T> {
    Missing,
    Malformed,
    Value(T),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotStreamFact {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
    pub kind: TrackTarget,
    pub codec_name: Option<String>,
    pub language: SnapshotFact<String>,
    pub channels: Option<u32>,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub filename: Option<String>,
    pub commentary: SnapshotFact<bool>,
    pub is_default: SnapshotFact<bool>,
    pub is_forced: SnapshotFact<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemuxFilterOperation {
    Defaults(TrackTarget),
    OrderTracks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemuxPlanningBlock {
    InsufficientSnapshotFacts,
    UnsupportedMediaShape,
    EmptyTrackFilterSelection {
        operation: RemuxFilterOperation,
    },
    AmbiguousTrackFilterSelection {
        operation: RemuxFilterOperation,
        match_count: usize,
    },
    ConflictingExplicitDefaults {
        target: TrackTarget,
    },
}

pub fn stream_facts(
    snapshot: &MediaSnapshotInput,
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
            language: optional_string_fact(stream.get("language")),
            channels: stream
                .get("channels")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
            title: optional_string(stream.get("title")),
            mime_type: optional_string(stream.get("mime_type")),
            filename: optional_string(stream.get("filename")),
            commentary: disposition_fact(stream.get("disposition"), "commentary"),
            is_default: disposition_fact(stream.get("disposition"), "default"),
            is_forced: disposition_fact(stream.get("disposition"), "forced"),
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
            let language = match &stream.language {
                SnapshotFact::Missing => "und",
                SnapshotFact::Value(language) => language,
                SnapshotFact::Malformed => {
                    return Err(RemuxPlanningBlock::InsufficientSnapshotFacts);
                }
            };
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
        TrackFilter::Commentary => required_bool_fact(&stream.commentary),
        TrackFilter::TitleMatches { .. } => Err(RemuxPlanningBlock::UnsupportedMediaShape),
        TrackFilter::Forced => required_bool_fact(&stream.is_forced),
        TrackFilter::Default => required_bool_fact(&stream.is_default),
        TrackFilter::Font => evaluate_font_filter(stream),
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
                    Err(
                        block @ (RemuxPlanningBlock::EmptyTrackFilterSelection { .. }
                        | RemuxPlanningBlock::AmbiguousTrackFilterSelection { .. }
                        | RemuxPlanningBlock::ConflictingExplicitDefaults { .. }),
                    ) => {
                        return Err(block);
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

pub fn resolve_track_keep_ids(
    facts: &[SnapshotStreamFact],
    actions: &[RemuxTrackAction],
) -> Result<BTreeSet<String>, RemuxPlanningBlock> {
    let mut keep_ids = facts
        .iter()
        .map(|stream| stream.snapshot_stream_id.clone())
        .collect::<BTreeSet<_>>();
    for action in actions {
        let target_streams = facts
            .iter()
            .filter(|stream| stream.kind == action.target)
            .collect::<Vec<_>>();
        if action.kind == RemuxTrackActionKind::KeepTracks {
            for stream in &target_streams {
                keep_ids.remove(&stream.snapshot_stream_id);
            }
        }
        for stream in target_streams {
            let matched = match &action.filter {
                Some(filter) => evaluate_filter(filter, stream)?,
                None => true,
            };
            if !matched {
                continue;
            }
            match action.kind {
                RemuxTrackActionKind::KeepTracks => {
                    keep_ids.insert(stream.snapshot_stream_id.clone());
                }
                RemuxTrackActionKind::RemoveTracks => {
                    keep_ids.remove(&stream.snapshot_stream_id);
                }
            }
        }
    }
    Ok(keep_ids)
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

fn optional_string_fact(value: Option<&Value>) -> SnapshotFact<String> {
    match value {
        Some(Value::String(value)) => SnapshotFact::Value(value.clone()),
        Some(Value::Null) | None => SnapshotFact::Missing,
        Some(_) => SnapshotFact::Malformed,
    }
}

fn disposition_fact(disposition: Option<&Value>, key: &str) -> SnapshotFact<bool> {
    let Some(disposition) = disposition else {
        return SnapshotFact::Missing;
    };
    let Some(disposition) = disposition.as_object() else {
        return if disposition.is_null() {
            SnapshotFact::Missing
        } else {
            SnapshotFact::Malformed
        };
    };
    match disposition.get(key) {
        Some(Value::Bool(value)) => SnapshotFact::Value(*value),
        Some(Value::Null) | None => SnapshotFact::Missing,
        Some(_) => SnapshotFact::Malformed,
    }
}

fn required_bool_fact(fact: &SnapshotFact<bool>) -> Result<bool, RemuxPlanningBlock> {
    match fact {
        SnapshotFact::Value(value) => Ok(*value),
        SnapshotFact::Missing | SnapshotFact::Malformed => {
            Err(RemuxPlanningBlock::InsufficientSnapshotFacts)
        }
    }
}

fn evaluate_font_filter(stream: &SnapshotStreamFact) -> Result<bool, RemuxPlanningBlock> {
    if stream.kind != TrackTarget::Attachment {
        return Ok(false);
    }
    let mime_type = stream
        .mime_type
        .as_deref()
        .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)?;
    Ok(is_font_attachment_mime_type(mime_type))
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
