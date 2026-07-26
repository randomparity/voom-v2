use serde_json::json;
use voom_core::RemuxTrackGroup;
use voom_policy::{TrackFilter, TrackTarget};

use super::RemuxOperationPayload;

#[test]
fn remux_payload_defaults_optional_collections() {
    let payload_json = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 99
    });
    let payload = RemuxOperationPayload::try_from_execution_value(&payload_json).unwrap();

    assert!(payload.track_actions.is_empty());
    assert!(payload.defaults.is_empty());
    assert_eq!(payload.head_snapshot_stream_id, None);
    assert_eq!(
        payload.track_order,
        vec![
            RemuxTrackGroup::Video,
            RemuxTrackGroup::Audio,
            RemuxTrackGroup::Subtitle,
        ]
    );
}

#[test]
fn remux_payload_round_trips_resolved_filter_selections() {
    let payload_json = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 99,
        "track_actions": [],
        "track_order": [],
        "head_snapshot_stream_id": "audio-main",
        "defaults": [{
            "target": "audio",
            "strategy": "preserve",
            "selected_snapshot_stream_id": "audio-main"
        }]
    });

    let payload = RemuxOperationPayload::try_from_execution_value(&payload_json).unwrap();

    assert_eq!(
        payload.head_snapshot_stream_id.as_deref(),
        Some("audio-main")
    );
    assert_eq!(
        payload.defaults[0].selected_snapshot_stream_id.as_deref(),
        Some("audio-main")
    );
    assert_eq!(payload.into_value(), payload_json);
}

#[test]
fn remux_payload_allows_missing_snapshot_id_for_planner_serialization() {
    let payload_json = json!({
        "type": "remux",
        "container": "mkv"
    });
    let payload = RemuxOperationPayload::try_from_value(&payload_json).unwrap();

    assert_eq!(payload.source_media_snapshot_id, None);
}

#[test]
fn remux_payload_accepts_attachment_actions_with_exact_filters() {
    let payload_json = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 99,
        "track_actions": [
            {
                "type": "keep_tracks",
                "target": "attachment",
                "filter": {"type": "font"}
            }
        ]
    });

    let payload = RemuxOperationPayload::try_from_execution_value(&payload_json).unwrap();

    assert_eq!(payload.track_actions[0].target, TrackTarget::Attachment);
    assert_eq!(payload.track_actions[0].filter, Some(TrackFilter::Font));
}

#[test]
fn remux_payload_rejects_invalid_contract_fields() {
    assert_remux_payload_error(
        &json!({
            "type": "copy",
            "container": "mkv",
            "source_media_snapshot_id": 99
        }),
        "remux payload missing `type: remux`",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mp4",
            "source_media_snapshot_id": 99
        }),
        "remux payload `container` must be mkv",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv"
        }),
        "remux payload `source_media_snapshot_id` must be a positive integer",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 0
        }),
        "remux payload `source_media_snapshot_id` must be a positive integer",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "track_actions": [{"type": "copy_tracks", "target": "audio"}]
        }),
        "remux track_actions[0] type `copy_tracks` is unsupported",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "track_order": []
        }),
        "remux track_order must include at least one group",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "track_order": ["video", "audio", "audio"]
        }),
        "remux track_order[2] duplicates target `audio`",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "head_snapshot_stream_id": ""
        }),
        "remux payload `head_snapshot_stream_id` must be a non-empty string",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "head_snapshot_stream_id": 1
        }),
        "remux payload `head_snapshot_stream_id` must be a non-empty string",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "defaults": [{
                "target": "audio",
                "strategy": "preserve",
                "selected_snapshot_stream_id": " "
            }]
        }),
        "remux defaults[0] `selected_snapshot_stream_id` must be a non-empty string",
    );
}

#[test]
fn remux_payload_distinguishes_missing_and_invalid_enum_fields() {
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "track_actions": [{"type": "keep_tracks"}]
        }),
        "remux track_actions[0] missing `target`",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "track_actions": [{"type": "keep_tracks", "target": "commentary"}]
        }),
        "remux track_actions[0] invalid `target`: unknown variant `commentary`, expected one of \
         `video`, `audio`, `subtitle`, `attachment`",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "defaults": [{"target": "audio"}]
        }),
        "remux defaults[0] missing `strategy`",
    );
    assert_remux_payload_error(
        &json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "defaults": [{"target": "audio", "strategy": "middle"}]
        }),
        "remux defaults[0] invalid `strategy`: unknown variant `middle`, expected one of `first`, \
         `best`, `none`, `preserve`",
    );
}

fn assert_remux_payload_error(payload: &serde_json::Value, expected: &str) {
    let err = RemuxOperationPayload::try_from_execution_value(payload).unwrap_err();

    assert_eq!(err.to_string(), expected);
}
