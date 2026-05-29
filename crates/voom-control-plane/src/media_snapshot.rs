use serde_json::{Value, json};
use voom_policy::{MediaSnapshotInput, TargetRef};
use voom_store::repo::identity::MediaSnapshot;

/// Convert a durable [`MediaSnapshot`] row into the planning-layer
/// [`MediaSnapshotInput`] shared by the audio and remux runtime selection paths.
///
/// Derives `video_stream_count` from the payload's `streams`, copies the
/// container and video codec, and leaves the remaining optional fact fields at
/// their defaults (selection only consults stream/video facts).
pub(crate) fn planning_input(snapshot: &MediaSnapshot) -> MediaSnapshotInput {
    let streams = snapshot
        .payload
        .get("streams")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let video_stream_count = streams.as_array().map_or(0, |streams| {
        streams
            .iter()
            .filter(|stream| stream.get("kind").and_then(Value::as_str) == Some("video"))
            .count()
    });
    let video_stream = streams.as_array().and_then(|arr| {
        arr.iter()
            .find(|s| s.get("kind").and_then(Value::as_str) == Some("video"))
    });
    let dimension = |key: &str| {
        video_stream
            .and_then(|s| s.get(key))
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
    };

    MediaSnapshotInput {
        ordinal: 1,
        target: TargetRef::FileVersion {
            id: snapshot.file_version_id,
        },
        container: snapshot
            .payload
            .get("container")
            .and_then(Value::as_str)
            .map(str::to_owned),
        stream_summary: json!({
            "video_stream_count": video_stream_count,
            "streams": streams,
        }),
        video_codec: snapshot
            .payload
            .get("video_codec")
            .and_then(Value::as_str)
            .map(str::to_owned),
        width: dimension("width"),
        height: dimension("height"),
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: Some(snapshot.id),
    }
}

#[cfg(test)]
#[path = "media_snapshot_test.rs"]
mod tests;
