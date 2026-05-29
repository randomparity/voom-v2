use super::*;

use serde_json::json;
use time::OffsetDateTime;
use voom_core::{FileVersionId, MediaSnapshotId};

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

    let input = planning_input(&snapshot);

    assert_eq!(input.stream_summary["video_stream_count"], 1);
    assert_eq!(input.stream_summary["streams"], snapshot.payload["streams"]);
    assert_eq!(input.container.as_deref(), Some("mkv"));
    assert_eq!(input.video_codec.as_deref(), Some("h264"));
    assert_eq!(input.existing_media_snapshot_id, Some(MediaSnapshotId(7)));
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

    let input = planning_input(&snapshot);

    assert_eq!(input.stream_summary["video_stream_count"], 0);
    assert_eq!(input.container, None);
    assert_eq!(input.video_codec, None);
}
