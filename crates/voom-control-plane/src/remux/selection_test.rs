use super::*;

use std::collections::BTreeSet;

use serde_json::json;
use time::OffsetDateTime;
use voom_core::{ErrorCode, FileVersionId, MediaSnapshotId};
use voom_store::repo::identity::MediaSnapshot;

#[test]
fn selection_preserves_video_and_applies_audio_keep() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
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
    assert_eq!(
        selection
            .default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["stream-0", "stream-1"]),
        "explicit audio default and preserved source-default video"
    );
}

#[test]
fn selection_preserves_source_default_video_without_defaults_action() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let snapshot = snapshot_with_video_audio_languages(["eng"]);

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(
        selection
            .default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<Vec<_>>(),
        vec!["stream-0"]
    );
    assert!(selection.clear_default_streams.is_empty());
}

#[test]
fn selection_preserves_source_default_video_with_explicit_preserve_action() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": [
            {
                "target": "video",
                "strategy": "preserve"
            }
        ]
    });
    let mut snapshot = snapshot_with_video_audio_languages(["eng"]);
    snapshot.payload["container"] = json!("mp4");

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(
        selection
            .default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<Vec<_>>(),
        vec!["stream-0"]
    );
    assert!(selection.clear_default_streams.is_empty());
}

#[test]
fn selection_executes_resolved_default_and_clears_other_target_streams() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": [{
            "target": "audio",
            "strategy": "preserve",
            "selected_snapshot_stream_id": "stream-2"
        }]
    });
    let mut snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);
    snapshot.payload["streams"][1]["disposition"]["default"] = json!(true);

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(
        selection
            .default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["stream-0", "stream-2"])
    );
    assert_eq!(
        selection
            .clear_default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<Vec<_>>(),
        ["stream-1"]
    );
}

#[test]
fn selection_executes_resolved_best_without_ranking_snapshot_languages() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": [{
            "target": "audio",
            "strategy": "best",
            "selected_snapshot_stream_id": "stream-2"
        }]
    });
    let mut snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);
    snapshot.payload["streams"][1]["language"] = json!(17);
    snapshot.payload["streams"][2]
        .as_object_mut()
        .unwrap()
        .remove("language");

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert!(
        selection
            .default_streams
            .iter()
            .any(|stream| stream.snapshot_stream_id == "stream-2")
    );
    assert!(
        selection
            .clear_default_streams
            .iter()
            .any(|stream| stream.snapshot_stream_id == "stream-1")
    );
}

#[test]
fn selection_rejects_unresolved_best() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": [{"target": "audio", "strategy": "best"}]
    });
    let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("best requires a resolved selected_snapshot_stream_id"),
        "{err}"
    );
}

#[test]
fn selection_rejects_selected_id_on_unresolved_strategies() {
    for strategy in ["first", "none"] {
        let payload = json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 1,
            "track_actions": [],
            "track_order": ["video", "audio"],
            "defaults": [{
                "target": "audio",
                "strategy": strategy,
                "selected_snapshot_stream_id": "stream-1"
            }]
        });
        let snapshot = snapshot_with_video_audio_languages(["eng"]);

        let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

        assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
        assert!(
            err.to_string().contains(&format!(
                "selected_snapshot_stream_id is invalid with default strategy {strategy}"
            )),
            "{err}"
        );
    }
}

#[test]
fn selection_executes_resolved_head_with_empty_group_order() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": [],
        "head_snapshot_stream_id": "stream-2",
        "defaults": []
    });
    let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert!(selection.track_order.is_empty());
    assert_eq!(
        selection
            .head_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<Vec<_>>(),
        ["stream-2"]
    );
}

#[test]
fn selection_rejects_invalid_resolved_default_identities() {
    for (selected_id, expected) in [
        ("missing", "is missing from the pinned snapshot"),
        ("stream-0", "is not an expected audio stream"),
    ] {
        let payload = json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 1,
            "track_actions": [],
            "track_order": ["video", "audio"],
            "defaults": [{
                "target": "audio",
                "strategy": "preserve",
                "selected_snapshot_stream_id": selected_id
            }]
        });
        let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

        let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

        assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
        assert!(err.to_string().contains(expected), "{err}");
    }
}

#[test]
fn selection_rejects_resolved_default_removed_by_track_actions() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [{
            "type": "remove_tracks",
            "target": "audio",
            "filter": {"type": "language_in", "values": ["spa"]}
        }],
        "track_order": ["video", "audio"],
        "defaults": [{
            "target": "audio",
            "strategy": "preserve",
            "selected_snapshot_stream_id": "stream-2"
        }]
    });
    let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("did not survive"), "{err}");
}

#[test]
fn selection_rejects_invalid_resolved_head_identities() {
    let mut snapshot = snapshot_with_video_audio_languages(["eng"]);
    snapshot.payload["streams"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "attachment",
            "index": 2,
            "kind": "attachment",
            "codec_name": "ttf",
            "filename": "font.ttf",
            "mime_type": "font/ttf"
        }));
    for (head_id, expected) in [
        ("missing", "is missing from the pinned snapshot"),
        ("attachment", "cannot be an attachment"),
    ] {
        let payload = json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 1,
            "track_actions": [],
            "track_order": [],
            "head_snapshot_stream_id": head_id,
            "defaults": []
        });

        let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

        assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
        assert!(err.to_string().contains(expected), "{err}");
    }
}

#[test]
fn selection_explicit_default_overrides_strategy_in_both_payload_orders() {
    let explicit = json!({
        "target": "audio",
        "strategy": "preserve",
        "selected_snapshot_stream_id": "stream-2"
    });
    for strategy in [
        json!({"target": "audio", "strategy": "none"}),
        json!({
            "target": "audio",
            "strategy": "best",
            "selected_snapshot_stream_id": "stream-1"
        }),
    ] {
        for defaults in [
            vec![explicit.clone(), strategy.clone()],
            vec![strategy.clone(), explicit.clone()],
        ] {
            let payload = json!({
                "type": "remux",
                "container": "mkv",
                "source_media_snapshot_id": 1,
                "track_actions": [],
                "track_order": ["video", "audio"],
                "defaults": defaults
            });
            let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

            let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

            assert!(
                selection
                    .default_streams
                    .iter()
                    .any(|stream| stream.snapshot_stream_id == "stream-2")
            );
            assert!(
                selection
                    .clear_default_streams
                    .iter()
                    .any(|stream| stream.snapshot_stream_id == "stream-1")
            );
        }
    }
}

#[test]
fn selection_rejects_best_combined_with_another_same_target_strategy() {
    let best = json!({
        "target": "audio",
        "strategy": "best",
        "selected_snapshot_stream_id": "stream-2"
    });
    let none = json!({"target": "audio", "strategy": "none"});
    for defaults in [
        vec![best.clone(), none.clone()],
        vec![none, best.clone()],
        vec![best.clone(), best],
    ] {
        let payload = json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 1,
            "track_actions": [],
            "track_order": ["video", "audio"],
            "defaults": defaults
        });
        let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

        let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

        assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
        assert!(
            err.to_string()
                .contains("multiple defaults strategy actions target audio and include best"),
            "{err}"
        );
    }
}

#[test]
fn selection_preserves_legacy_non_best_strategy_composition() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": [
            {"target": "audio", "strategy": "none"},
            {"target": "audio", "strategy": "preserve"}
        ]
    });
    let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(
        selection
            .clear_default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<Vec<_>>(),
        ["stream-1", "stream-2"]
    );
}

#[test]
fn selection_rejects_multiple_explicit_defaults_for_one_target() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": [
            {
                "target": "audio",
                "strategy": "preserve",
                "selected_snapshot_stream_id": "stream-1"
            },
            {
                "target": "audio",
                "strategy": "preserve",
                "selected_snapshot_stream_id": "stream-2"
            }
        ]
    });
    let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("multiple explicit defaults actions target audio"),
        "{err}"
    );
}

#[test]
fn selection_sorts_retained_streams_by_provider_index() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let mut snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);
    snapshot
        .payload
        .get_mut("streams")
        .and_then(serde_json::Value::as_array_mut)
        .unwrap()
        .swap(0, 2);

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(
        selection
            .keep_streams
            .iter()
            .map(|stream| stream.provider_stream_index)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
}

#[test]
fn selection_rejects_duplicate_provider_indexes() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let mut snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);
    snapshot.payload["streams"][2]["index"] = json!(1);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("remux snapshot has insufficient stream facts"),
        "{err}"
    );
}

#[test]
fn selection_keeps_default_streams_empty_for_non_default_video() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let mut snapshot = snapshot_with_video_audio_languages(["eng"]);
    snapshot.payload["streams"][0]["disposition"]["default"] = json!(false);

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert!(
        selection.default_streams.is_empty(),
        "a non-default source video must not be forced default (MKV-source behavior)"
    );
    assert!(selection.clear_default_streams.is_empty());
}

#[test]
fn selection_rejects_keep_remove_video_policy() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
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

#[test]
fn selection_preserves_attachment_source_stream_without_attachment_action() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let mut snapshot = snapshot_with_video_audio_languages(["eng"]);
    snapshot.payload["streams"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "stream-2",
            "index": 2,
            "kind": "attachment",
            "codec_name": "mjpeg",
            "filename": "cover.jpg"
        }));

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert!(
        selection
            .keep_streams
            .iter()
            .any(|stream| stream.snapshot_stream_id == "stream-2")
    );
}

#[test]
fn selection_executes_commentary_and_font_actions_without_touching_other_kinds() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [
            {
                "type": "remove_tracks",
                "target": "audio",
                "filter": {"type": "commentary"}
            },
            {
                "type": "keep_tracks",
                "target": "attachment",
                "filter": {"type": "font"}
            }
        ],
        "track_order": ["video", "audio", "subtitle"],
        "defaults": []
    });
    let snapshot = snapshot_with_remux_selector_facts();

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();
    let kept = selection
        .keep_streams
        .iter()
        .map(|stream| stream.snapshot_stream_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(kept, ["video", "main", "subtitle", "font"]);
}

#[test]
fn selection_rejects_commentary_removal_that_would_remove_final_audio() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [{
            "type": "remove_tracks",
            "target": "audio",
            "filter": {"type": "commentary"}
        }]
    });
    let mut snapshot = snapshot_with_video_audio_languages(["eng"]);
    snapshot.payload["streams"][1]["disposition"]["commentary"] = json!(true);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("no audio"), "{err}");
}

#[test]
fn selection_blocks_commentary_filter_when_fact_is_missing() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [{
            "type": "remove_tracks",
            "target": "audio",
            "filter": {"type": "commentary"}
        }]
    });
    let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("remux snapshot has insufficient stream facts"),
        "{err}"
    );
}

#[test]
fn selection_maps_shared_payload_schema_errors_to_config() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [],
        "track_order": ["video", "audio", "audio"],
        "defaults": []
    });
    let snapshot = snapshot_with_video_audio_languages(["eng"]);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("remux track_order[2] duplicates target `audio`"),
        "{err}"
    );
}

#[test]
fn selection_rejects_payload_for_a_different_snapshot() {
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 9,
        "track_actions": [],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let snapshot = snapshot_with_video_audio_languages(["eng"]);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("pins media snapshot 9, but execution loaded snapshot 1"),
        "{err}"
    );
}

#[test]
fn keep_audio_untagged_kept_under_und_and_rejected_under_eng() {
    // The remux selector inherits the shared `und` fallback (ADR 0021): an
    // untagged audio track is kept by `keep audio where language in ["und"]` and
    // is a zero-match failure under `["eng"]` (never an empty-audio artifact).
    let snapshot = snapshot_with_video_and_untagged_audio();

    let selection =
        selection_from_payload_and_snapshot(&keep_audio_language_payload(&["und"]), &snapshot)
            .unwrap();
    assert!(
        selection
            .keep_streams
            .iter()
            .any(|stream| stream.snapshot_stream_id == "stream-1"),
        "untagged audio kept under und"
    );

    let err =
        selection_from_payload_and_snapshot(&keep_audio_language_payload(&["eng"]), &snapshot)
            .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("no audio"), "{err}");
}

#[test]
fn keep_audio_matching_zero_tracks_rejects_empty_audio() {
    // A `keep audio` that matches no track must never produce an audio-less
    // artifact (ADR 0021, issue #158): it is a per-file failure instead.
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [
            {
                "type": "keep_tracks",
                "target": "audio",
                "filter": {
                    "type": "language_in",
                    "values": ["fra"]
                }
            }
        ],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("no audio"), "{err}");
}

#[test]
fn keep_audio_on_video_only_source_is_not_guarded() {
    // A source with no audio to begin with is a valid video-only remux; the
    // empty-audio guard must not fire when there was never any audio.
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [
            {
                "type": "keep_tracks",
                "target": "audio",
                "filter": {
                    "type": "language_in",
                    "values": ["fra"]
                }
            }
        ],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let snapshot = snapshot_with_video_audio_languages([]);

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(
        selection
            .keep_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<Vec<_>>(),
        vec!["stream-0"]
    );
}

#[test]
fn keep_subtitle_matching_zero_tracks_is_allowed() {
    // A subtitle-less file is a valid outcome, so a zero-match subtitle keep is
    // not guarded; the audio it leaves untouched is what keeps the file playable.
    let payload = json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [
            {
                "type": "keep_tracks",
                "target": "subtitle",
                "filter": {
                    "type": "language_in",
                    "values": ["fra"]
                }
            }
        ],
        "track_order": ["video", "audio", "subtitle"],
        "defaults": []
    });
    let mut snapshot = snapshot_with_video_audio_languages(["eng"]);
    snapshot.payload["streams"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "sub-1",
            "index": 5,
            "kind": "subtitle",
            "codec_name": "subrip",
            "language": "spa",
            "disposition": { "default": false }
        }));

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    let kept = selection
        .keep_streams
        .iter()
        .map(|stream| stream.snapshot_stream_id.as_str())
        .collect::<Vec<_>>();
    assert!(kept.contains(&"stream-0"), "video kept: {kept:?}");
    assert!(kept.contains(&"stream-1"), "audio kept: {kept:?}");
    assert!(
        !kept.contains(&"sub-1"),
        "non-matching subtitle dropped: {kept:?}"
    );
}

fn keep_audio_language_payload(values: &[&str]) -> serde_json::Value {
    json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 1,
        "track_actions": [
            {
                "type": "keep_tracks",
                "target": "audio",
                "filter": { "type": "language_in", "values": values }
            }
        ],
        "track_order": ["video", "audio"],
        "defaults": []
    })
}

fn snapshot_with_video_and_untagged_audio() -> MediaSnapshot {
    let streams = vec![
        json!({
            "id": "stream-0",
            "index": 0,
            "kind": "video",
            "codec_name": "h264",
            "disposition": { "default": true }
        }),
        json!({
            "id": "stream-1",
            "index": 1,
            "kind": "audio",
            "codec_name": "aac",
            "channels": 2,
            "disposition": { "default": false }
        }),
    ];
    MediaSnapshot {
        id: MediaSnapshotId(1),
        file_version_id: FileVersionId(1),
        probed_by: None,
        probed_at: OffsetDateTime::UNIX_EPOCH,
        payload: json!({ "streams": streams }),
    }
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

fn snapshot_with_remux_selector_facts() -> MediaSnapshot {
    MediaSnapshot {
        id: MediaSnapshotId(1),
        file_version_id: FileVersionId(1),
        probed_by: None,
        probed_at: OffsetDateTime::UNIX_EPOCH,
        payload: json!({
            "streams": [
                {
                    "id": "video",
                    "index": 0,
                    "kind": "video",
                    "codec_name": "h264",
                    "disposition": {"default": true}
                },
                {
                    "id": "main",
                    "index": 1,
                    "kind": "audio",
                    "codec_name": "aac",
                    "disposition": {"commentary": false}
                },
                {
                    "id": "commentary",
                    "index": 2,
                    "kind": "audio",
                    "codec_name": "aac",
                    "disposition": {"commentary": true}
                },
                {
                    "id": "subtitle",
                    "index": 3,
                    "kind": "subtitle",
                    "codec_name": "subrip",
                    "disposition": {"forced": true}
                },
                {
                    "id": "font",
                    "index": 4,
                    "kind": "attachment",
                    "codec_name": "ttf",
                    "filename": "OpenSans.ttf",
                    "mime_type": "font/ttf"
                },
                {
                    "id": "cover",
                    "index": 5,
                    "kind": "attachment",
                    "codec_name": "mjpeg",
                    "filename": "cover.jpg",
                    "mime_type": "image/jpeg"
                }
            ]
        }),
    }
}
