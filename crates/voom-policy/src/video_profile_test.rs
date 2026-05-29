use super::*;

#[test]
fn deserializes_legacy_bare_string_as_named() {
    let r: VideoProfileRef = serde_json::from_str("\"default-hevc\"").unwrap();
    assert_eq!(r, VideoProfileRef::Named("default-hevc".to_owned()));
}

#[test]
fn deserializes_tagged_named() {
    let r: VideoProfileRef = serde_json::from_str(r#"{"named":"hevc-archive"}"#).unwrap();
    assert_eq!(r, VideoProfileRef::Named("hevc-archive".to_owned()));
}

#[test]
fn deserializes_tagged_inline() {
    let json = r#"{"inline":{"encoder":"libsvtav1","crf":28,"preset":"6"}}"#;
    let r: VideoProfileRef = serde_json::from_str(json).unwrap();
    match r {
        VideoProfileRef::Inline(s) => {
            assert_eq!(s.encoder, "libsvtav1");
            assert_eq!(s.crf, 28);
            assert_eq!(s.preset, "6");
            assert!(s.tune.is_none());
        }
        VideoProfileRef::Named(_) => panic!("expected inline"),
    }
}

#[test]
fn new_named_serializes_tagged_and_round_trips() {
    let r = VideoProfileRef::Named("default-av1".to_owned());
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(json, r#"{"named":"default-av1"}"#);
    let back: VideoProfileRef = serde_json::from_str(&json).unwrap();
    assert_eq!(r, back);
}
