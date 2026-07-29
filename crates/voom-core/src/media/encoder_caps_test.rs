use super::*;

#[test]
fn descriptor_lookup_knows_supported_encoders() {
    assert!(encoder_descriptor("libx265").is_some());
    assert!(encoder_descriptor("libsvtav1").is_some());
    assert!(encoder_descriptor("libaom-av1").is_some());
    assert!(encoder_descriptor("hevc_nvenc").is_some());
    assert!(encoder_descriptor("av1_nvenc").is_none());
    assert!(encoder_descriptor("x264").is_none());
}

#[test]
fn nvidia_descriptor_uses_cq_and_current_ffmpeg_vocabulary() {
    let nvidia = encoder_descriptor("hevc_nvenc").unwrap();
    assert_eq!(nvidia.quality_domain, QualityDomain::Cq { min: 1, max: 51 });
    assert!(nvidia.accepts_preset("p1"));
    assert!(nvidia.accepts_preset("p7"));
    assert!(!nvidia.accepts_preset("slow"));
    assert!(nvidia.accepts_tune("uhq"));
    assert!(nvidia.accepts_codec_profile("main10"));
    assert!(nvidia.accepts_codec_level("6.2"));
    assert!(nvidia.accepts_pixel_format("yuv420p10le"));
    assert!(!nvidia.accepts_pixel_format("yuv444p"));
}

#[test]
fn descriptor_crf_ranges_are_enforced() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(x265.accepts_crf(0));
    assert!(x265.accepts_crf(51));
    assert!(!x265.accepts_crf(52));

    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert!(svt.accepts_crf(63));
    assert!(!svt.accepts_crf(64));
}

#[test]
fn descriptor_preset_domains_accept_named_or_numeric_tokens() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(x265.accepts_preset("medium"));
    assert!(!x265.accepts_preset("13"));

    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert!(svt.accepts_preset("13"));
    assert!(!svt.accepts_preset("14"));
    assert!(!svt.accepts_preset("fast"));
}

#[test]
fn descriptor_optional_vocab_is_checked() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(x265.accepts_tune("grain"));
    assert!(!x265.accepts_tune("film"));
    assert!(x265.accepts_codec_profile("main10"));
    assert!(!x265.accepts_codec_profile("high"));

    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert!(svt.accepts_pixel_format("yuv420p10le"));
    assert!(!svt.accepts_pixel_format("yuv444p"));

    let aom = encoder_descriptor("libaom-av1").unwrap();
    assert!(aom.requires_bitrate_zero);
}

#[test]
fn descriptor_rejects_ten_bit_pixel_format_for_eight_bit_profile() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(!x265.pixel_format_compatible_with_profile("yuv420p10le", Some("main")));
    assert!(x265.pixel_format_compatible_with_profile("yuv420p", Some("main")));
    assert!(x265.pixel_format_compatible_with_profile("yuv420p10le", Some("main10")));
    assert!(x265.pixel_format_compatible_with_profile("yuv420p10le", None));
}

#[test]
fn av1_profiles_allow_declared_ten_bit_formats() {
    let aom = encoder_descriptor("libaom-av1").unwrap();
    let svt = encoder_descriptor("libsvtav1").unwrap();

    assert!(aom.pixel_format_compatible_with_profile("yuv420p10le", Some("main")));
    assert!(svt.pixel_format_compatible_with_profile("yuv420p10le", Some("main")));
}
