use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::time::Duration;
use voom_core::VoomError;
use voom_events::payload::MediaSnapshotRecordedPayload;
use voom_events::{Event, SubjectType};
use voom_policy::{MediaSnapshotInput, TargetRef};
use voom_store::repo::identity::{IdentityRepo, MediaSnapshot, NewMediaSnapshot};

use crate::ControlPlane;
use crate::cases::append_event;

pub(crate) async fn record_with_event_in_tx(
    control_plane: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    input: NewMediaSnapshot,
) -> Result<MediaSnapshot, VoomError> {
    let snapshot = control_plane
        .identity
        .record_media_snapshot_in_tx(tx, input)
        .await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::MediaSnapshot,
        Some(snapshot.id.0),
        snapshot.probed_at,
        Event::MediaSnapshotRecorded(MediaSnapshotRecordedPayload {
            media_snapshot_id: snapshot.id.0,
            file_version_id: snapshot.file_version_id.0,
            probed_by_worker_id: snapshot.probed_by.map(|worker| worker.0),
            probed_at: snapshot.probed_at,
        }),
    )
    .await?;
    Ok(snapshot)
}

/// Convert a durable [`MediaSnapshot`] row into the planning-layer
/// [`MediaSnapshotInput`] shared by the audio and remux runtime selection paths.
///
/// Derives stream facts, canonicalizes supported container names, projects the
/// video codec and dimensions, and leaves unsupported optional facts at their
/// defaults.
pub(crate) fn planning_input(ordinal: u32, snapshot: &MediaSnapshot) -> MediaSnapshotInput {
    let payload = &snapshot.payload;
    let video_stream = payload
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| {
            streams
                .iter()
                .find(|stream| stream.get("kind").and_then(Value::as_str) == Some("video"))
        });
    let container = canonical_container(payload);
    let container_facts = payload.get("container").and_then(Value::as_object);
    let video_codec = video_stream
        .and_then(|stream| payload_str(stream, "codec_name"))
        .or_else(|| payload_str(payload, "video_codec"));

    MediaSnapshotInput {
        ordinal,
        target: TargetRef::FileVersion {
            id: snapshot.file_version_id,
        },
        container,
        stream_summary: stream_summary_from_snapshot_payload(payload),
        video_codec,
        width: video_stream.and_then(|stream| payload_u32(stream, "width")),
        height: video_stream.and_then(|stream| payload_u32(stream, "height")),
        hdr: None,
        bitrate: container_facts
            .and_then(|facts| facts.get("bit_rate"))
            .and_then(Value::as_u64),
        duration_millis: container_facts
            .and_then(|facts| facts.get("duration_seconds"))
            .and_then(Value::as_f64)
            .and_then(duration_millis),
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: Some(snapshot.id),
    }
}

fn duration_millis(seconds: f64) -> Option<u64> {
    let duration = Duration::try_from_secs_f64(seconds).ok()?;
    u64::try_from(duration.as_millis()).ok()
}

fn canonical_container(payload: &Value) -> Option<String> {
    let container = payload.get("container")?;
    let container = container
        .as_str()
        .or_else(|| container.get("format_name").and_then(Value::as_str))?;
    let canonical = if container == "mkv" || container == "matroska" || container == "matroska,webm"
    {
        "mkv"
    } else if container == "mp4" || container == "mov,mp4" || container == "mov,mp4,m4a,3gp,3g2,mj2"
    {
        "mp4"
    } else if container == "ogg" {
        "ogg"
    } else {
        return None;
    };
    Some(canonical.to_owned())
}

fn payload_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn payload_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

pub(crate) fn stream_summary_from_snapshot_payload(payload: &Value) -> Value {
    let streams = payload.get("streams");
    let video_stream_count = streams.and_then(Value::as_array).map_or(0, |streams| {
        streams
            .iter()
            .filter(|stream| stream.get("kind").and_then(Value::as_str) == Some("video"))
            .count()
    });
    let mut summary = json!({"video_stream_count": video_stream_count});
    if let Some(streams) = streams {
        summary["streams"] = streams.clone();
    }
    summary
}

#[cfg(test)]
#[path = "media_snapshot_test.rs"]
mod tests;
