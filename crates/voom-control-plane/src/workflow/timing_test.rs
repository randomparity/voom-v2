use super::timing::branch_codec;

#[test]
fn default_seed_two_codec_fixture_exercises_both_transform_paths() {
    assert_eq!(branch_codec(2, "file-000"), "h265");
    assert_eq!(branch_codec(2, "file-001"), "h264");
    assert_eq!(branch_codec(2, "file-002"), "h265");
}
