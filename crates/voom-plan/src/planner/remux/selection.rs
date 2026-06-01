use std::collections::HashSet;

use serde_json::Value;
use voom_policy::{ComparisonOp, MediaSnapshotInput, TrackFilter, TrackTarget};

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
#[path = "selection_test.rs"]
mod tests;
