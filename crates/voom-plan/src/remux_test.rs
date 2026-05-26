use serde_json::json;
use voom_policy::{
    ComparisonOp, MediaSnapshotInput, TargetKind, TargetRef, TrackFilter, TrackTarget,
};
use voom_worker_protocol::RemuxTrackGroup;

use super::{
    RemuxOperationPayload, RemuxPlanningBlock, SnapshotStreamFact, evaluate_filter, stream_facts,
};

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

fn assert_remux_payload_error(payload: &serde_json::Value, expected: &str) {
    let err = RemuxOperationPayload::try_from_execution_value(payload).unwrap_err();

    assert_eq!(err.to_string(), expected);
}

#[test]
fn remux_stream_facts_parse_normalized_streams() {
    let streams = json!([
        {
            "id": "stream-0",
            "index": 0,
            "kind": "video",
            "codec_name": "h264",
            "disposition": {
                "default": true
            }
        },
        {
            "id": "stream-1",
            "index": 1,
            "kind": "audio",
            "codec_name": "aac",
            "language": "eng",
            "channels": 6,
            "title": "Main",
            "disposition": {
                "forced": true
            }
        }
    ]);
    let snapshot = snapshot_with_streams(&streams);

    let facts = stream_facts(&snapshot).unwrap();

    assert_eq!(
        facts,
        vec![
            SnapshotStreamFact {
                snapshot_stream_id: "stream-0".to_owned(),
                provider_stream_index: 0,
                kind: TrackTarget::Video,
                codec_name: Some("h264".to_owned()),
                language: None,
                channels: None,
                title: None,
                mime_type: None,
                filename: None,
                is_default: true,
                is_forced: false,
            },
            SnapshotStreamFact {
                snapshot_stream_id: "stream-1".to_owned(),
                provider_stream_index: 1,
                kind: TrackTarget::Audio,
                codec_name: Some("aac".to_owned()),
                language: Some("eng".to_owned()),
                channels: Some(6),
                title: Some("Main".to_owned()),
                mime_type: None,
                filename: None,
                is_default: false,
                is_forced: true,
            },
        ]
    );
}

#[test]
fn remux_stream_facts_missing_stream_id_blocks_planning() {
    let streams = json!([
        {
            "index": 0,
            "kind": "audio"
        }
    ]);
    let snapshot = snapshot_with_streams(&streams);

    let err = stream_facts(&snapshot).unwrap_err();

    assert_eq!(err, RemuxPlanningBlock::InsufficientSnapshotFacts);
}

#[test]
fn remux_language_filter_missing_fact_blocks_planning() {
    let stream = SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Audio,
        codec_name: Some("aac".to_owned()),
        language: None,
        channels: Some(2),
        title: None,
        mime_type: None,
        filename: None,
        is_default: false,
        is_forced: false,
    };

    let err = evaluate_filter(
        &TrackFilter::LanguageIn {
            values: vec!["eng".to_owned()],
        },
        &stream,
    )
    .unwrap_err();

    assert_eq!(err, RemuxPlanningBlock::InsufficientSnapshotFacts);
}

#[test]
fn remux_or_returns_true_before_later_insufficient_child() {
    let stream = SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Audio,
        codec_name: Some("aac".to_owned()),
        language: None,
        channels: Some(2),
        title: None,
        mime_type: None,
        filename: None,
        is_default: false,
        is_forced: false,
    };

    let matched = evaluate_filter(
        &TrackFilter::Or {
            filters: vec![
                TrackFilter::CodecIn {
                    values: vec!["aac".to_owned()],
                },
                TrackFilter::LanguageIn {
                    values: vec!["eng".to_owned()],
                },
            ],
        },
        &stream,
    )
    .unwrap();

    assert!(matched);
}

#[test]
fn remux_and_evaluates_later_missing_facts_after_false_child() {
    let stream = SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Audio,
        codec_name: Some("aac".to_owned()),
        language: None,
        channels: Some(2),
        title: None,
        mime_type: None,
        filename: None,
        is_default: false,
        is_forced: false,
    };

    let err = evaluate_filter(
        &TrackFilter::And {
            filters: vec![
                TrackFilter::CodecIn {
                    values: vec!["flac".to_owned()],
                },
                TrackFilter::LanguageIn {
                    values: vec!["eng".to_owned()],
                },
            ],
        },
        &stream,
    )
    .unwrap_err();

    assert_eq!(err, RemuxPlanningBlock::InsufficientSnapshotFacts);
}

#[test]
fn remux_title_contains_is_case_sensitive() {
    let stream = SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Audio,
        codec_name: Some("aac".to_owned()),
        language: Some("eng".to_owned()),
        channels: Some(2),
        title: Some("Main Audio".to_owned()),
        mime_type: None,
        filename: None,
        is_default: false,
        is_forced: false,
    };

    let matched = evaluate_filter(
        &TrackFilter::TitleContains {
            value: "main".to_owned(),
        },
        &stream,
    )
    .unwrap();

    assert!(!matched);
}

#[test]
fn remux_channels_filter_uses_comparison_op() {
    let stream = SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Audio,
        codec_name: Some("aac".to_owned()),
        language: Some("eng".to_owned()),
        channels: Some(6),
        title: None,
        mime_type: None,
        filename: None,
        is_default: false,
        is_forced: false,
    };

    let matched = evaluate_filter(
        &TrackFilter::Channels {
            op: ComparisonOp::Gte,
            value: 6,
        },
        &stream,
    )
    .unwrap();

    assert!(matched);
}

#[test]
fn remux_font_filter_is_false_for_non_font_attachment() {
    let stream = SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Attachment,
        codec_name: None,
        language: None,
        channels: None,
        title: None,
        mime_type: None,
        filename: Some("cover.jpg".to_owned()),
        is_default: false,
        is_forced: false,
    };

    let matched = evaluate_filter(&TrackFilter::Font, &stream).unwrap();

    assert!(!matched);
}

fn snapshot_with_streams(streams: &serde_json::Value) -> MediaSnapshotInput {
    MediaSnapshotInput {
        ordinal: 1,
        target: TargetRef::Synthetic {
            key: "media".to_owned(),
            kind: TargetKind::FileVersion,
        },
        container: None,
        stream_summary: json!({ "streams": streams }),
        video_codec: None,
        width: None,
        height: None,
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: None,
    }
}
