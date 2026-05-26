use super::*;

use serde_json::json;
use time::OffsetDateTime;
use voom_core::{ErrorCode, FileVersionId, MediaSnapshotId};
use voom_store::repo::identity::MediaSnapshot;

#[test]
fn selection_preserves_video_and_applies_audio_keep() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "track_actions": [
            {
                "type": "keep_tracks",
                "target": "audio",
                "filter": {
                    "type": "language_in",
                    "values": ["eng"]
                }
            }
        ],
        "track_order": ["video", "audio", "subtitle"],
        "defaults": [
            {
                "target": "audio",
                "strategy": "first"
            }
        ]
    });
    let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(
        selection
            .keep_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<Vec<_>>(),
        vec!["stream-0", "stream-1"]
    );
    assert_eq!(selection.default_streams[0].snapshot_stream_id, "stream-1");
}

#[test]
fn selection_rejects_keep_remove_video_policy() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "track_actions": [
            {
                "type": "remove_tracks",
                "target": "video",
                "filter": null
            }
        ],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let snapshot = snapshot_with_video_audio_languages(["eng"]);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("video track policy is unsupported")
    );
}

fn snapshot_with_video_audio_languages<const N: usize>(languages: [&str; N]) -> MediaSnapshot {
    let audio_streams = languages
        .iter()
        .enumerate()
        .map(|(offset, language)| {
            let index = offset + 1;
            json!({
                "id": format!("stream-{index}"),
                "index": index,
                "kind": "audio",
                "codec_name": "aac",
                "language": language,
                "channels": 2,
                "disposition": {
                    "default": false
                }
            })
        })
        .collect::<Vec<_>>();
    let mut streams = vec![json!({
        "id": "stream-0",
        "index": 0,
        "kind": "video",
        "codec_name": "h264",
        "disposition": {
            "default": true
        }
    })];
    streams.extend(audio_streams);

    MediaSnapshot {
        id: MediaSnapshotId(1),
        file_version_id: FileVersionId(1),
        probed_by: None,
        probed_at: OffsetDateTime::UNIX_EPOCH,
        payload: json!({ "streams": streams }),
    }
}
