use super::*;

#[test]
fn descriptor_lookup_is_keyed_on_encoder() {
    assert!(encoder_descriptor("libx265").is_some());
    assert!(encoder_descriptor("libsvtav1").is_some());
    assert!(encoder_descriptor("libaom-av1").is_some());
    assert!(encoder_descriptor("x264").is_none());
}

#[test]
fn encoder_must_match_target_codec() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert_eq!(x265.target_codec, "hevc");
    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert_eq!(svt.target_codec, "av1");
}

#[test]
fn crf_range_is_per_encoder() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(x265.accepts_crf(0));
    assert!(x265.accepts_crf(23));
    assert!(x265.accepts_crf(51));
    assert!(!x265.accepts_crf(52));
    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert!(svt.accepts_crf(0));
    assert!(svt.accepts_crf(63));
    assert!(!svt.accepts_crf(64));
}

#[test]
fn preset_domain_is_named_for_x265_numeric_for_av1() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(x265.accepts_preset("medium"));
    assert!(x265.accepts_preset("placebo"));
    assert!(!x265.accepts_preset("8"));
    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert!(svt.accepts_preset("8"));
    assert!(svt.accepts_preset("0"));
    assert!(svt.accepts_preset("13"));
    assert!(!svt.accepts_preset("14"));
    assert!(!svt.accepts_preset("medium"));
    let aom = encoder_descriptor("libaom-av1").unwrap();
    assert!(aom.accepts_preset("4"));
    assert!(aom.accepts_preset("8"));
    assert!(!aom.accepts_preset("9"));
}

#[test]
fn pixel_format_and_profile_combinations_are_validated() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(x265.accepts_pixel_format("yuv420p"));
    assert!(x265.accepts_pixel_format("yuv420p10le"));
    assert!(!x265.accepts_pixel_format("rgb24"));
    assert!(x265.accepts_codec_profile("main10"));
    // 10-bit pixel format under an 8-bit-only codec profile is incompatible.
    assert!(x265.pixel_format_compatible_with_profile("yuv420p10le", Some("main10")));
    assert!(!x265.pixel_format_compatible_with_profile("yuv420p10le", Some("main")));
    assert!(x265.pixel_format_compatible_with_profile("yuv420p", Some("main")));
}

#[test]
fn libaom_requires_constant_quality_bitrate_zero() {
    let aom = encoder_descriptor("libaom-av1").unwrap();
    assert!(aom.requires_bitrate_zero);
    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert!(!svt.requires_bitrate_zero);
}
