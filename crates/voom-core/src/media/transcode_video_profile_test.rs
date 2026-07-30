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
    profile.preset = Some("p7".to_owned());
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
            "preset" => invalid.preset = Some(value.to_owned()),
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
    nvidia.preset = Some("p4".to_owned());
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
    profile.preset = Some("p4".to_owned());
    profile.decode = VideoDecodeMode::nvidia();

    let value = serde_json::to_value(&profile).unwrap();
    assert_eq!(value["decode"]["backend"], "nvidia");
    assert!(!value.as_object().unwrap().contains_key("crf"));

    let mut invalid = value;
    invalid["decode"]["device"] = serde_json::json!(0);
    assert!(serde_json::from_value::<TranscodeVideoProfile>(invalid).is_err());
}

/// Each decode-backend predicate must answer for exactly its own backend. No
/// predicate may be defined as the negation of another: `is_nvidia()` gates NVENC
/// profile validation, so a VAAPI profile answering `true` there would be validated
/// against — and dispatched to — the wrong hardware backend.
#[test]
fn decode_predicates_answer_only_for_their_own_backend() {
    let vaapi = VideoDecodeMode::vaapi();
    assert!(!vaapi.is_software());
    assert!(!vaapi.is_nvidia());
    assert!(vaapi.is_vaapi());

    let nvidia = VideoDecodeMode::nvidia();
    assert!(nvidia.is_nvidia());
    assert!(!nvidia.is_software());
    assert!(!nvidia.is_vaapi());

    let software = VideoDecodeMode::default();
    assert!(software.is_software());
    assert!(!software.is_nvidia());
    assert!(!software.is_vaapi());
}

/// `vaapi` is durable `SQLite` vocabulary from migration 0030 onward, so the parse
/// side and the stored token must agree for every backend, and unknown tokens must
/// stay rejected rather than silently degrading to software.
#[test]
fn decode_backend_tokens_round_trip_through_the_durable_vocabulary() {
    for mode in [
        VideoDecodeMode::default(),
        VideoDecodeMode::nvidia(),
        VideoDecodeMode::vaapi(),
    ] {
        assert_eq!(VideoDecodeMode::parse(mode.as_str()), Ok(mode));
    }
    assert_eq!(VideoDecodeMode::vaapi().as_str(), "vaapi");
    assert!(VideoDecodeMode::parse("qsv").is_err());
}

/// A minimal valid `hevc_vaapi` profile: `qp` quality, no preset, VAAPI decode.
fn vaapi_hevc_profile() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "vaapi-hevc".to_owned(),
        target_codec: TRANSCODE_VIDEO_CODEC.to_owned(),
        encoder: "hevc_vaapi".to_owned(),
        crf: None,
        cq: None,
        qp: Some(23),
        preset: None,
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        decode: VideoDecodeMode::vaapi(),
        copy_compatible: false,
    }
}

/// `FFmpeg` accepts `-qp 0..52` on `hevc_vaapi` and rejects 53, but 0 is the default and
/// means "auto". Admitting 0 would let an operator state a quality target and silently
/// get whatever the driver chose, so the operator vocabulary is `1..=52`.
#[test]
fn vaapi_profile_accepts_only_qp_one_through_fifty_two() {
    let mut profile = vaapi_hevc_profile();

    profile.qp = Some(1);
    assert!(validate_profile_against_descriptor(&profile).is_ok());
    profile.qp = Some(52);
    assert!(validate_profile_against_descriptor(&profile).is_ok());

    profile.qp = Some(0);
    assert!(validate_profile_against_descriptor(&profile).is_err());
    profile.qp = Some(53);
    assert!(validate_profile_against_descriptor(&profile).is_err());
    profile.qp = None;
    assert!(validate_profile_against_descriptor(&profile).is_err());
}

/// The quality knob is per-encoder and not interchangeable. `crf` and `cq` are not
/// `hevc_vaapi` options at all, and `qp` is not a knob any other shipped encoder has, so
/// a profile mixing them would generate a command the encoder rejects.
#[test]
fn quality_fields_are_exclusive_to_their_own_encoder() {
    let mut vaapi = vaapi_hevc_profile();
    vaapi.crf = Some(23);
    assert!(validate_profile_against_descriptor(&vaapi).is_err());

    let mut vaapi = vaapi_hevc_profile();
    vaapi.cq = Some(23);
    assert!(validate_profile_against_descriptor(&vaapi).is_err());

    let mut software = TranscodeVideoProfile::default_hevc();
    software.qp = Some(23);
    assert!(validate_profile_against_descriptor(&software).is_err());

    let mut nvidia = TranscodeVideoProfile::default_hevc();
    nvidia.encoder = "hevc_nvenc".to_owned();
    nvidia.preset = Some("p4".to_owned());
    nvidia.crf = None;
    nvidia.cq = Some(23);
    nvidia.qp = Some(23);
    assert!(validate_profile_against_descriptor(&nvidia).is_err());
}

/// `hevc_vaapi` exposes neither `-preset` nor `-compression_level`, so a preset on a
/// VAAPI profile is an operator knob that maps to no flag — it must be rejected, not
/// silently dropped from the generated command. The presence rule runs both ways: every
/// encoder that does have a speed knob still requires one, so widening `preset` to
/// `Option` for VAAPI cannot make it accidentally optional everywhere.
#[test]
fn preset_presence_follows_the_encoders_preset_domain() {
    let mut vaapi = vaapi_hevc_profile();
    assert!(validate_profile_against_descriptor(&vaapi).is_ok());
    vaapi.preset = Some("medium".to_owned());
    assert!(validate_profile_against_descriptor(&vaapi).is_err());

    let mut software = TranscodeVideoProfile::default_hevc();
    software.preset = None;
    assert!(validate_profile_against_descriptor(&software).is_err());

    let mut nvidia = TranscodeVideoProfile::default_hevc();
    nvidia.encoder = "hevc_nvenc".to_owned();
    nvidia.crf = None;
    nvidia.cq = Some(23);
    nvidia.preset = None;
    assert!(validate_profile_against_descriptor(&nvidia).is_err());
}

/// `codec_level` is not offered for VAAPI in this slice (ADR 0051 §4): `FFmpeg` derives
/// `general_level_idc` itself, and VOOM spells the whole levels `4.0`/`5.0`/`6.0` where
/// `FFmpeg` spells them `4`/`5`/`6`. Storing a level VOOM never normalizes would emit a
/// token the encoder does not know, so the empty `codec_levels` list must reject every
/// value rather than pass it through.
#[test]
fn vaapi_profile_rejects_codec_level() {
    let mut profile = vaapi_hevc_profile();
    profile.codec_level = Some("5.1".to_owned());
    assert!(validate_profile_against_descriptor(&profile).is_err());
    profile.codec_level = Some("5".to_owned());
    assert!(validate_profile_against_descriptor(&profile).is_err());
}

/// VAAPI encodes from hardware surfaces, so `nv12` and `p010` are the whole vocabulary —
/// a software format such as `yuv420p` never reaches the encoder. And `FFmpeg` fails a
/// 10-bit surface under an 8-bit profile with `No usable encoding profile found`
/// (design §2.2); rejecting `main` + `p010` here turns that mid-encode failure into a
/// validation error naming the conflict.
#[test]
fn vaapi_profile_enforces_surface_formats_and_profile_bit_depth() {
    let mut profile = vaapi_hevc_profile();

    profile.pixel_format = Some("nv12".to_owned());
    profile.codec_profile = Some("main".to_owned());
    assert!(validate_profile_against_descriptor(&profile).is_ok());

    profile.pixel_format = Some("p010".to_owned());
    profile.codec_profile = Some("main10".to_owned());
    assert!(validate_profile_against_descriptor(&profile).is_ok());

    profile.codec_profile = Some("main".to_owned());
    assert!(validate_profile_against_descriptor(&profile).is_err());

    profile.codec_profile = None;
    profile.pixel_format = Some("yuv420p".to_owned());
    assert!(validate_profile_against_descriptor(&profile).is_err());
    profile.pixel_format = Some("yuv420p10le".to_owned());
    assert!(validate_profile_against_descriptor(&profile).is_err());
}

/// A hardware decode backend and the encoder must be the same accelerator: hardware
/// frames produced by one backend cannot enter the other's encoder, and dispatch routes
/// a ticket on that pairing, so a mismatched profile would be scheduled onto a device
/// that cannot run it. Software decode into a hardware encoder stays legal — it is the
/// upload path in design §7.
#[test]
fn hardware_decode_requires_a_matching_hardware_encoder() {
    let mut vaapi_decode_nvenc = TranscodeVideoProfile::default_hevc();
    vaapi_decode_nvenc.encoder = "hevc_nvenc".to_owned();
    vaapi_decode_nvenc.crf = None;
    vaapi_decode_nvenc.cq = Some(23);
    vaapi_decode_nvenc.preset = Some("p4".to_owned());
    vaapi_decode_nvenc.decode = VideoDecodeMode::vaapi();
    assert!(validate_profile_against_descriptor(&vaapi_decode_nvenc).is_err());

    let mut vaapi_decode_software = TranscodeVideoProfile::default_hevc();
    vaapi_decode_software.decode = VideoDecodeMode::vaapi();
    assert!(validate_profile_against_descriptor(&vaapi_decode_software).is_err());

    let mut nvidia_decode_vaapi = vaapi_hevc_profile();
    nvidia_decode_vaapi.decode = VideoDecodeMode::nvidia();
    assert!(validate_profile_against_descriptor(&nvidia_decode_vaapi).is_err());

    let mut software_decode_vaapi = vaapi_hevc_profile();
    software_decode_vaapi.decode = VideoDecodeMode::default();
    assert!(validate_profile_against_descriptor(&software_decode_vaapi).is_ok());
}

/// A VAAPI profile carries no `preset` and no `crf`, so neither may appear in its
/// durable JSON, and `qp` must survive the round trip — the store and the worker request
/// both read this shape back.
#[test]
fn vaapi_profile_serializes_without_preset_or_crf() {
    let profile = vaapi_hevc_profile();

    let value = serde_json::to_value(&profile).unwrap();

    let obj = value.as_object().unwrap();
    assert!(!obj.contains_key("preset"));
    assert!(!obj.contains_key("crf"));
    assert!(!obj.contains_key("cq"));
    assert_eq!(value["qp"], 23);
    assert_eq!(value["decode"]["backend"], "vaapi");
    assert_eq!(
        serde_json::from_value::<TranscodeVideoProfile>(value).unwrap(),
        profile
    );
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
    bad_preset.preset = Some("turbofast".to_owned());
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
