use super::*;

#[test]
fn descriptor_lookup_knows_supported_encoders() {
    assert!(encoder_descriptor("libx265").is_some());
    assert!(encoder_descriptor("libsvtav1").is_some());
    assert!(encoder_descriptor("libaom-av1").is_some());
    assert!(encoder_descriptor("hevc_nvenc").is_some());
    assert!(encoder_descriptor("hevc_vaapi").is_some());
    assert!(encoder_descriptor("av1_nvenc").is_none());
    assert!(encoder_descriptor("av1_vaapi").is_none());
    assert!(encoder_descriptor("h264_vaapi").is_none());
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

/// The descriptor is the only thing standing between an operator profile and an
/// `FFmpeg` invocation, so it must spell `hevc_vaapi`'s vocabulary exactly as the
/// acceptance host reported it (design §2.2): `-qp 0..52` where 0 means auto, so the
/// operator range starts at 1; no `-preset` and no `-compression_level` exist on this
/// encoder; `-level` is deliberately not offered in this slice (ADR 0051 §4); and the
/// only surface formats are the hardware ones, `nv12` and `p010`. Any widening here
/// lets validation admit a profile the encoder then rejects mid-run.
#[test]
fn vaapi_descriptor_matches_the_probed_encoder_vocabulary() {
    let vaapi = encoder_descriptor("hevc_vaapi").unwrap();

    assert_eq!(vaapi.target_codec, "hevc");
    assert_eq!(vaapi.backend, VideoEncoderBackend::Vaapi);
    assert_eq!(vaapi.quality_domain, QualityDomain::Qp { min: 1, max: 52 });
    assert_eq!(vaapi.preset_domain, PresetDomain::None);
    assert_eq!(vaapi.tunes, &[] as &[&str]);
    assert_eq!(vaapi.codec_profiles, &["main", "main10"]);
    assert_eq!(vaapi.codec_levels, &[] as &[&str]);
    assert_eq!(vaapi.pixel_formats, &["nv12", "p010"]);
    assert_eq!(vaapi.ten_bit_pixel_formats, &["p010"]);
    assert_eq!(vaapi.eight_bit_only_profiles, &["main"]);
    assert!(!vaapi.requires_bitrate_zero);
}

/// Each encoder answers only for its own quality knob. `qp` is not `crf` and not `cq`,
/// so a profile cannot smuggle a quality target past the descriptor by spelling it with
/// a field the encoder does not have.
#[test]
fn quality_domains_do_not_answer_for_each_other() {
    let vaapi = encoder_descriptor("hevc_vaapi").unwrap();
    assert!(!vaapi.accepts_qp(0));
    assert!(vaapi.accepts_qp(1));
    assert!(vaapi.accepts_qp(52));
    assert!(!vaapi.accepts_qp(53));
    assert!(!vaapi.accepts_crf(23));
    assert!(!vaapi.accepts_cq(23));

    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(!x265.accepts_qp(23));
    let nvidia = encoder_descriptor("hevc_nvenc").unwrap();
    assert!(!nvidia.accepts_qp(23));
}

/// `PresetDomain::None` is not "any preset is fine" — it is "this encoder has no speed
/// knob at all", so every token must be refused rather than passed through to a flag
/// `hevc_vaapi` does not accept.
#[test]
fn preset_domain_none_accepts_no_token() {
    let vaapi = encoder_descriptor("hevc_vaapi").unwrap();
    assert!(!vaapi.accepts_preset("medium"));
    assert!(!vaapi.accepts_preset("0"));
    assert!(!vaapi.accepts_preset(""));
}

/// VAAPI decode is selected by `-hwaccel vaapi` plus the codec's own decoder, so the
/// supported set is a flat codec list. CUVID needs `NVIDIA_VIDEO_DECODERS`' pairs only
/// because it has a distinct decoder name per codec; a VAAPI pair would repeat the same
/// string twice (design §3).
#[test]
fn vaapi_decoders_are_a_flat_codec_list() {
    assert_eq!(VAAPI_VIDEO_DECODERS, &["h264", "hevc", "av1"]);
}

#[test]
fn nvidia_decoder_mapping_accepts_supported_codecs_and_hevc_alias() {
    assert_eq!(nvidia_decoder_for_video_codec("h264"), Some("h264_cuvid"));
    assert_eq!(nvidia_decoder_for_video_codec("H265"), Some("hevc_cuvid"));
    assert_eq!(nvidia_decoder_for_video_codec("av1"), Some("av1_cuvid"));
    assert_eq!(nvidia_decoder_for_video_codec("vp9"), None);
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

/// `expected_output_pixel_format` may only fail when a surface format was added without
/// recording what it writes, so every encoder's declared `pixel_formats` must be mapped.
/// Without this, adding a surface would turn a conforming encode into a hard failure at
/// the worker and a never-converging plan.
#[test]
fn every_declared_pixel_format_has_a_recorded_output_format() {
    for encoder in [
        "libx265",
        "libsvtav1",
        "libaom-av1",
        "hevc_nvenc",
        "hevc_vaapi",
    ] {
        let descriptor = encoder_descriptor(encoder).unwrap();
        for pixel_format in descriptor.pixel_formats {
            assert!(
                descriptor.output_pixel_format(pixel_format).is_some(),
                "`{encoder}` records no output format for surface `{pixel_format}`"
            );
        }
        for (surface, _) in descriptor.surface_output_pixel_formats {
            assert!(
                descriptor.pixel_formats.contains(surface),
                "`{encoder}` maps `{surface}`, which it does not accept"
            );
        }
    }
}

/// A software encoder's `pixel_formats` are already file formats, so the mapping is the
/// identity; a hardware encoder's are surfaces and must translate (design §2.2).
#[test]
fn surface_formats_translate_only_for_hardware_encoders() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert_eq!(x265.output_pixel_format("yuv420p10le"), Some("yuv420p10le"));

    let vaapi = encoder_descriptor("hevc_vaapi").unwrap();
    assert_eq!(vaapi.output_pixel_format("nv12"), Some("yuv420p"));
    assert_eq!(vaapi.output_pixel_format("p010"), Some("yuv420p10le"));
    assert_eq!(vaapi.output_pixel_format("yuv420p"), None);
}
