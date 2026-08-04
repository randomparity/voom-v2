use voom_store::repo::policy::video_profiles::{NewVideoProfile, VideoProfile};

use crate::cli::{VideoDecodeBackendArg, VideoProfileFields};

use super::ProfileData;

#[test]
fn profile_data_maps_every_field_from_video_profile() {
    let profile = VideoProfile {
        id: "vp-hevc-archive".to_owned(),
        name: "hevc-archive".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: Some(18),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("slow".to_owned()),
        tune: Some("grain".to_owned()),
        codec_profile: Some("main10".to_owned()),
        codec_level: Some("5.1".to_owned()),
        pixel_format: Some("yuv420p10le".to_owned()),
        max_width: Some(1920),
        max_height: Some(1080),
        output_container: "mkv".to_owned(),
        copy_compatible: true,
        decode: voom_core::VideoDecodeMode::default(),
        retired_at: Some(time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()),
    };

    let data = ProfileData::from(profile);

    assert_eq!(data.id, "vp-hevc-archive");
    assert_eq!(data.name, "hevc-archive");
    assert_eq!(data.target_codec, "hevc");
    assert_eq!(data.encoder, "libx265");
    assert_eq!(data.crf, Some(18));
    assert!(data.cq.is_none());
    assert!(data.decode.is_software());
    assert_eq!(data.preset.as_deref(), Some("slow"));
    assert!(data.qp.is_none());
    assert_eq!(data.tune.as_deref(), Some("grain"));
    assert_eq!(data.codec_profile.as_deref(), Some("main10"));
    assert_eq!(data.codec_level.as_deref(), Some("5.1"));
    assert_eq!(data.pixel_format.as_deref(), Some("yuv420p10le"));
    assert_eq!(data.max_width, Some(1920));
    assert_eq!(data.max_height, Some(1080));
    assert_eq!(data.output_container, "mkv");
    assert!(data.copy_compatible);
    assert_eq!(
        data.retired_at.as_deref(),
        Some("2023-11-14T22:13:20.000000000Z")
    );
}

/// The clap arguments are the only authoring path for a durable profile, so the
/// conversion must carry `qp` and an absent `preset` straight through. Hardcoding
/// either — as this conversion did while `--qp` did not exist — makes a VAAPI
/// profile unauthorable from the CLI no matter what the DB and descriptor accept.
#[test]
fn vaapi_fields_convert_to_a_qp_profile_with_no_preset() {
    let fields = VideoProfileFields {
        name: "gpu-vaapi-hevc".to_owned(),
        encoder: "hevc_vaapi".to_owned(),
        crf: None,
        cq: None,
        qp: Some(23),
        bitrate_kbps: None,
        preset: None,
        tune: None,
        codec_profile: Some("main10".to_owned()),
        codec_level: None,
        pixel_format: Some("p010".to_owned()),
        max_width: None,
        max_height: None,
        output_container: "mkv".to_owned(),
        copy_compatible: false,
        decode: VideoDecodeBackendArg::Vaapi,
    };

    let new = NewVideoProfile::from(fields);

    assert_eq!(new.qp, Some(23));
    assert!(new.preset.is_none());
    assert!(new.crf.is_none());
    assert!(new.cq.is_none());
    assert_eq!(new.codec_profile.as_deref(), Some("main10"));
    assert_eq!(new.pixel_format.as_deref(), Some("p010"));
    assert!(new.decode.is_vaapi());
}

/// A preset-domain encoder must still round-trip its preset. `--preset` became
/// optional at the clap layer only so VAAPI could omit it; the conversion must not
/// drop a preset an operator did supply.
#[test]
fn a_supplied_preset_survives_the_conversion() {
    let fields = VideoProfileFields {
        name: "home-hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: Some(20),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("slow".to_owned()),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        output_container: "mkv".to_owned(),
        copy_compatible: false,
        decode: VideoDecodeBackendArg::Software,
    };

    let new = NewVideoProfile::from(fields);

    assert_eq!(new.preset.as_deref(), Some("slow"));
    assert_eq!(new.crf, Some(20));
    assert!(new.qp.is_none());
    assert!(new.decode.is_software());
}

/// `hardware_backend` evidence starts here: a stored VAAPI profile reports its `qp`
/// and no preset, so an operator reading `profile show` sees the quality knob the
/// encode actually used rather than a preset `hevc_vaapi` has no flag for.
#[test]
fn profile_data_reports_a_vaapi_profiles_qp_and_omits_its_preset() {
    let profile = VideoProfile {
        id: "vp-gpu-vaapi-hevc".to_owned(),
        name: "gpu-vaapi-hevc".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "hevc_vaapi".to_owned(),
        crf: None,
        cq: None,
        qp: Some(23),
        bitrate_kbps: None,
        preset: None,
        tune: None,
        codec_profile: Some("main10".to_owned()),
        codec_level: None,
        pixel_format: Some("p010".to_owned()),
        max_width: None,
        max_height: None,
        output_container: "mkv".to_owned(),
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::vaapi(),
        retired_at: None,
    };

    let data = ProfileData::from(profile);

    assert_eq!(data.qp, Some(23));
    assert!(data.preset.is_none());
    assert!(data.crf.is_none());
    assert!(data.cq.is_none());
    assert!(data.decode.is_vaapi());
    let json = serde_json::to_value(&data).unwrap();
    assert!(
        json.get("preset").is_none(),
        "the serialized envelope must omit `preset`, not emit null: {json}"
    );
    assert_eq!(json["qp"], 23);
    assert_eq!(json["decode"]["backend"], "vaapi");
}
