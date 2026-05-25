use serde_json::{Map, Value};
use thiserror::Error;
use voom_core::FailureClass;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("artifact unavailable: {0}")]
    ArtifactUnavailable(String),
    #[error("malformed worker result: {0}")]
    MalformedWorkerResult(String),
}

impl WorkerError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        match self {
            Self::ArtifactUnavailable(_) => FailureClass::ArtifactUnavailable,
            Self::MalformedWorkerResult(_) => FailureClass::MalformedWorkerResult,
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

    let mut raw_object = Map::new();
    raw_object.insert("ffprobe_json".to_owned(), raw);
    snapshot.insert("raw".to_owned(), Value::Object(raw_object));

    Ok(Value::Object(snapshot))
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
            insert_u64_value(input, &mut output, "channels", "channels")?;
            insert_stream_language(input, &mut output);
            insert_disposition(input, &mut output)?;

            Ok(Value::Object(output))
        })
        .collect()
}

fn insert_stream_language(input: &Map<String, Value>, output: &mut Map<String, Value>) {
    let Some(tags) = input.get("tags").and_then(Value::as_object) else {
        return;
    };
    insert_string_as(tags, output, "language", "language");
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
    for key in ["default", "forced"] {
        if let Some(value) = object.get(key) {
            normalized.insert(key.to_owned(), Value::Bool(disposition_bool(value, key)?));
        }
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
