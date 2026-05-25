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
