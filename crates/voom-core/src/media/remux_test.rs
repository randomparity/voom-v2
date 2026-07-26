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

#[test]
fn font_attachment_mime_vocab_is_exact() {
    for mime_type in [
        "font/sfnt",
        "font/ttf",
        "font/otf",
        "font/collection",
        "font/woff",
        "font/woff2",
        "application/x-truetype-font",
        "application/x-font-ttf",
        "application/vnd.ms-opentype",
        "application/font-sfnt",
        "application/font-woff",
    ] {
        assert!(is_font_attachment_mime_type(mime_type), "{mime_type}");
    }
    for mime_type in [
        "application/octet-stream",
        "application/font",
        "font/ttf; charset=binary",
        "FONT/TTF",
        "",
    ] {
        assert!(!is_font_attachment_mime_type(mime_type), "{mime_type}");
    }
}
