use super::*;

#[test]
fn remux_container_vocab_is_case_insensitive() {
    assert_eq!(REMUX_CONTAINER_MKV, "mkv");
    assert!(is_supported_remux_container("mkv"));
    assert!(is_supported_remux_container("MKV"));
    assert!(!is_supported_remux_container("mp4"));
}

#[test]
fn remux_track_group_uses_snake_case_json() {
    let value = serde_json::to_value(RemuxTrackGroup::Subtitle).unwrap();
    assert_eq!(value, "subtitle");

    let parsed: RemuxTrackGroup = serde_json::from_str("\"attachment\"").unwrap();
    assert_eq!(parsed, RemuxTrackGroup::Attachment);
}
