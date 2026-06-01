use super::*;

#[test]
fn contract_helpers_pin_canonical_values_and_aliases() {
    assert_eq!(TRANSCODE_VIDEO_CONTAINER, "mkv");
    assert_eq!(TRANSCODE_VIDEO_CODEC, "hevc");
    assert_eq!(TRANSCODE_VIDEO_PROFILE, "default-hevc");

    assert!(is_supported_transcode_video_container("mkv"));
    assert!(is_supported_transcode_video_container("mp4"));
    assert!(is_supported_transcode_video_codec("hevc"));
    assert!(is_supported_transcode_video_codec("h265"));
    assert!(is_supported_transcode_video_codec("HEVC"));
    assert!(is_supported_transcode_video_codec("H265"));
    assert!(is_supported_transcode_video_codec("av1"));
    assert!(is_supported_transcode_video_codec("AV1"));
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

#[test]
fn default_hevc_profile_serializes_minimal_superset() {
    let profile = TranscodeVideoProfile::default_hevc();
    let value = serde_json::to_value(&profile).unwrap();
    assert_eq!(value["name"], "default-hevc");
    assert_eq!(value["target_codec"], "hevc");
    assert_eq!(value["encoder"], "libx265");
    assert_eq!(value["crf"], 23);
    assert_eq!(value["preset"], "medium");

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
fn profile_validates_against_its_encoder_descriptor() {
    let ok = TranscodeVideoProfile::default_hevc();
    assert!(validate_profile_against_descriptor(&ok).is_ok());

    let mut bad_codec = TranscodeVideoProfile::default_hevc();
    bad_codec.target_codec = "av1".to_owned();
    assert!(validate_profile_against_descriptor(&bad_codec).is_err());

    let mut bad_crf = TranscodeVideoProfile::default_hevc();
    bad_crf.crf = 60;
    assert!(validate_profile_against_descriptor(&bad_crf).is_err());

    let mut bad_combo = TranscodeVideoProfile::default_hevc();
    bad_combo.pixel_format = Some("yuv420p10le".to_owned());
    bad_combo.codec_profile = Some("main".to_owned());
    assert!(validate_profile_against_descriptor(&bad_combo).is_err());

    let mut unknown_encoder = TranscodeVideoProfile::default_hevc();
    unknown_encoder.encoder = "libx264".to_owned();
    assert!(validate_profile_against_descriptor(&unknown_encoder).is_err());

    let mut bad_preset = TranscodeVideoProfile::default_hevc();
    bad_preset.preset = "turbofast".to_owned();
    assert!(validate_profile_against_descriptor(&bad_preset).is_err());

    let mut bad_tune = TranscodeVideoProfile::default_hevc();
    bad_tune.tune = Some("film".to_owned());
    assert!(validate_profile_against_descriptor(&bad_tune).is_err());

    let mut bad_level = TranscodeVideoProfile::default_hevc();
    bad_level.codec_level = Some("2.0".to_owned());
    assert!(validate_profile_against_descriptor(&bad_level).is_err());

    let mut bad_pixel_format = TranscodeVideoProfile::default_hevc();
    bad_pixel_format.pixel_format = Some("rgb24".to_owned());
    assert!(validate_profile_against_descriptor(&bad_pixel_format).is_err());
}
