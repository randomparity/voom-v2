use super::*;

fn sample_settings() -> voom_policy::VideoProfileSettings {
    voom_policy::VideoProfileSettings {
        encoder: "libsvtav1".to_owned(),
        crf: Some(30),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("8".to_owned()),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        output_container: None,
        copy_compatible: None,
        decode: voom_core::VideoDecodeMode::default(),
    }
}

#[test]
fn inline_hash_is_stable_across_serde_round_trip() {
    let settings = sample_settings(); // libsvtav1, crf 30, preset 8
    let h1 = inline_profile_id(&settings);
    let json = serde_json::to_string(&settings).unwrap();
    let back: voom_policy::VideoProfileSettings = serde_json::from_str(&json).unwrap();
    let h2 = inline_profile_id(&back);
    assert_eq!(h1, h2);
    assert!(h1.starts_with("inline-"));
    assert_eq!(h1.len(), "inline-".len() + 12);
}

#[test]
fn inline_hash_differs_for_near_identical_profiles() {
    let mut a = sample_settings();
    a.crf = Some(22);
    let mut b = sample_settings();
    b.crf = Some(23);
    assert_ne!(inline_profile_id(&a), inline_profile_id(&b));
}

/// `bitrate_kbps` is a quality knob like `crf` and `qp`, so two profiles differing
/// only in it are different profiles. Omitting it from the canonical form made them
/// share one `inline-<hash>` — a durable identity that names staged artifacts.
#[test]
fn inline_hash_differs_for_profiles_differing_only_in_bitrate() {
    let mut a = sample_settings();
    a.encoder = "hevc_videotoolbox".to_owned();
    a.crf = None;
    a.preset = Some("default".to_owned());
    a.bitrate_kbps = Some(6000);
    let mut b = a.clone();
    b.bitrate_kbps = Some(12000);

    assert_ne!(inline_profile_id(&a), inline_profile_id(&b));
}

#[test]
fn inline_hash_normalizes_codec_level_case_and_whitespace() {
    let mut a = sample_settings();
    a.codec_level = Some(" 5.1 ".to_owned());
    let mut b = sample_settings();
    b.codec_level = Some("5.1".to_owned());
    assert_eq!(inline_profile_id(&a), inline_profile_id(&b));
}

#[test]
fn inline_hash_stable_across_omitted_vs_default_optionals() {
    let omitted = sample_settings(); // output_container: None, copy_compatible: None
    let mut defaulted = sample_settings();
    defaulted.output_container = Some("mkv".to_owned());
    defaulted.copy_compatible = Some(false);
    assert_eq!(
        inline_profile_id(&omitted),
        inline_profile_id(&defaulted),
        "omitted optionals must resolve to the same inline id as the explicit defaults"
    );
}

/// The `inline-<hash>` id is a durable identity: it names staged artifacts and appears
/// in plan output, so making `preset` optional and adding `qp` must not move it for any
/// pre-#409 profile. The expected value is derived here from the canonical string the
/// pre-#409 code produced, written out independently of the current implementation so
/// the assertion is not a tautology over it.
#[test]
fn inline_hash_is_unchanged_for_a_pre_409_software_profile() {
    let canonical = "encoder=libsvtav1;crf=30;preset=8;output_container=mkv;copy_compatible=false";
    let expected = format!(
        "inline-{}",
        &blake3::hash(canonical.as_bytes()).to_hex()[..12]
    );

    assert_eq!(inline_profile_id(&sample_settings()), expected);
}

/// A qp-domain profile contributes a `qp=` part and no `preset=` part at all, rather
/// than a placeholder for the speed knob `hevc_vaapi` does not have.
#[test]
fn inline_hash_covers_qp_and_omits_an_absent_preset() {
    let mut vaapi = sample_settings();
    vaapi.encoder = "hevc_vaapi".to_owned();
    vaapi.crf = None;
    vaapi.qp = Some(24);
    vaapi.preset = None;
    vaapi.decode = voom_core::VideoDecodeMode::vaapi();
    let canonical =
        "encoder=hevc_vaapi;qp=24;output_container=mkv;copy_compatible=false;decode=vaapi";
    let expected = format!(
        "inline-{}",
        &blake3::hash(canonical.as_bytes()).to_hex()[..12]
    );

    assert_eq!(inline_profile_id(&vaapi), expected);

    let mut other_qp = vaapi.clone();
    other_qp.qp = Some(25);
    assert_ne!(inline_profile_id(&vaapi), inline_profile_id(&other_qp));
}

#[test]
fn cpu_cost_lookup_is_deterministic() {
    assert_eq!(cpu_cost("libx265", "placebo"), "high");
    assert_eq!(cpu_cost("libx265", "medium"), "medium");
    assert_eq!(cpu_cost("libaom-av1", "0"), "high");
    assert_eq!(cpu_cost("libsvtav1", "8"), "low");
}
