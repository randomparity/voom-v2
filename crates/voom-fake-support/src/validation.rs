use voom_worker_protocol::{
    ExtractAudioRequest, OperationKind, OperationRequest, ProtocolError, RemuxRequest,
    TranscodeAudioRequest, TranscodeVideoRequest, canonical_video_codec,
    is_supported_transcode_video_codec, is_supported_transcode_video_container,
};

use crate::catalog::ProviderKind;

pub(crate) const MAX_FAKE_DURATION_MS: u64 = 30_000;
pub(crate) const MAX_FAKE_FAN_OUT_COUNT: u32 = 1_000;
const MAX_FAKE_PROGRESS_FRAMES: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimingControls {
    pub(crate) duration_ms: u64,
    pub(crate) progress_interval_ms: u64,
    pub(crate) fan_out_count: Option<u32>,
}

impl TimingControls {
    pub(crate) fn from_payload(payload: &serde_json::Value) -> Result<Self, ProtocolError> {
        let duration_ms = optional_u64(payload, "duration_ms")?.unwrap_or(0);
        let progress_interval_ms =
            optional_u64(payload, "progress_interval_ms")?.unwrap_or(duration_ms);
        let fan_out_count = optional_u64(payload, "fan_out_count")?
            .map(u32::try_from)
            .transpose()
            .map_err(|_| invalid("fan_out_count out of range"))?;

        if matches!(fan_out_count, Some(0)) {
            return Err(invalid("fan_out_count must be positive"));
        }
        if fan_out_count.is_some_and(|count| count > MAX_FAKE_FAN_OUT_COUNT) {
            return Err(invalid("fan_out_count exceeds fake-provider cap"));
        }
        if duration_ms > MAX_FAKE_DURATION_MS {
            return Err(invalid("duration_ms exceeds fake-provider cap"));
        }
        if progress_interval_ms == 0 && duration_ms > 0 {
            return Err(invalid(
                "progress_interval_ms must be positive for timed runs",
            ));
        }
        if duration_ms > 0 {
            let frame_count = duration_ms.div_ceil(progress_interval_ms);
            if frame_count > MAX_FAKE_PROGRESS_FRAMES {
                return Err(invalid(
                    "timed progress frame count exceeds fake-provider cap",
                ));
            }
        }

        Ok(Self {
            duration_ms,
            progress_interval_ms,
            fan_out_count,
        })
    }
}

pub(crate) fn validate_payload(
    kind: ProviderKind,
    req: &OperationRequest,
) -> Result<(), ProtocolError> {
    match kind {
        ProviderKind::Scanner => {
            require_field(&req.payload, "path", "/library")?;
        }
        ProviderKind::Prober
        | ProviderKind::BackupStore
        | ProviderKind::HealthChecker
        | ProviderKind::IdentityProvider => {
            require_path(&req.payload)?;
        }
        ProviderKind::Transcoder => match req.operation {
            OperationKind::TranscodeVideo => {
                if let Some(request) = transcode_video_protocol_payload(&req.payload)? {
                    validate_transcode_video_request(&request)?;
                } else {
                    require_path(&req.payload)?;
                    require_field(&req.payload, "target_codec", "h265")?;
                }
            }
            OperationKind::TranscodeAudio => {
                if let Some(request) = transcode_audio_protocol_payload(&req.payload)? {
                    validate_transcode_audio_request(&request)?;
                } else {
                    require_path(&req.payload)?;
                    require_one_of(&req.payload, "target_codec", &["aac", "opus"])?;
                }
            }
            OperationKind::ExtractAudio => {
                if let Some(request) = extract_audio_protocol_payload(&req.payload)? {
                    validate_extract_audio_request(&request)?;
                } else {
                    require_path(&req.payload)?;
                    require_field(&req.payload, "target_codec", "h265")?;
                }
            }
            _ => {
                require_path(&req.payload)?;
                require_field(&req.payload, "target_codec", "h265")?;
            }
        },
        ProviderKind::Remuxer => {
            if let Some(request) = remux_protocol_payload(&req.payload)? {
                if request.input.path.trim().is_empty() {
                    return Err(invalid("remux input.path must not be empty"));
                }
                if request.output.container != "mkv" {
                    return Err(invalid("remux output.container must be mkv"));
                }
            } else {
                require_path(&req.payload)?;
                require_field(&req.payload, "container", "mkv")?;
            }
        }
        ProviderKind::ExternalSystem => {
            require_path(&req.payload)?;
            require_field(&req.payload, "system", "plex")?;
            require_field(&req.payload, "action", "refresh")?;
        }
        ProviderKind::QualityScorer => {
            require_path(&req.payload)?;
            require_field(&req.payload, "profile", "default")?;
        }
        ProviderKind::IssueProvider => {
            require_path(&req.payload)?;
            require_field(&req.payload, "reason", "quality_regression")?;
        }
        ProviderKind::UseLeaseProvider => {
            require_path(&req.payload)?;
            require_field(&req.payload, "holder", "manual")?;
            require_field(&req.payload, "reason", "playback")?;
        }
    }
    Ok(())
}

pub(crate) fn string_field<'a>(
    payload: &'a serde_json::Value,
    field: &'static str,
) -> Result<&'a str, ProtocolError> {
    payload
        .as_object()
        .and_then(|object| object.get(field))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(format!("payload missing {field}")))
}

pub(crate) fn scenario(payload: &serde_json::Value) -> &str {
    payload
        .as_object()
        .and_then(|object| object.get("scenario"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("default")
}

pub(crate) fn remux_protocol_payload(
    payload: &serde_json::Value,
) -> Result<Option<RemuxRequest>, ProtocolError> {
    if !(payload.get("input").is_some() && payload.get("output").is_some()) {
        return Ok(None);
    }
    serde_json::from_value(payload.clone())
        .map(Some)
        .map_err(|err| invalid(format!("remux protocol payload invalid: {err}")))
}

pub(crate) fn transcode_video_protocol_payload(
    payload: &serde_json::Value,
) -> Result<Option<TranscodeVideoRequest>, ProtocolError> {
    if !(payload.get("input").is_some()
        && payload.get("output").is_some()
        && payload.get("profile").is_some())
    {
        return Ok(None);
    }
    serde_json::from_value(payload.clone())
        .map(Some)
        .map_err(|err| invalid(format!("transcode_video protocol payload invalid: {err}")))
}

pub(crate) fn transcode_audio_protocol_payload(
    payload: &serde_json::Value,
) -> Result<Option<TranscodeAudioRequest>, ProtocolError> {
    if !(payload.get("input").is_some()
        && payload.get("output").is_some()
        && payload.get("selection").is_some()
        && payload.get("audio").is_some())
    {
        return Ok(None);
    }
    serde_json::from_value(payload.clone())
        .map(Some)
        .map_err(|err| invalid(format!("transcode_audio protocol payload invalid: {err}")))
}

pub(crate) fn extract_audio_protocol_payload(
    payload: &serde_json::Value,
) -> Result<Option<ExtractAudioRequest>, ProtocolError> {
    if !(payload.get("input").is_some()
        && payload.get("output").is_some()
        && payload.get("selection").is_some())
    {
        return Ok(None);
    }
    serde_json::from_value(payload.clone())
        .map(Some)
        .map_err(|err| invalid(format!("extract_audio protocol payload invalid: {err}")))
}

pub(crate) fn optional_string_array_field<'a>(
    payload: &'a serde_json::Value,
    field: &'static str,
) -> Result<Option<Vec<&'a str>>, ProtocolError> {
    payload
        .as_object()
        .and_then(|object| object.get(field))
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| invalid(format!("{field} must be an array")))
                .and_then(|items| {
                    items
                        .iter()
                        .map(|item| {
                            item.as_str()
                                .ok_or_else(|| invalid(format!("{field} must contain strings")))
                        })
                        .collect()
                })
        })
        .transpose()
}

pub(crate) fn string_array_field<'a>(
    payload: &'a serde_json::Value,
    field: &'static str,
) -> Result<Vec<&'a str>, ProtocolError> {
    optional_string_array_field(payload, field)?
        .ok_or_else(|| invalid(format!("payload missing {field}")))
}

pub(crate) fn invalid(detail: impl Into<String>) -> ProtocolError {
    ProtocolError::InvalidPayload {
        detail: detail.into(),
    }
}

fn require_path(payload: &serde_json::Value) -> Result<&str, ProtocolError> {
    let path = string_field(payload, "path")?;
    if path.trim().is_empty() {
        return Err(invalid("path must not be empty"));
    }
    Ok(path)
}

fn require_field(
    payload: &serde_json::Value,
    field: &'static str,
    expected: &'static str,
) -> Result<(), ProtocolError> {
    let actual = string_field(payload, field)?;
    if actual == expected {
        Ok(())
    } else {
        Err(invalid(format!("{field} must be {expected}")))
    }
}

fn require_one_of(
    payload: &serde_json::Value,
    field: &'static str,
    expected: &[&'static str],
) -> Result<(), ProtocolError> {
    let actual = string_field(payload, field)?;
    if expected.contains(&actual) {
        Ok(())
    } else {
        Err(invalid(format!(
            "{field} must be one of {}",
            expected.join(", ")
        )))
    }
}

fn optional_u64(
    payload: &serde_json::Value,
    field: &'static str,
) -> Result<Option<u64>, ProtocolError> {
    match payload.as_object().and_then(|object| object.get(field)) {
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid(format!("{field} must be an unsigned integer"))),
        None => Ok(None),
    }
}

fn validate_transcode_video_request(request: &TranscodeVideoRequest) -> Result<(), ProtocolError> {
    if request.input.path.trim().is_empty() {
        return Err(invalid("transcode_video input.path must not be empty"));
    }
    if request.output.path.trim().is_empty() {
        return Err(invalid("transcode_video output.path must not be empty"));
    }
    if !is_supported_transcode_video_container(&request.output.container) {
        return Err(invalid(
            "transcode_video output.container must be mkv or mp4",
        ));
    }
    if !is_supported_transcode_video_codec(&request.output.video_codec) {
        return Err(invalid(
            "transcode_video output.video_codec must be hevc or av1",
        ));
    }
    if canonical_video_codec(&request.output.video_codec)
        != canonical_video_codec(&request.profile.target_codec)
    {
        return Err(invalid(
            "transcode_video output.video_codec must match profile.target_codec",
        ));
    }
    Ok(())
}

fn validate_transcode_audio_request(request: &TranscodeAudioRequest) -> Result<(), ProtocolError> {
    if request.input.path.trim().is_empty() {
        return Err(invalid("transcode_audio input.path must not be empty"));
    }
    if request.output.container != "mkv" {
        return Err(invalid("transcode_audio output.container must be mkv"));
    }
    if !matches!(request.audio.target_codec.as_str(), "aac" | "opus") {
        return Err(invalid(
            "transcode_audio audio.target_codec must be aac or opus",
        ));
    }
    if request.selection.selected_streams.is_empty() {
        return Err(invalid("transcode_audio selection must not be empty"));
    }
    Ok(())
}

fn validate_extract_audio_request(request: &ExtractAudioRequest) -> Result<(), ProtocolError> {
    if request.input.path.trim().is_empty() {
        return Err(invalid("extract_audio input.path must not be empty"));
    }
    if request.output.container != "ogg" || request.output.audio_codec != "opus" {
        return Err(invalid("extract_audio output must be opus in ogg"));
    }
    if request.selection.snapshot_stream_id.trim().is_empty() {
        return Err(invalid("extract_audio selection must not be empty"));
    }
    Ok(())
}
