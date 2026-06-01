use serde_json::json;
use voom_core::RemuxTrackGroup;

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
fn remux_payload_allows_missing_snapshot_id_for_planner_serialization() {
    let payload_json = json!({
        "type": "remux",
        "container": "mkv"
    });
    let payload = RemuxOperationPayload::try_from_value(&payload_json).unwrap();

    assert_eq!(payload.source_media_snapshot_id, None);
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
            "track_actions": [{"type": "keep_tracks", "target": "attachment"}]
        }),
        "remux track_actions[0] target `attachment` is unsupported",
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
