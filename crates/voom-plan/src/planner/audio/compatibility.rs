use super::payload::CompatibilityStreamIdentity;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use voom_policy::MediaSnapshotInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCompatibilityTarget {
    pub codec: String,
    pub channels: u32,
    pub channel_layout: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCompatibilityResolution {
    pub retained_streams: Vec<CompatibilityStreamIdentity>,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub source_channels: u32,
    pub source_channel_layout: String,
    pub target: AudioCompatibilityTarget,
    /// None means ensure must encode; place must diagnose the missing companion.
    pub resolved_snapshot_stream_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioCompatibilityError {
    InvalidFacts(String),
    NoEligibleSource,
    UnsupportedLayout { channels: u32, layout: String },
    InvalidAssociation(String),
}

pub fn compatibility_target(
    channels: u32,
    layout: &str,
) -> Result<AudioCompatibilityTarget, AudioCompatibilityError> {
    let codec = match (channels, layout) {
        (1, "mono") | (2, "stereo") => "aac",
        (3, "2.1" | "3.0" | "3.0(back)")
        | (4, "3.1" | "4.0" | "quad" | "quad(side)" | "4 channels (FL+FR+LFE+BC)")
        | (5, "4.1" | "5.0" | "5.0(side)")
        | (6, "5.1" | "5.1(side)")
        | (8, "7.1") => "eac3",
        _ => {
            return Err(AudioCompatibilityError::UnsupportedLayout {
                channels,
                layout: layout.to_owned(),
            });
        }
    };
    Ok(AudioCompatibilityTarget {
        codec: codec.to_owned(),
        channels: if channels == 8 { 6 } else { channels },
        channel_layout: match layout {
            "quad" => "quad(side)",
            "5.0" => "5.0(side)",
            "5.1" | "7.1" => "5.1(side)",
            other => other,
        }
        .to_owned(),
    })
}

fn fact_error(stream: &Value, key: &str) -> AudioCompatibilityError {
    let label = stream.get("id").and_then(Value::as_str).map_or_else(
        || "<unidentified>".to_owned(),
        |id| id.chars().take(80).collect::<String>(),
    );
    AudioCompatibilityError::InvalidFacts(format!("invalid {key} for stream {label}"))
}

fn text<'a>(stream: &'a Value, key: &str) -> Result<&'a str, AudioCompatibilityError> {
    stream
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| fact_error(stream, key))
}

fn association(stream: &Value) -> Result<Option<&str>, AudioCompatibilityError> {
    match stream.get("compatibility_source_snapshot_stream_id") {
        None => Ok(None),
        Some(Value::String(id)) if !id.trim().is_empty() => Ok(Some(id)),
        Some(_) => Err(AudioCompatibilityError::InvalidAssociation(
            "compatibility_source_snapshot_stream_id must be a nonempty string".to_owned(),
        )),
    }
}

fn language(stream: &Value) -> Result<&str, AudioCompatibilityError> {
    if stream
        .get("compatibility_invalid_language")
        .is_some_and(|v| v != &Value::Bool(false))
    {
        return Err(fact_error(stream, "language"));
    }
    match stream.get("language") {
        None => Ok("und"),
        Some(Value::String(value))
            if value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_lowercase()) =>
        {
            Ok(value)
        }
        Some(_) => Err(fact_error(stream, "language")),
    }
}

fn channels(stream: &Value) -> Result<u32, AudioCompatibilityError> {
    stream
        .get("channels")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| fact_error(stream, "channels"))
}

fn commentary(stream: &Value) -> Result<bool, AudioCompatibilityError> {
    stream
        .pointer("/disposition/commentary")
        .or_else(|| stream.pointer("/disposition/comment"))
        .and_then(Value::as_bool)
        .ok_or_else(|| fact_error(stream, "commentary"))
}

fn validate_present_audio_facts(stream: &Value) -> Result<(), AudioCompatibilityError> {
    if stream.get("channels").is_some() {
        channels(stream)?;
    }
    for key in ["codec_name", "channel_layout"] {
        if stream.get(key).is_some() {
            text(stream, key)?;
        }
    }
    if stream.pointer("/disposition/commentary").is_some()
        || stream.pointer("/disposition/comment").is_some()
    {
        commentary(stream)?;
    }
    Ok(())
}

fn validate_associations(
    derived: &[(&str, &Value)],
    by_id: &BTreeMap<&str, &Value>,
) -> Result<(), AudioCompatibilityError> {
    for (source_id, companion) in derived {
        let source = by_id.get(source_id).ok_or_else(|| {
            AudioCompatibilityError::InvalidAssociation(format!("missing source {source_id}"))
        })?;
        if text(source, "kind")? != "audio"
            || association(source)?.is_some()
            || text(companion, "id")? == *source_id
        {
            return Err(AudioCompatibilityError::InvalidAssociation(format!(
                "non-original source {source_id}"
            )));
        }
    }
    Ok(())
}

pub fn resolve_audio_compatibility(
    snapshot: &MediaSnapshotInput,
) -> Result<AudioCompatibilityResolution, AudioCompatibilityError> {
    let streams = snapshot
        .stream_summary
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| AudioCompatibilityError::InvalidFacts("streams absent".to_owned()))?;
    let mut by_id = BTreeMap::new();
    let mut indexes = BTreeSet::new();
    let mut originals = Vec::new();
    let mut derived = Vec::new();
    let mut retained_streams = Vec::with_capacity(streams.len());
    for stream in streams {
        let id = text(stream, "id")?;
        let index = stream
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| AudioCompatibilityError::InvalidFacts(format!("invalid index: {id}")))?;
        if by_id.insert(id, stream).is_some() || !indexes.insert(index) {
            return Err(AudioCompatibilityError::InvalidFacts(format!(
                "duplicate stream identity: {id}"
            )));
        }
        retained_streams.push(CompatibilityStreamIdentity {
            snapshot_stream_id: id.to_owned(),
            provider_stream_index: index,
        });
        if text(stream, "kind")? != "audio" {
            continue;
        }
        validate_present_audio_facts(stream)?;
        if let Some(source_id) = association(stream)? {
            derived.push((source_id, stream));
            continue;
        }
        let lang = language(stream)?;
        if matches!(lang, "eng" | "und") && !commentary(stream)? {
            originals.push((u8::from(lang != "eng"), index, stream));
        }
    }
    validate_associations(&derived, &by_id)?;
    originals.sort_by_key(|(rank, index, _)| (*rank, *index));
    let (_, index, source) = originals
        .first()
        .ok_or(AudioCompatibilityError::NoEligibleSource)?;
    let source_id = text(source, "id")?;
    let source_channels = channels(source)?;
    let source_layout = text(source, "channel_layout")?;
    let source_codec = text(source, "codec_name")?;
    let target = compatibility_target(source_channels, source_layout)?;
    let companions = derived
        .iter()
        .filter(|(id, _)| *id == source_id)
        .collect::<Vec<_>>();
    if companions.len() > 1 {
        return Err(AudioCompatibilityError::InvalidAssociation(format!(
            "multiple companions for {source_id}"
        )));
    }
    let associated_id = match companions.as_slice() {
        [] => None,
        [(_, companion)] => {
            if text(companion, "codec_name")? != target.codec
                || channels(companion)? != target.channels
                || text(companion, "channel_layout")? != target.channel_layout
                || language(companion)? != language(source)?
                || commentary(companion)?
            {
                return Err(AudioCompatibilityError::InvalidAssociation(format!(
                    "incompatible companion for {source_id}"
                )));
            }
            Some(text(companion, "id")?.to_owned())
        }
        _ => {
            return Err(AudioCompatibilityError::InvalidAssociation(
                source_id.to_owned(),
            ));
        }
    };
    // The three encoder canonical aliases do not require another encode.
    let original_matches = source_codec == target.codec && source_channels == target.channels;
    retained_streams.sort_by_key(|stream| stream.provider_stream_index);
    Ok(AudioCompatibilityResolution {
        retained_streams,
        source_snapshot_stream_id: source_id.to_owned(),
        source_provider_stream_index: *index,
        source_channels,
        source_channel_layout: source_layout.to_owned(),
        target,
        resolved_snapshot_stream_id: if original_matches {
            Some(source_id.to_owned())
        } else {
            associated_id
        },
    })
}

#[cfg(test)]
#[path = "compatibility_test.rs"]
mod tests;
