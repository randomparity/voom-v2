use super::*;

#[test]
fn remux_request_serializes_wire_shape() {
    let request = RemuxRequest {
        input: RemuxInput {
            path: "/library/input.mp4".to_owned(),
            expected: RemuxExpectedFacts {
                size_bytes: 1234,
                content_hash: "blake3:abc".to_owned(),
                modified_at: Some("2026-05-25T00:00:00Z".to_owned()),
                local_file_key: None,
            },
        },
        output: RemuxOutput {
            staging_root: "/tmp/voom-stage".to_owned(),
            path: "/tmp/voom-stage/ticket-1/lease-1/input.remux.mkv".to_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: RemuxSelection {
            keep_streams: vec![RemuxStreamRef {
                snapshot_stream_id: "stream-0".to_owned(),
                provider_stream_index: 0,
            }],
            default_streams: vec![RemuxStreamRef {
                snapshot_stream_id: "stream-1".to_owned(),
                provider_stream_index: 1,
            }],
            clear_default_streams: vec![RemuxStreamRef {
                snapshot_stream_id: "stream-2".to_owned(),
                provider_stream_index: 2,
            }],
            track_order: vec![
                RemuxTrackGroup::Video,
                RemuxTrackGroup::Audio,
                RemuxTrackGroup::Subtitle,
            ],
        },
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(
        json["selection"]["track_order"],
        serde_json::json!(["video", "audio", "subtitle"])
    );
    assert_eq!(json["output"]["overwrite"], false);
}

#[test]
fn remux_result_rejects_unknown_fields() {
    let err = serde_json::from_value::<RemuxResult>(serde_json::json!({
        "status": "remuxed",
        "provider": "mkvtoolnix",
        "provider_version": "mkvmerge v80",
        "input_pre": { "size_bytes": 1, "content_hash": "blake3:a" },
        "input_post": { "size_bytes": 1, "content_hash": "blake3:a" },
        "output": { "size_bytes": 2, "content_hash": "blake3:b" },
        "output_container": "mkv",
        "kept_snapshot_stream_ids": ["stream-0"],
        "default_snapshot_stream_ids": [],
        "extra": true
    }))
    .unwrap_err();
    assert!(err.to_string().contains("unknown field"));
}
