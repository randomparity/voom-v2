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
fn nvidia_hevc_profile_requires_cq_and_closed_vocabulary() {
    let mut profile = TranscodeVideoProfile::default_hevc();
    profile.encoder = "hevc_nvenc".to_owned();
    profile.crf = None;
    profile.cq = Some(23);
    profile.preset = "p7".to_owned();
    profile.tune = Some("uhq".to_owned());
    profile.codec_profile = Some("main10".to_owned());
    profile.codec_level = Some("6.2".to_owned());
    profile.pixel_format = Some("yuv420p10le".to_owned());

    assert!(validate_profile_against_descriptor(&profile).is_ok());

    profile.cq = Some(0);
    assert!(validate_profile_against_descriptor(&profile).is_err());
    profile.cq = Some(52);
    assert!(validate_profile_against_descriptor(&profile).is_err());
    profile.cq = Some(23);

    for (field, value) in [
        ("preset", "slow"),
        ("tune", "film"),
        ("codec_profile", "rext"),
        ("codec_level", "7.0"),
        ("pixel_format", "yuv444p"),
    ] {
        let mut invalid = profile.clone();
        match field {
            "preset" => invalid.preset = value.to_owned(),
            "tune" => invalid.tune = Some(value.to_owned()),
            "codec_profile" => invalid.codec_profile = Some(value.to_owned()),
            "codec_level" => invalid.codec_level = Some(value.to_owned()),
            "pixel_format" => invalid.pixel_format = Some(value.to_owned()),
            _ => unreachable!(),
        }
        assert!(validate_profile_against_descriptor(&invalid).is_err());
    }
}

#[test]
fn quality_and_decode_modes_are_mutually_compatible() {
    let mut software = TranscodeVideoProfile::default_hevc();
    software.cq = Some(23);
    assert!(validate_profile_against_descriptor(&software).is_err());

    software.cq = None;
    software.decode = VideoDecodeMode::nvidia();
    assert!(validate_profile_against_descriptor(&software).is_err());

    let mut nvidia = TranscodeVideoProfile::default_hevc();
    nvidia.encoder = "hevc_nvenc".to_owned();
    nvidia.preset = "p4".to_owned();
    nvidia.cq = Some(23);
    assert!(validate_profile_against_descriptor(&nvidia).is_err());

    nvidia.crf = None;
    assert!(validate_profile_against_descriptor(&nvidia).is_ok());
}

#[test]
fn nvidia_decode_serializes_as_strict_typed_mode() {
    let mut profile = TranscodeVideoProfile::default_hevc();
    profile.encoder = "hevc_nvenc".to_owned();
    profile.crf = None;
    profile.cq = Some(23);
    profile.preset = "p4".to_owned();
    profile.decode = VideoDecodeMode::nvidia();

    let value = serde_json::to_value(&profile).unwrap();
    assert_eq!(value["decode"]["backend"], "nvidia");
    assert!(!value.as_object().unwrap().contains_key("crf"));

    let mut invalid = value;
    invalid["decode"]["device"] = serde_json::json!(0);
    assert!(serde_json::from_value::<TranscodeVideoProfile>(invalid).is_err());
}

#[test]
fn transcode_video_profile_rejects_unknown_durable_fields() {
    let mut value = serde_json::to_value(TranscodeVideoProfile::default_hevc()).unwrap();
    value["future_profile"] = serde_json::json!(true);

    let error = serde_json::from_value::<TranscodeVideoProfile>(value).unwrap_err();

    assert!(error.to_string().contains("unknown field `future_profile`"));
}

#[test]
fn profile_validates_against_its_encoder_descriptor() {
    let ok = TranscodeVideoProfile::default_hevc();
    assert!(validate_profile_against_descriptor(&ok).is_ok());

    let mut bad_codec = TranscodeVideoProfile::default_hevc();
    bad_codec.target_codec = "av1".to_owned();
    assert!(validate_profile_against_descriptor(&bad_codec).is_err());

    let mut bad_crf = TranscodeVideoProfile::default_hevc();
    bad_crf.crf = Some(60);
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
