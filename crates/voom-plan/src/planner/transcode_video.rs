use serde_json::json;
use voom_policy::MediaSnapshotInput;

use crate::{
    NodeStatus, PlanOperationKind, PlanningDiagnostic, PlanningDiagnosticCode, ResourceEstimates,
};

use super::{OperationPlan, PlanGenerationError, serialization_error, video_stream_count};

#[derive(Debug, Clone, PartialEq, Eq)]
enum TranscodeVideoShape {
    Compliant,
    NeedsTranscode,
    InsufficientFacts(String),
    UnsupportedShape(String),
}

pub(super) fn plan(
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
    resolved: &voom_core::TranscodeVideoProfile,
    container: &str,
) -> Result<OperationPlan, PlanGenerationError> {
    let target_codec = &resolved.target_codec;
    let payload = transcode_video_payload(resolved, container)
        .map_err(|error| serialization_error(&error))?;
    let observed_state = transcode_video_observed_state(snapshot);
    let mut notes = Vec::new();
    let (status, status_reason, capability, diagnostic) =
        match transcode_video_shape(snapshot, resolved, container) {
            TranscodeVideoShape::Compliant => (
                NodeStatus::NoOp,
                format!("video is already {target_codec} in {container}"),
                None,
                None,
            ),
            TranscodeVideoShape::NeedsTranscode => {
                notes = transcode_video_notes(resolved, snapshot);
                (
                    NodeStatus::Planned,
                    format!("video will be transcoded to {target_codec} in {container}"),
                    Some("transcode_video".to_owned()),
                    None,
                )
            }
            TranscodeVideoShape::InsufficientFacts(message) => (
                NodeStatus::Blocked,
                message,
                None,
                Some(PlanningDiagnosticCode::InsufficientSnapshotFacts),
            ),
            TranscodeVideoShape::UnsupportedShape(message) => (
                NodeStatus::Blocked,
                message,
                None,
                Some(PlanningDiagnosticCode::UnsupportedMediaShape),
            ),
        };

    let plan = OperationPlan::new(
        PlanOperationKind::TranscodeVideo,
        payload,
        observed_state,
        status,
        status_reason,
        capability,
    )
    .with_resource_estimates(ResourceEstimates { notes });
    Ok(with_optional_diagnostic(
        plan, diagnostic, phase_name, snapshot,
    ))
}

pub(super) fn missing_resolution_diagnostic(
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
) -> PlanningDiagnostic {
    let message = "transcode_video profile was not resolved before planning";
    PlanningDiagnostic::error(PlanningDiagnosticCode::InvalidPlanningRequest, message)
        .with_phase(phase_name)
        .with_operation_kind(PlanOperationKind::TranscodeVideo.as_str())
        .with_target(snapshot.target.clone())
}

fn with_optional_diagnostic(
    plan: OperationPlan,
    code: Option<PlanningDiagnosticCode>,
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
) -> OperationPlan {
    let Some(code) = code else {
        return plan;
    };
    let message = plan.status_reason.clone();
    plan.with_diagnostic(
        PlanningDiagnostic::error(code, &message)
            .with_phase(phase_name)
            .with_operation_kind(PlanOperationKind::TranscodeVideo.as_str())
            .with_target(snapshot.target.clone()),
    )
}

fn transcode_video_shape(
    snapshot: &MediaSnapshotInput,
    resolved: &voom_core::TranscodeVideoProfile,
    target_container: &str,
) -> TranscodeVideoShape {
    let Some(video_stream_count) = video_stream_count(snapshot) else {
        return TranscodeVideoShape::InsufficientFacts(
            "snapshot video stream count is unknown".to_owned(),
        );
    };
    if video_stream_count != 1 {
        return TranscodeVideoShape::UnsupportedShape(
            "transcode_video requires exactly one video stream".to_owned(),
        );
    }

    let Some(container) = snapshot.container.as_deref() else {
        return TranscodeVideoShape::InsufficientFacts("snapshot container is unknown".to_owned());
    };
    let Some(video_codec) = snapshot.video_codec.as_deref() else {
        return TranscodeVideoShape::InsufficientFacts(
            "snapshot video codec is unknown".to_owned(),
        );
    };

    let needs_change = match transcode_video_needs_change(
        snapshot,
        resolved,
        container,
        video_codec,
        target_container,
    ) {
        Ok(needs_change) => needs_change,
        Err(shape) => return shape,
    };

    if target_container.eq_ignore_ascii_case(voom_core::TRANSCODE_VIDEO_CONTAINER_MP4)
        && let Some(shape) = mp4_gate_shape(snapshot)
    {
        return shape;
    }

    if needs_change {
        TranscodeVideoShape::NeedsTranscode
    } else {
        TranscodeVideoShape::Compliant
    }
}

fn transcode_video_needs_change(
    snapshot: &MediaSnapshotInput,
    resolved: &voom_core::TranscodeVideoProfile,
    container: &str,
    video_codec: &str,
    target_container: &str,
) -> Result<bool, TranscodeVideoShape> {
    let mut needs_change = false;

    let codec_matches = voom_core::canonical_video_codec(video_codec)
        .is_some_and(|canonical| canonical.eq_ignore_ascii_case(&resolved.target_codec));
    if !codec_matches {
        needs_change = true;
    }
    if !container.eq_ignore_ascii_case(target_container) {
        needs_change = true;
    }

    needs_change |= dimensions_need_change(snapshot, resolved)?;
    needs_change |= pixel_format_needs_change(snapshot, resolved)?;
    needs_change |= codec_profile_needs_change(snapshot, resolved)?;
    needs_change |= codec_level_needs_change(snapshot, resolved)?;

    Ok(needs_change)
}

fn dimensions_need_change(
    snapshot: &MediaSnapshotInput,
    resolved: &voom_core::TranscodeVideoProfile,
) -> Result<bool, TranscodeVideoShape> {
    let mut needs_change = false;
    if let Some(cap_w) = resolved.max_width {
        let Some(width) = snapshot.width else {
            return Err(TranscodeVideoShape::InsufficientFacts(
                "snapshot video width is unknown".to_owned(),
            ));
        };
        needs_change |= width > cap_w;
    }
    if let Some(cap_h) = resolved.max_height {
        let Some(height) = snapshot.height else {
            return Err(TranscodeVideoShape::InsufficientFacts(
                "snapshot video height is unknown".to_owned(),
            ));
        };
        needs_change |= height > cap_h;
    }
    Ok(needs_change)
}

fn pixel_format_needs_change(
    snapshot: &MediaSnapshotInput,
    resolved: &voom_core::TranscodeVideoProfile,
) -> Result<bool, TranscodeVideoShape> {
    let Some(target) = resolved.pixel_format.as_deref() else {
        return Ok(false);
    };
    let Some(observed) = video_stream_field(snapshot, "pixel_format") else {
        return Err(TranscodeVideoShape::InsufficientFacts(
            "snapshot video pixel_format is unknown".to_owned(),
        ));
    };
    Ok(!observed.eq_ignore_ascii_case(target))
}

fn codec_profile_needs_change(
    snapshot: &MediaSnapshotInput,
    resolved: &voom_core::TranscodeVideoProfile,
) -> Result<bool, TranscodeVideoShape> {
    let Some(target) = resolved.codec_profile.as_deref() else {
        return Ok(false);
    };
    let Some(observed) = video_stream_field(snapshot, "profile") else {
        return Err(TranscodeVideoShape::InsufficientFacts(
            "snapshot video codec profile is unknown".to_owned(),
        ));
    };
    Ok(voom_core::normalize_codec_token(observed) != voom_core::normalize_codec_token(target))
}

fn codec_level_needs_change(
    snapshot: &MediaSnapshotInput,
    resolved: &voom_core::TranscodeVideoProfile,
) -> Result<bool, TranscodeVideoShape> {
    let Some(target) = resolved.codec_level.as_deref() else {
        return Ok(false);
    };
    let Some(observed) = video_stream_field(snapshot, "level") else {
        return Err(TranscodeVideoShape::InsufficientFacts(
            "snapshot video codec level is unknown".to_owned(),
        ));
    };
    Ok(voom_core::normalize_codec_token(observed) != voom_core::normalize_codec_token(target))
}

fn mp4_gate_shape(snapshot: &MediaSnapshotInput) -> Option<TranscodeVideoShape> {
    let Some(streams) = snapshot
        .stream_summary
        .get("streams")
        .and_then(serde_json::Value::as_array)
    else {
        return Some(TranscodeVideoShape::InsufficientFacts(
            "mp4 target requires fully enumerated streams".to_owned(),
        ));
    };
    let mut offenders = Vec::new();
    for stream in streams {
        let Some(object) = stream.as_object() else {
            return Some(TranscodeVideoShape::InsufficientFacts(
                "mp4 target requires fully enumerated streams".to_owned(),
            ));
        };
        let kind = object.get("kind").and_then(serde_json::Value::as_str);
        if kind == Some("video") {
            continue;
        }
        let codec_name = object.get("codec_name").and_then(serde_json::Value::as_str);
        let (Some(kind), Some(codec_name)) = (kind, codec_name) else {
            return Some(TranscodeVideoShape::InsufficientFacts(
                "mp4 target requires fully enumerated streams".to_owned(),
            ));
        };
        if !mp4_muxable(kind, codec_name) {
            offenders.push(format!("{kind}:{codec_name}"));
        }
    }
    if offenders.is_empty() {
        None
    } else {
        Some(TranscodeVideoShape::UnsupportedShape(format!(
            "mp4 target cannot mux stream(s) {}",
            offenders.join(", ")
        )))
    }
}

fn mp4_muxable(kind: &str, codec_name: &str) -> bool {
    match kind {
        "audio" => matches!(codec_name, "aac" | "ac3" | "eac3" | "opus"),
        _ => false,
    }
}

/// Returns the value of a field from the first video stream in the snapshot's
/// `stream_summary.streams` array.
#[must_use]
pub fn video_stream_field<'a>(snapshot: &'a MediaSnapshotInput, key: &str) -> Option<&'a str> {
    snapshot
        .stream_summary
        .get("streams")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .find(|stream| stream.get("kind").and_then(serde_json::Value::as_str) == Some("video"))
        .and_then(|stream| stream.get(key))
        .and_then(serde_json::Value::as_str)
}

fn transcode_video_payload(
    resolved: &voom_core::TranscodeVideoProfile,
    container: &str,
) -> Result<serde_json::Value, serde_json::Error> {
    Ok(json!({
        "type": "transcode_video",
        "target_codec": resolved.target_codec,
        "container": container,
        "profile": resolved.name,
        "resolved_profile": serde_json::to_value(resolved)?,
    }))
}

fn transcode_video_notes(
    resolved: &voom_core::TranscodeVideoProfile,
    snapshot: &MediaSnapshotInput,
) -> Vec<String> {
    let mut notes = vec![
        format!("encoder={}", resolved.encoder),
        format!("speed={}", resolved.preset),
        format!(
            "cpu_cost={}",
            crate::transcode_video_profile::cpu_cost(&resolved.encoder, &resolved.preset)
        ),
        format!("crf={}", resolved.crf),
    ];
    if let (Some(src_w), Some(src_h)) = (snapshot.width, snapshot.height) {
        let cap_w = resolved.max_width.unwrap_or(src_w);
        let cap_h = resolved.max_height.unwrap_or(src_h);
        if src_w > cap_w || src_h > cap_h {
            notes.push(format!("downscale={src_w}x{src_h}->{cap_w}x{cap_h}"));
        }
    }
    notes
}

fn transcode_video_observed_state(snapshot: &MediaSnapshotInput) -> Option<serde_json::Value> {
    let mut observed = serde_json::Map::new();
    if let Some(container) = &snapshot.container {
        observed.insert("container".to_owned(), json!(container));
    }
    if let Some(video_codec) = &snapshot.video_codec {
        observed.insert("video_codec".to_owned(), json!(video_codec));
    }
    if let Some(video_stream_count) = video_stream_count(snapshot) {
        observed.insert("video_stream_count".to_owned(), json!(video_stream_count));
    }
    if observed.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(observed))
    }
}
