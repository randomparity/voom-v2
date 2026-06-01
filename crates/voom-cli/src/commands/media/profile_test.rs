use voom_store::repo::video_profiles::VideoProfile;

use super::ProfileData;

#[test]
fn profile_data_maps_every_field_from_video_profile() {
    let profile = VideoProfile {
        id: "vp-hevc-archive".to_owned(),
        name: "hevc-archive".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: 18,
        preset: "slow".to_owned(),
        tune: Some("grain".to_owned()),
        codec_profile: Some("main10".to_owned()),
        codec_level: Some("5.1".to_owned()),
        pixel_format: Some("yuv420p10le".to_owned()),
        max_width: Some(1920),
        max_height: Some(1080),
        output_container: "mkv".to_owned(),
        copy_compatible: true,
    };

    let data = ProfileData::from(profile);

    assert_eq!(data.id, "vp-hevc-archive");
    assert_eq!(data.name, "hevc-archive");
    assert_eq!(data.target_codec, "hevc");
    assert_eq!(data.encoder, "libx265");
    assert_eq!(data.crf, 18);
    assert_eq!(data.preset, "slow");
    assert_eq!(data.tune.as_deref(), Some("grain"));
    assert_eq!(data.codec_profile.as_deref(), Some("main10"));
    assert_eq!(data.codec_level.as_deref(), Some("5.1"));
    assert_eq!(data.pixel_format.as_deref(), Some("yuv420p10le"));
    assert_eq!(data.max_width, Some(1920));
    assert_eq!(data.max_height, Some(1080));
    assert_eq!(data.output_container, "mkv");
    assert!(data.copy_compatible);
}
