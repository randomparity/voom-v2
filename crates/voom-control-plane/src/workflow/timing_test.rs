use super::timing::{branch_codec, seeded_timing};

#[test]
fn default_seed_two_codec_fixture_exercises_both_transform_paths() {
    assert_eq!(branch_codec(2, "file-000"), "h265");
    assert_eq!(branch_codec(2, "file-001"), "h264");
    assert_eq!(branch_codec(2, "file-002"), "h265");
}

#[test]
fn seeded_timing_is_reproducible_and_seed_sensitive() {
    let first = seeded_timing(2, "probe", "file-001", 25, 10);
    let second = seeded_timing(2, "probe", "file-001", 25, 10);
    let different_seed = seeded_timing(3, "probe", "file-001", 25, 10);

    assert_eq!(first, second);
    assert_ne!(first, different_seed);
    assert!((25..=35).contains(&first.duration_ms));
    assert!((1..=first.duration_ms).contains(&first.progress_interval_ms));
}
