use super::*;

use serde_json::json;
use time::OffsetDateTime;
use voom_core::{FileVersionId, MediaSnapshotId};

fn snapshot_with_payload(payload: serde_json::Value) -> MediaSnapshot {
    MediaSnapshot {
        id: MediaSnapshotId(1),
        file_version_id: FileVersionId(1),
        probed_by: None,
        probed_at: OffsetDateTime::UNIX_EPOCH,
        payload,
    }
}

#[test]
fn planning_input_derives_video_count_and_copies_container_and_codec() {
    let snapshot = MediaSnapshot {
        id: MediaSnapshotId(7),
        file_version_id: FileVersionId(3),
        probed_by: None,
        probed_at: OffsetDateTime::UNIX_EPOCH,
        payload: json!({
            "container": "mkv",
            "video_codec": "h264",
            "streams": [
                {"id": "v-1", "index": 0, "kind": "video", "codec_name": "h264"},
                {"id": "a-1", "index": 1, "kind": "audio", "codec_name": "aac"},
            ],
        }),
    };

    let input = planning_input(1, &snapshot);

    assert_eq!(input.stream_summary["video_stream_count"], 1);
    assert_eq!(input.stream_summary["streams"], snapshot.payload["streams"]);
    assert_eq!(input.container.as_deref(), Some("mkv"));
    assert_eq!(input.video_codec.as_deref(), Some("h264"));
    assert_eq!(input.existing_media_snapshot_id, Some(MediaSnapshotId(7)));
    assert_eq!(input.width, None);
    assert_eq!(input.height, None);
}

#[test]
fn planning_input_defaults_video_count_zero_when_no_streams() {
    let snapshot = MediaSnapshot {
        id: MediaSnapshotId(1),
        file_version_id: FileVersionId(1),
        probed_by: None,
        probed_at: OffsetDateTime::UNIX_EPOCH,
        payload: json!({}),
    };

    let input = planning_input(1, &snapshot);

    assert_eq!(input.stream_summary["video_stream_count"], 0);
    assert!(input.stream_summary.get("streams").is_none());
    assert_eq!(input.container, None);
    assert_eq!(input.video_codec, None);
}

#[test]
fn stream_summary_preserves_unavailable_stream_inventory_shapes() {
    for streams in [serde_json::Value::Null, json!({"unexpected": "shape"})] {
        let summary = stream_summary_from_snapshot_payload(&json!({"streams": streams.clone()}));

        assert_eq!(summary["streams"], streams);
        assert_eq!(summary["video_stream_count"], 0);
    }
}

#[test]
fn planning_input_projects_video_dimensions() {
    let snapshot = snapshot_with_payload(serde_json::json!({
        "container": "matroska",
        "streams": [{
            "id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264",
            "width": 3840, "height": 2160, "pixel_format": "yuv420p"
        }]
    }));
    let input = planning_input(1, &snapshot);
    assert_eq!(input.width, Some(3840));
    assert_eq!(input.height, Some(2160));
}

#[test]
fn planning_input_projects_duration_and_bitrate_from_container_facts() {
    let snapshot = snapshot_with_payload(serde_json::json!({
        "container": {
            "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
            "duration_seconds": 2.1259,
            "bit_rate": 2_048_000
        },
        "streams": [{
            "id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264"
        }]
    }));

    let input = planning_input(1, &snapshot);

    assert_eq!(input.duration_millis, Some(2_125));
    assert_eq!(input.bitrate, Some(2_048_000));
}

#[test]
fn planning_input_omits_malformed_duration_and_bitrate_facts() {
    for container in [
        json!({"format_name": "matroska", "duration_seconds": -1.0, "bit_rate": "2M"}),
        json!({"format_name": "matroska", "duration_seconds": "2.0", "bit_rate": -1}),
        json!({"format_name": "matroska", "duration_seconds": 1.0e30, "bit_rate": 1.0e30}),
    ] {
        let input = planning_input(
            1,
            &snapshot_with_payload(json!({
                "container": container,
                "streams": []
            })),
        );

        assert_eq!(input.duration_millis, None);
        assert_eq!(input.bitrate, None);
    }
}

#[test]
fn planning_input_canonicalizes_supported_container_names() {
    let cases = [
        ("mkv", "mkv"),
        ("matroska", "mkv"),
        ("matroska,webm", "mkv"),
        ("mp4", "mp4"),
        ("mov,mp4", "mp4"),
        ("mov,mp4,m4a,3gp,3g2,mj2", "mp4"),
        ("ogg", "ogg"),
    ];

    for (durable, canonical) in cases {
        for container in [
            json!(durable),
            json!({"format_name": durable, "format_long_name": "inspection only"}),
        ] {
            let input = planning_input(1, &snapshot_with_payload(json!({"container": container})));

            assert_eq!(
                input.container.as_deref(),
                Some(canonical),
                "durable container {durable:?}"
            );
        }
    }
}

#[test]
fn planning_input_rejects_unknown_or_malformed_container_names() {
    let cases = [
        json!(null),
        json!(true),
        json!(42),
        json!([]),
        json!({}),
        json!({"format_long_name": "Matroska"}),
        json!({"format_name": null}),
        json!({"format_name": ["matroska,webm"]}),
        json!(""),
        json!(" matroska,webm"),
        json!("matroska,webm "),
        json!("MATROSKA,WEBM"),
        json!("webm"),
        json!("mov"),
        json!("m4a"),
        json!("webm,matroska"),
        json!("matroska,webm,webm"),
        json!("mov,mp4,m4a,3gp,3g2"),
        json!("mov,mp4,m4a,3gp,3g2,mj2,unknown"),
    ];

    assert_eq!(
        planning_input(1, &snapshot_with_payload(json!({}))).container,
        None
    );
    for container in cases {
        let input = planning_input(
            1,
            &snapshot_with_payload(json!({"container": container.clone()})),
        );

        assert_eq!(input.container, None, "durable container {container}");
    }
}
