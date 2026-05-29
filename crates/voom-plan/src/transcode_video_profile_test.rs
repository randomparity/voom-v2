use super::*;

fn sample_settings() -> voom_policy::VideoProfileSettings {
    voom_policy::VideoProfileSettings {
        encoder: "libsvtav1".to_owned(),
        crf: 30,
        preset: "8".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        output_container: None,
        copy_compatible: None,
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
    a.crf = 22;
    let mut b = sample_settings();
    b.crf = 23;
    assert_ne!(inline_profile_id(&a), inline_profile_id(&b));
}

#[test]
fn cpu_cost_lookup_is_deterministic() {
    assert_eq!(cpu_cost("libx265", "placebo"), "high");
    assert_eq!(cpu_cost("libx265", "medium"), "medium");
    assert_eq!(cpu_cost("libaom-av1", "0"), "high");
    assert_eq!(cpu_cost("libsvtav1", "8"), "low");
}
