use super::*;

#[test]
fn parses_supported_mkvmerge_version() {
    let version = parse_mkvmerge_version("mkvmerge v80.0 ('Roundabout') 64-bit").unwrap();

    assert_eq!(version.major, 80);
}

#[test]
fn rejects_unsupported_mkvmerge_version() {
    let err = parse_mkvmerge_version("mkvmerge v40.0").unwrap_err();

    assert!(err.to_string().contains("unsupported mkvmerge version"));
}
