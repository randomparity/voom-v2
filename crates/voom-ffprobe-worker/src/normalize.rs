use serde_json::{Map, Value};
use thiserror::Error;
use voom_core::FailureClass;
use voom_worker_protocol::MAX_PROBE_SNAPSHOT_BYTES;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("artifact unavailable: {0}")]
    ArtifactUnavailable(String),
    #[error("artifact checksum mismatch: {0}")]
    ArtifactChecksumMismatch(String),
    #[error("malformed worker result: {0}")]
    MalformedWorkerResult(String),
    #[error("malformed media: {0}")]
    MalformedMedia(String),
}

impl WorkerError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        match self {
            Self::ArtifactUnavailable(_) => FailureClass::ArtifactUnavailable,
            Self::ArtifactChecksumMismatch(_) => FailureClass::ArtifactChecksumMismatch,
            Self::MalformedWorkerResult(_) => FailureClass::MalformedWorkerResult,
            Self::MalformedMedia(_) => FailureClass::MalformedMedia,
        }
    }
}

pub fn normalize_ffprobe_json(
    raw: Value,
    provider_version: &str,
    probed_at: &str,
) -> Result<Value, WorkerError> {
    let root = raw
        .as_object()
        .ok_or_else(|| malformed("ffprobe output must be a JSON object"))?;

    let mut snapshot = Map::new();
    snapshot.insert("format".to_owned(), Value::String("sprint10-v1".to_owned()));
    snapshot.insert(
        "probe".to_owned(),
        Value::Object(probe_object(provider_version, probed_at)),
    );

    if let Some(format) = optional_object(root, "format")? {
        snapshot.insert(
            "container".to_owned(),
            Value::Object(container_object(format)?),
        );
    }

    if let Some(streams) = optional_array(root, "streams")? {
        snapshot.insert("streams".to_owned(), Value::Array(stream_objects(streams)?));
    }

    bound_snapshot(snapshot, raw)
}

/// Keeps the snapshot within `MAX_PROBE_SNAPSHOT_BYTES` (ADR 0122). The raw ffprobe JSON
/// goes first; a snapshot still over the bound is refused, never truncated.
fn bound_snapshot(mut snapshot: Map<String, Value>, raw: Value) -> Result<Value, WorkerError> {
    let raw_bytes = compact_len(&raw)?;
    let mut raw_object = Map::new();
    raw_object.insert("ffprobe_json".to_owned(), raw);
    snapshot.insert("raw".to_owned(), Value::Object(raw_object));
    if compact_len(&snapshot)? <= MAX_PROBE_SNAPSHOT_BYTES {
        return Ok(Value::Object(snapshot));
    }
    snapshot.insert(
        "raw".to_owned(),
        serde_json::json!({"ffprobe_json_omitted_bytes": raw_bytes}),
    );
    let bytes = compact_len(&snapshot)?;
    if bytes > MAX_PROBE_SNAPSHOT_BYTES {
        return Err(WorkerError::MalformedMedia(format!(
            "probe snapshot is {bytes} bytes without the raw ffprobe JSON, above the \
             {MAX_PROBE_SNAPSHOT_BYTES}-byte bound"
        )));
    }
    Ok(Value::Object(snapshot))
}

fn compact_len(value: &impl serde::Serialize) -> Result<usize, WorkerError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| malformed(format!("snapshot encode: {error}")))
}

fn probe_object(provider_version: &str, probed_at: &str) -> Map<String, Value> {
    let mut probe = Map::new();
    probe.insert("provider".to_owned(), Value::String("ffprobe".to_owned()));
    probe.insert(
        "provider_version".to_owned(),
        Value::String(provider_version.to_owned()),
    );
    probe.insert("command".to_owned(), Value::String("ffprobe".to_owned()));
    probe.insert("probed_at".to_owned(), Value::String(probed_at.to_owned()));
    probe
}

fn optional_object<'a>(
    input: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a Map<String, Value>>, WorkerError> {
    let Some(value) = input.get(key) else {
        return Ok(None);
    };
    value
        .as_object()
        .map(Some)
        .ok_or_else(|| malformed(format!("{key} must be a JSON object")))
}

fn optional_array<'a>(
    input: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a Vec<Value>>, WorkerError> {
    let Some(value) = input.get(key) else {
        return Ok(None);
    };
    value
        .as_array()
        .map(Some)
        .ok_or_else(|| malformed(format!("{key} must be a JSON array")))
}

fn container_object(format: &Map<String, Value>) -> Result<Map<String, Value>, WorkerError> {
    let mut container = Map::new();

    insert_string(format, &mut container, "format_name");
    insert_string(format, &mut container, "format_long_name");
    insert_f64_string(format, &mut container, "duration", "duration_seconds")?;
    insert_u64_string(format, &mut container, "bit_rate", "bit_rate")?;

    Ok(container)
}

fn stream_objects(streams: &[Value]) -> Result<Vec<Value>, WorkerError> {
    streams
        .iter()
        .map(|stream| {
            let input = stream
                .as_object()
                .ok_or_else(|| malformed("ffprobe stream must be a JSON object"))?;
            let mut output = Map::new();

            insert_u64_value(input, &mut output, "index", "index")?;
            insert_string_as(input, &mut output, "codec_type", "kind");
            insert_string(input, &mut output, "codec_name");
            insert_u64_value(input, &mut output, "width", "width")?;
            insert_u64_value(input, &mut output, "height", "height")?;
            insert_f64_string(input, &mut output, "duration", "duration_seconds")?;
            insert_string(input, &mut output, "avg_frame_rate");
            insert_u64_string(input, &mut output, "sample_rate", "sample_rate")?;
            if input.get("codec_type").and_then(Value::as_str) == Some("audio") {
                let facts = voom_worker_protocol::AudioSampleFacts::from_ffprobe(stream)
                    .map_err(|error| malformed(error.to_string()))?;
                if let Some(format) = facts.sample_format {
                    output.insert("sample_format".to_owned(), Value::String(format));
                }
                if let Some(bits) = facts.bits_per_sample {
                    output.insert("bits_per_sample".to_owned(), Value::Number(bits.into()));
                }
            }
            insert_u64_value(input, &mut output, "channels", "channels")?;
            insert_string(input, &mut output, "channel_layout");
            insert_string_as(input, &mut output, "pix_fmt", "pixel_format");
            insert_string(input, &mut output, "profile");
            // ffprobe may emit `level` as a JSON number (e.g. 153); normalize to string
            // for stable downstream comparison regardless of the ffprobe version.
            insert_u64_as_string(input, &mut output, "level");
            insert_stream_tags(input, &mut output)?;
            insert_disposition(input, &mut output)?;

            Ok(Value::Object(output))
        })
        .collect()
}

fn insert_stream_tags(
    input: &Map<String, Value>,
    output: &mut Map<String, Value>,
) -> Result<(), WorkerError> {
    let Some(tags) = input.get("tags").and_then(Value::as_object) else {
        return Ok(());
    };
    // Match by folded key: ffprobe passes Matroska custom tags such as ROLE and
    // HANDLER_NAME through uppercase, and Matroska tag names are case-insensitive
    // by spec. Folding `language`/`title` too is intentional and harmless — real
    // ffprobe emits those lowercase, so folding only adds tolerance and never
    // changes current output.
    for (input_key, output_key) in [
        ("language", "language"),
        ("title", "title"),
        ("role", "role"),
        ("handler_name", "handler_name"),
        ("filename", "filename"),
        ("mimetype", "mime_type"),
    ] {
        let value = tags
            .iter()
            .find_map(|(tag_key, tag_value)| {
                if tag_key.eq_ignore_ascii_case(input_key) {
                    tag_value.as_str()
                } else {
                    None
                }
            })
            // Drop ffprobe sentinels ("", "N/A", "unknown") so they never reach
            // the durable snapshot — the same guard insert_string_as applies.
            .filter(|value| !is_unknown_value(value));
        if let Some(value) = value {
            output.insert(output_key.to_owned(), Value::String(value.to_owned()));
        }
    }
    for (tag, field) in [
        ("voom_compatibility_stream_id", "id"),
        (
            "voom_compatibility_source_stream_id",
            "compatibility_source_snapshot_stream_id",
        ),
    ] {
        if let Some(value) = observed_identity_tag(tags, tag)? {
            output.insert(field.to_owned(), Value::String(value));
        }
    }
    if invalid_language_tags(tags) {
        output.insert(
            "compatibility_invalid_language".to_owned(),
            Value::Bool(true),
        );
    }
    Ok(())
}

fn observed_identity_tag(
    tags: &Map<String, Value>,
    name: &str,
) -> Result<Option<String>, WorkerError> {
    let mut observed: Option<String> = None;
    for (key, value) in tags
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
    {
        let value = value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| malformed(format!("{key} must be a non-empty string")))?;
        if observed
            .as_deref()
            .is_some_and(|previous| previous != value)
        {
            return Err(malformed(format!("conflicting {name} tags")));
        }
        observed = Some(value.to_owned());
    }
    Ok(observed)
}

fn invalid_language_tags(tags: &Map<String, Value>) -> bool {
    let mut observed = None;
    for (_, value) in tags
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("language"))
    {
        match value {
            Value::Null => {}
            Value::String(value) if is_unknown_value(value) => {}
            Value::String(value)
                if value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_lowercase()) =>
            {
                if observed.is_some_and(|previous| previous != value) {
                    return true;
                }
                observed = Some(value);
            }
            _ => return true,
        }
    }
    false
}

fn insert_disposition(
    input: &Map<String, Value>,
    output: &mut Map<String, Value>,
) -> Result<(), WorkerError> {
    let Some(disposition) = input.get("disposition") else {
        return Ok(());
    };
    let object = disposition
        .as_object()
        .ok_or_else(|| malformed("disposition must be a JSON object"))?;
    let mut normalized = Map::new();
    // ffprobe names the commentary flag `comment`; the downstream audio planner
    // reads it as `commentary` (a required preservation fact for transcodes).
    for (input_key, output_key) in [
        ("default", "default"),
        ("forced", "forced"),
        ("comment", "commentary"),
    ] {
        if let Some(value) = object.get(input_key) {
            normalized.insert(
                output_key.to_owned(),
                Value::Bool(disposition_bool(value, input_key)?),
            );
        }
    }
    // ADR 0125: cover art is a `video` stream whose attachment identity is this flag.
    // Written only when true so snapshots of files without cover art keep their shape.
    if let Some(value) = object.get("attached_pic")
        && disposition_bool(value, "attached_pic")?
    {
        normalized.insert("attached_pic".to_owned(), Value::Bool(true));
    }
    if !normalized.is_empty() {
        output.insert("disposition".to_owned(), Value::Object(normalized));
    }
    Ok(())
}

fn disposition_bool(value: &Value, key: &str) -> Result<bool, WorkerError> {
    if let Some(value) = value.as_bool() {
        return Ok(value);
    }
    if let Some(value) = value.as_u64() {
        return match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(malformed(format!("{key} disposition must be 0 or 1"))),
        };
    }
    if let Some(value) = value.as_str() {
        return match value {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(malformed(format!("{key} disposition must be 0 or 1"))),
        };
    }
    Err(malformed(format!("{key} disposition must be 0 or 1")))
}

fn insert_string(input: &Map<String, Value>, output: &mut Map<String, Value>, key: &str) {
    insert_string_as(input, output, key, key);
}

fn insert_string_as(
    input: &Map<String, Value>,
    output: &mut Map<String, Value>,
    input_key: &str,
    output_key: &str,
) {
    if let Some(value) = input
        .get(input_key)
        .and_then(Value::as_str)
        .filter(|value| !is_unknown_value(value))
    {
        output.insert(output_key.to_owned(), Value::String(value.to_owned()));
    }
}

/// Inserts `key` from `input` into `output` as a JSON string, regardless of
/// whether the raw ffprobe value is a number or already a string. Absent or
/// sentinel values are omitted. The output key equals the input key.
/// Numeric values are treated as unsigned; negative numbers are omitted.
fn insert_u64_as_string(input: &Map<String, Value>, output: &mut Map<String, Value>, key: &str) {
    let Some(value) = input.get(key) else {
        return;
    };
    // Already a string — delegate to the sentinel-filtering path.
    if value.is_string() {
        insert_string(input, output, key);
        return;
    }
    // Numeric value: stringify directly.
    if let Some(n) = value.as_u64() {
        output.insert(key.to_owned(), Value::String(n.to_string()));
    }
}

fn insert_f64_string(
    input: &Map<String, Value>,
    output: &mut Map<String, Value>,
    input_key: &str,
    output_key: &str,
) -> Result<(), WorkerError> {
    let Some(value) = input.get(input_key) else {
        return Ok(());
    };
    let Some(parsed) = optional_f64(value, input_key)? else {
        return Ok(());
    };
    if !parsed.is_finite() {
        return Err(malformed(format!("{input_key} must be finite")));
    }
    let number = serde_json::Number::from_f64(parsed)
        .ok_or_else(|| malformed(format!("{input_key} must be representable as JSON")))?;
    output.insert(output_key.to_owned(), Value::Number(number));
    Ok(())
}

fn insert_u64_string(
    input: &Map<String, Value>,
    output: &mut Map<String, Value>,
    input_key: &str,
    output_key: &str,
) -> Result<(), WorkerError> {
    let Some(value) = input.get(input_key) else {
        return Ok(());
    };
    let Some(parsed) = optional_u64(value, input_key)? else {
        return Ok(());
    };
    output.insert(output_key.to_owned(), Value::Number(parsed.into()));
    Ok(())
}

fn insert_u64_value(
    input: &Map<String, Value>,
    output: &mut Map<String, Value>,
    input_key: &str,
    output_key: &str,
) -> Result<(), WorkerError> {
    let Some(value) = input.get(input_key) else {
        return Ok(());
    };
    let Some(parsed) = optional_u64(value, input_key)? else {
        return Ok(());
    };
    output.insert(output_key.to_owned(), Value::Number(parsed.into()));
    Ok(())
}

fn optional_f64(value: &Value, key: &str) -> Result<Option<f64>, WorkerError> {
    if let Some(raw) = value.as_str() {
        if is_unknown_value(raw) {
            return Ok(None);
        }
        return raw
            .parse::<f64>()
            .map(Some)
            .map_err(|_| malformed(format!("{key} must be numeric")));
    }
    if let Some(parsed) = value.as_f64() {
        return Ok(Some(parsed));
    }
    Err(malformed(format!("{key} must be numeric")))
}

fn optional_u64(value: &Value, key: &str) -> Result<Option<u64>, WorkerError> {
    if let Some(raw) = value.as_str() {
        if is_unknown_value(raw) {
            return Ok(None);
        }
        return raw
            .parse::<u64>()
            .map(Some)
            .map_err(|_| malformed(format!("{key} must be an unsigned integer")));
    }
    if let Some(parsed) = value.as_u64() {
        return Ok(Some(parsed));
    }
    Err(malformed(format!("{key} must be an unsigned integer")))
}

fn is_unknown_value(value: &str) -> bool {
    matches!(value.trim(), "" | "N/A" | "n/a" | "unknown")
}

fn malformed(message: impl Into<String>) -> WorkerError {
    WorkerError::MalformedWorkerResult(message.into())
}

#[cfg(test)]
#[path = "normalize_test.rs"]
mod tests;
