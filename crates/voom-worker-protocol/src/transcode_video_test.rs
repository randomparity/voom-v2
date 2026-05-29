use super::*;

#[test]
fn transcode_video_request_serializes_stable_snake_case_shape() {
    let request = sample_request();

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
                "target_codec": "hevc",
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
        output_width: 1920,
        output_height: 1080,
        output_pixel_format: "yuv420p".to_owned(),
        copied_video: false,
    };

    let json = serde_json::to_value(&result).unwrap();

    assert_eq!(json["status"], "transcoded");
    assert_eq!(json["input_pre"]["content_hash"], "blake3:input-before");
    assert_eq!(json["input_post"]["content_hash"], "blake3:input-after");
    assert_eq!(json["output"]["content_hash"], "blake3:output");
    assert_eq!(json["output_container"], "mkv");
    assert_eq!(json["output_video_codec"], "hevc");
    assert_eq!(json["output_width"], 1920);
    assert_eq!(json["output_height"], 1080);
    assert_eq!(json["output_pixel_format"], "yuv420p");
    // copied_video: false is omitted
    assert!(!json.as_object().unwrap().contains_key("copied_video"));
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
            "target_codec": "hevc",
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
        "output_width": 1920,
        "output_height": 1080,
        "output_pixel_format": "yuv420p",
        "unexpected": true
    }))
    .unwrap_err();
    assert!(result_err.to_string().contains("unknown field"));
}

#[test]
fn transcode_video_contract_helpers_pin_canonical_values_and_aliases() {
    assert_eq!(TRANSCODE_VIDEO_CONTAINER, "mkv");
    assert_eq!(TRANSCODE_VIDEO_CODEC, "hevc");
    assert_eq!(TRANSCODE_VIDEO_PROFILE, "default-hevc");

    assert!(is_supported_transcode_video_container("mkv"));
    assert!(is_supported_transcode_video_container("mp4"));
    assert!(is_supported_transcode_video_codec("hevc"));
    assert!(is_supported_transcode_video_codec("h265"));
    assert!(is_supported_transcode_video_codec("HEVC"));
    assert!(is_supported_transcode_video_codec("H265"));
    assert!(!is_supported_transcode_video_container("avi"));
    assert!(!is_supported_transcode_video_codec("h264"));
}

#[test]
fn normalize_codec_token_collapses_case_and_whitespace() {
    assert_eq!(normalize_codec_token("Main 10"), "main10");
    assert_eq!(normalize_codec_token("main10"), "main10");
    assert_eq!(normalize_codec_token("  HEVC  "), "hevc");
    assert_eq!(normalize_codec_token(""), "");
}

fn observed_facts(content_hash: &str) -> TranscodeVideoObservedFacts {
    TranscodeVideoObservedFacts {
        size_bytes: 1234,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

fn sample_request() -> TranscodeVideoRequest {
    TranscodeVideoRequest {
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
        copy_video: false,
    }
}

#[test]
fn supported_codecs_and_containers_are_recognized() {
    assert!(is_supported_transcode_video_codec("hevc"));
    assert!(is_supported_transcode_video_codec("H265")); // alias, case-insensitive
    assert!(is_supported_transcode_video_codec("av1"));
    assert!(is_supported_transcode_video_codec("AV1"));
    assert!(!is_supported_transcode_video_codec("h264"));
    assert!(is_supported_transcode_video_container("mkv"));
    assert!(is_supported_transcode_video_container("mp4"));
    assert!(!is_supported_transcode_video_container("avi"));
}

#[test]
fn default_hevc_profile_serializes_minimal_superset() {
    let profile = TranscodeVideoProfile::default_hevc();
    let value = serde_json::to_value(&profile).unwrap();
    // Required keys present.
    assert_eq!(value["name"], "default-hevc");
    assert_eq!(value["target_codec"], "hevc");
    assert_eq!(value["encoder"], "libx265");
    assert_eq!(value["crf"], 23);
    assert_eq!(value["preset"], "medium");
    // All optional keys omitted; copy_compatible (false) omitted.
    let obj = value.as_object().unwrap();
    assert!(!obj.contains_key("tune"));
    assert!(!obj.contains_key("codec_profile"));
    assert!(!obj.contains_key("codec_level"));
    assert!(!obj.contains_key("pixel_format"));
    assert!(!obj.contains_key("max_width"));
    assert!(!obj.contains_key("max_height"));
    assert!(!obj.contains_key("copy_compatible"));
    assert_eq!(obj.len(), 5);
}

#[test]
fn request_carries_copy_video_flag_skipped_when_false() {
    let req = sample_request(); // copy_video defaults false
    let value = serde_json::to_value(&req).unwrap();
    assert!(!value.as_object().unwrap().contains_key("copy_video"));

    let mut req_copy = sample_request();
    req_copy.copy_video = true;
    let value = serde_json::to_value(&req_copy).unwrap();
    assert_eq!(value["copy_video"], true);
}

#[test]
fn profile_validates_against_its_encoder_descriptor() {
    let ok = TranscodeVideoProfile::default_hevc();
    assert!(validate_profile_against_descriptor(&ok).is_ok());

    let mut bad_codec = TranscodeVideoProfile::default_hevc();
    bad_codec.target_codec = "av1".to_owned(); // libx265 is hevc-only
    assert!(validate_profile_against_descriptor(&bad_codec).is_err());

    let mut bad_crf = TranscodeVideoProfile::default_hevc();
    bad_crf.crf = 60; // > 51 for libx265
    assert!(validate_profile_against_descriptor(&bad_crf).is_err());

    let mut bad_combo = TranscodeVideoProfile::default_hevc();
    bad_combo.pixel_format = Some("yuv420p10le".to_owned());
    bad_combo.codec_profile = Some("main".to_owned()); // 10-bit under 8-bit profile
    assert!(validate_profile_against_descriptor(&bad_combo).is_err());

    let mut unknown_encoder = TranscodeVideoProfile::default_hevc();
    unknown_encoder.encoder = "libx264".to_owned(); // no descriptor
    assert!(validate_profile_against_descriptor(&unknown_encoder).is_err());

    let mut bad_preset = TranscodeVideoProfile::default_hevc();
    bad_preset.preset = "turbofast".to_owned(); // not an x265 preset
    assert!(validate_profile_against_descriptor(&bad_preset).is_err());

    let mut bad_tune = TranscodeVideoProfile::default_hevc();
    bad_tune.tune = Some("film".to_owned()); // not an x265 tune
    assert!(validate_profile_against_descriptor(&bad_tune).is_err());

    let mut bad_level = TranscodeVideoProfile::default_hevc();
    bad_level.codec_level = Some("2.0".to_owned()); // not an x265 level
    assert!(validate_profile_against_descriptor(&bad_level).is_err());

    let mut bad_pixel_format = TranscodeVideoProfile::default_hevc();
    bad_pixel_format.pixel_format = Some("rgb24".to_owned()); // not an x265 pixel format
    assert!(validate_profile_against_descriptor(&bad_pixel_format).is_err());
}

#[test]
fn result_carries_observed_output_dimensions_and_copied_flag() {
    let json = serde_json::json!({
        "status": "transcoded",
        "provider": "ffmpeg",
        "provider_version": "ffmpeg version 7.0",
        "input_pre": {"size_bytes": 1, "content_hash": "blake3:a"},
        "input_post": {"size_bytes": 1, "content_hash": "blake3:a"},
        "output": {"size_bytes": 2, "content_hash": "blake3:b"},
        "output_container": "mp4",
        "output_video_codec": "av1",
        "output_width": 1920,
        "output_height": 1080,
        "output_pixel_format": "yuv420p",
        "copied_video": false
    });
    let result: TranscodeVideoResult = serde_json::from_value(json).unwrap();
    assert_eq!(result.output_width, 1920);
    assert_eq!(result.output_height, 1080);
    assert_eq!(result.output_pixel_format, "yuv420p");
    assert!(!result.copied_video);
}
