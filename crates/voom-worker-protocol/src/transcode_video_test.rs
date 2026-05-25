use super::*;

#[test]
fn transcode_video_request_serializes_stable_snake_case_shape() {
    let request = TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: "/library/input.mkv".to_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: 1234,
                content_hash: "blake3:abc".to_owned(),
                modified_at: Some("2026-05-25T00:00:00Z".to_owned()),
                local_file_key: None,
            },
        },
        output: TranscodeVideoOutput {
            staging_root: "/tmp/voom-stage".to_owned(),
            path: "/tmp/voom-stage/ticket-1/lease-1/input.hevc.mkv".to_owned(),
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
            overwrite: false,
        },
        profile: TranscodeVideoProfile::default_hevc(),
    };

    let json = serde_json::to_value(&request).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "input": {
                "path": "/library/input.mkv",
                "expected": {
                    "size_bytes": 1234,
                    "content_hash": "blake3:abc",
                    "modified_at": "2026-05-25T00:00:00Z",
                    "local_file_key": null
                }
            },
            "output": {
                "staging_root": "/tmp/voom-stage",
                "path": "/tmp/voom-stage/ticket-1/lease-1/input.hevc.mkv",
                "container": "mkv",
                "video_codec": "hevc",
                "overwrite": false
            },
            "profile": {
                "name": "default-hevc",
                "encoder": "libx265",
                "crf": 23,
                "preset": "medium"
            }
        })
    );
}

#[test]
fn transcode_video_result_status_serializes_as_transcoded() {
    let result = TranscodeVideoResult {
        status: TranscodeVideoStatus::Transcoded,
        provider: "ffmpeg".to_owned(),
        provider_version: "ffmpeg version 7.0".to_owned(),
        input_pre: observed_facts("blake3:input-before"),
        input_post: observed_facts("blake3:input-after"),
        output: observed_facts("blake3:output"),
        output_container: "mkv".to_owned(),
        output_video_codec: "hevc".to_owned(),
    };

    let json = serde_json::to_value(&result).unwrap();

    assert_eq!(json["status"], "transcoded");
    assert_eq!(json["input_pre"]["content_hash"], "blake3:input-before");
    assert_eq!(json["input_post"]["content_hash"], "blake3:input-after");
    assert_eq!(json["output"]["content_hash"], "blake3:output");
    assert_eq!(json["output_container"], "mkv");
    assert_eq!(json["output_video_codec"], "hevc");
}

#[test]
fn transcode_video_payloads_reject_unknown_fields() {
    let request_err = serde_json::from_value::<TranscodeVideoRequest>(serde_json::json!({
        "input": {
            "path": "/library/input.mkv",
            "expected": {
                "size_bytes": 1234,
                "content_hash": "blake3:abc",
                "modified_at": null,
                "local_file_key": null
            }
        },
        "output": {
            "staging_root": "/tmp/voom-stage",
            "path": "/tmp/voom-stage/ticket-1/lease-1/input.hevc.mkv",
            "container": "mkv",
            "video_codec": "hevc",
            "overwrite": false
        },
        "profile": {
            "name": "default-hevc",
            "encoder": "libx265",
            "crf": 23,
            "preset": "medium"
        },
        "unexpected": true
    }))
    .unwrap_err();
    assert!(request_err.to_string().contains("unknown field"));

    let result_err = serde_json::from_value::<TranscodeVideoResult>(serde_json::json!({
        "status": "transcoded",
        "provider": "ffmpeg",
        "provider_version": "ffmpeg version 7.0",
        "input_pre": { "size_bytes": 1234, "content_hash": "blake3:input-before" },
        "input_post": { "size_bytes": 1234, "content_hash": "blake3:input-after" },
        "output": { "size_bytes": 987, "content_hash": "blake3:output" },
        "output_container": "mkv",
        "output_video_codec": "hevc",
        "unexpected": true
    }))
    .unwrap_err();
    assert!(result_err.to_string().contains("unknown field"));
}

fn observed_facts(content_hash: &str) -> TranscodeVideoObservedFacts {
    TranscodeVideoObservedFacts {
        size_bytes: 1234,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}
