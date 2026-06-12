use super::*;

#[test]
fn normalizes_ffprobe_json_into_sprint10_snapshot() {
    let raw_result = serde_json::from_str(include_str!("../fixtures/ffprobe/basic-mp4.json"));
    assert!(raw_result.is_ok());
    let Ok(raw) = raw_result else {
        return;
    };

    let snapshot_result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");
    assert!(snapshot_result.is_ok());
    let Ok(snapshot) = snapshot_result else {
        return;
    };

    assert_eq!(snapshot["format"], "sprint10-v1");
    assert_eq!(snapshot["probe"]["provider"], "ffprobe");
    assert_eq!(snapshot["probe"]["provider_version"], "7.0");
    assert_eq!(snapshot["probe"]["command"], "ffprobe");
    assert_eq!(snapshot["probe"]["probed_at"], "2026-05-24T00:00:00Z");
    assert!(snapshot["probe"].get("observed_at").is_none());
    assert_eq!(snapshot["container"]["duration_seconds"], 1.0);
    assert_eq!(snapshot["container"]["bit_rate"], 128_000);
    assert_eq!(snapshot["streams"][0]["kind"], "video");
    assert_eq!(snapshot["streams"][0]["avg_frame_rate"], "30/1");
    assert_eq!(snapshot["streams"][1]["kind"], "audio");
    assert!(snapshot["raw"]["ffprobe_json"].is_object());
}

#[test]
fn normalizes_audio_channel_layout_and_role_tag() {
    // Channel layout ("5.1", "stereo") and the audio role tag are oracle facts in
    // Chaos Librarian probe comparison; omitting them reads as a probe mismatch.
    let raw = serde_json::json!({
        "format": { "format_name": "matroska,webm" },
        "streams": [
            {
                "index": 1,
                "codec_type": "audio",
                "codec_name": "aac",
                "channels": 6,
                "channel_layout": "5.1",
                "tags": { "language": "eng", "role": "main" }
            }
        ]
    });

    let snapshot_result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");
    assert!(snapshot_result.is_ok());
    let Ok(snapshot) = snapshot_result else {
        return;
    };

    assert_eq!(snapshot["streams"][0]["channel_layout"], "5.1");
    assert_eq!(snapshot["streams"][0]["role"], "main");
}

#[test]
fn omits_channel_layout_and_role_when_ffprobe_does_not_report_them() {
    let raw = serde_json::json!({
        "format": { "format_name": "mov,mp4" },
        "streams": [
            { "index": 0, "codec_type": "video", "codec_name": "h264" }
        ]
    });

    let snapshot_result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");
    assert!(snapshot_result.is_ok());
    let Ok(snapshot) = snapshot_result else {
        return;
    };

    assert!(snapshot["streams"][0].get("channel_layout").is_none());
    assert!(snapshot["streams"][0].get("role").is_none());
}

#[test]
fn matches_matroska_tag_keys_case_insensitively() {
    // Matroska tag names are case-insensitive by spec; ffprobe passes through
    // uppercase keys such as ROLE and HANDLER_NAME unchanged.
    let raw = serde_json::json!({
        "format": { "format_name": "matroska,webm" },
        "streams": [
            {
                "index": 1,
                "codec_type": "audio",
                "codec_name": "aac",
                "tags": {
                    "LANGUAGE": "eng",
                    "TITLE": "Main Audio",
                    "ROLE": "main",
                    "HANDLER_NAME": "Main Audio"
                }
            }
        ]
    });

    let snapshot_result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");
    assert!(snapshot_result.is_ok());
    let Ok(snapshot) = snapshot_result else {
        return;
    };

    assert_eq!(snapshot["streams"][0]["language"], "eng");
    assert_eq!(snapshot["streams"][0]["title"], "Main Audio");
    assert_eq!(snapshot["streams"][0]["role"], "main");
    assert_eq!(snapshot["streams"][0]["handler_name"], "Main Audio");
}

#[test]
fn captures_mp4_handler_name_without_synthesizing_title() {
    // MP4 has no stream-title atom; ffmpeg stores titles in the hdlr box, which
    // ffprobe reports as handler_name. The snapshot records the raw fact and
    // leaves title interpretation to consumers.
    let raw = serde_json::json!({
        "format": { "format_name": "mov,mp4" },
        "streams": [
            {
                "index": 1,
                "codec_type": "audio",
                "codec_name": "aac",
                "tags": { "language": "eng", "handler_name": "Main Audio" }
            }
        ]
    });

    let snapshot_result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");
    assert!(snapshot_result.is_ok());
    let Ok(snapshot) = snapshot_result else {
        return;
    };

    assert_eq!(snapshot["streams"][0]["handler_name"], "Main Audio");
    assert!(snapshot["streams"][0].get("title").is_none());
}

#[test]
fn normalizes_stream_language_and_disposition_for_mp4() {
    let raw = serde_json::json!({
        "format": { "format_name": "mov,mp4" },
        "streams": [
            {
                "index": 1,
                "codec_type": "audio",
                "codec_name": "aac",
                "tags": { "language": "und" },
                "disposition": { "default": 1, "forced": 0 }
            }
        ]
    });

    let snapshot_result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");
    assert!(snapshot_result.is_ok());
    let Ok(snapshot) = snapshot_result else {
        return;
    };

    assert_eq!(snapshot["streams"][0]["language"], "und");
    assert_eq!(snapshot["streams"][0]["disposition"]["default"], true);
    assert_eq!(snapshot["streams"][0]["disposition"]["forced"], false);
    // ffprobe reported no `title` tag and no `comment` disposition, so neither is
    // synthesized in the normalized snapshot.
    assert!(snapshot["streams"][0].get("title").is_none());
    assert!(
        snapshot["streams"][0]["disposition"]
            .get("commentary")
            .is_none()
    );
}

#[test]
fn normalizes_stream_title_and_commentary_disposition() {
    // The audio-transcode planner requires per-stream `title` and a `commentary`
    // disposition fact; ffprobe names them `tags.title` and `disposition.comment`,
    // so normalization lifts the title and renames the commentary flag.
    let raw = serde_json::json!({
        "format": { "format_name": "matroska,webm" },
        "streams": [
            {
                "index": 1,
                "codec_type": "audio",
                "codec_name": "aac",
                "channels": 2,
                "tags": { "language": "eng", "title": "Director Commentary" },
                "disposition": { "default": 0, "forced": 0, "comment": 1 }
            }
        ]
    });

    let snapshot_result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");
    assert!(snapshot_result.is_ok());
    let Ok(snapshot) = snapshot_result else {
        return;
    };

    assert_eq!(snapshot["streams"][0]["title"], "Director Commentary");
    assert_eq!(snapshot["streams"][0]["channels"], 2);
    assert_eq!(snapshot["streams"][0]["disposition"]["commentary"], true);
    assert_eq!(snapshot["streams"][0]["disposition"]["default"], false);
    // The raw ffprobe key `comment` is renamed, not passed through verbatim.
    assert!(
        snapshot["streams"][0]["disposition"]
            .get("comment")
            .is_none()
    );
}

#[test]
fn normalizes_stream_language_and_disposition_for_mkv_subtitles() {
    let raw = serde_json::json!({
        "format": { "format_name": "matroska,webm" },
        "streams": [
            {
                "index": 0,
                "codec_type": "audio",
                "codec_name": "flac",
                "tags": { "language": "eng" },
                "disposition": { "default": true }
            },
            {
                "index": 1,
                "codec_type": "subtitle",
                "codec_name": "subrip",
                "tags": { "language": "spa" },
                "disposition": { "forced": "1" }
            }
        ]
    });

    let snapshot_result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");
    assert!(snapshot_result.is_ok());
    let Ok(snapshot) = snapshot_result else {
        return;
    };

    assert_eq!(snapshot["streams"][0]["language"], "eng");
    assert_eq!(snapshot["streams"][0]["disposition"]["default"], true);
    assert!(
        snapshot["streams"][0]["disposition"]
            .get("forced")
            .is_none()
    );
    assert_eq!(snapshot["streams"][1]["kind"], "subtitle");
    assert_eq!(snapshot["streams"][1]["language"], "spa");
    assert_eq!(snapshot["streams"][1]["disposition"]["forced"], true);
}

#[test]
fn rejects_non_numeric_duration() {
    let raw = serde_json::json!({
        "format": { "duration": "not-a-number" },
        "streams": []
    });

    let result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");

    assert!(matches!(
        result.as_ref().map_err(WorkerError::failure_class),
        Err(voom_core::FailureClass::MalformedWorkerResult)
    ));
}

#[test]
fn rejects_malformed_disposition_values() {
    let raw = serde_json::json!({
        "format": {},
        "streams": [
            {
                "index": 0,
                "codec_type": "audio",
                "disposition": { "default": "maybe" }
            }
        ]
    });

    let result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");

    assert!(matches!(
        result.as_ref().map_err(WorkerError::failure_class),
        Err(voom_core::FailureClass::MalformedWorkerResult)
    ));
}

#[test]
fn omits_unknown_ffprobe_sentinel_values() {
    let raw = serde_json::json!({
        "format": {
            "format_name": "N/A",
            "duration": "N/A",
            "bit_rate": "N/A"
        },
        "streams": [
            {
                "index": 0,
                "codec_type": "unknown",
                "codec_name": "N/A",
                "width": "N/A"
            }
        ]
    });

    let snapshot_result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");
    assert!(snapshot_result.is_ok());
    let Ok(snapshot) = snapshot_result else {
        return;
    };

    assert!(snapshot["container"].get("format_name").is_none());
    assert!(snapshot["container"].get("duration_seconds").is_none());
    assert!(snapshot["container"].get("bit_rate").is_none());
    assert_eq!(snapshot["streams"][0]["index"], 0);
    assert!(snapshot["streams"][0].get("codec_name").is_none());
    assert!(snapshot["streams"][0].get("width").is_none());
}

#[test]
fn rejects_malformed_present_numeric_values() {
    let raw = serde_json::json!({
        "format": {},
        "streams": [
            {
                "width": -1
            }
        ]
    });

    let result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");

    assert!(matches!(
        result.as_ref().map_err(WorkerError::failure_class),
        Err(voom_core::FailureClass::MalformedWorkerResult)
    ));
}

#[test]
fn captures_video_pixel_format_profile_and_level() {
    let raw = serde_json::json!({
        "format": {"format_name": "matroska,webm", "duration": "10.0"},
        "streams": [{
            "index": 0, "codec_type": "video", "codec_name": "hevc",
            "width": 1920, "height": 1080,
            "pix_fmt": "yuv420p10le", "profile": "Main 10", "level": 153
        }]
    });
    let result = normalize_ffprobe_json(raw, "ffprobe 7.0", "2026-05-28T00:00:00Z");
    assert!(result.is_ok());
    let Ok(snapshot) = result else { return };
    let stream = &snapshot["streams"][0];
    assert_eq!(stream["pixel_format"], "yuv420p10le");
    assert_eq!(stream["profile"], "Main 10");
    assert_eq!(stream["level"], "153");
}

#[test]
fn omits_absent_video_profile_fields() {
    let raw = serde_json::json!({
        "streams": [{"index": 0, "codec_type": "video", "codec_name": "hevc", "width": 1, "height": 1}]
    });
    let result = normalize_ffprobe_json(raw, "v", "t");
    assert!(result.is_ok());
    let Ok(snapshot) = result else { return };
    let Some(stream) = snapshot["streams"][0].as_object() else {
        return;
    };
    assert!(!stream.contains_key("pixel_format"));
    assert!(!stream.contains_key("profile"));
    assert!(!stream.contains_key("level"));
}

#[test]
fn rejects_malformed_top_level_sections() {
    for raw in [
        serde_json::json!({
            "format": [],
            "streams": []
        }),
        serde_json::json!({
            "format": {},
            "streams": {}
        }),
    ] {
        let result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");

        assert!(matches!(
            result.as_ref().map_err(WorkerError::failure_class),
            Err(voom_core::FailureClass::MalformedWorkerResult)
        ));
    }
}
