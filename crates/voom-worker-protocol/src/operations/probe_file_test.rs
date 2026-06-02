use super::*;

#[test]
fn probe_request_serializes_stable_snake_case_shape() {
    let req = ProbeFileRequest {
        path: "/media/movie.mkv".to_owned(),
        expected: ExpectedFileFacts {
            size_bytes: 12,
            content_hash: "blake3:012345".to_owned(),
            modified_at: Some("2026-05-24T00:00:00Z".to_owned()),
            local_file_key: Some("dev=1,ino=2".to_owned()),
        },
    };

    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(json["path"], "/media/movie.mkv");
    assert_eq!(json["expected"]["size_bytes"], 12);
    assert_eq!(json["expected"]["content_hash"], "blake3:012345");
    assert_eq!(json["expected"]["modified_at"], "2026-05-24T00:00:00Z");
    assert_eq!(json["expected"]["local_file_key"], "dev=1,ino=2");
}

#[test]
fn probe_result_requires_known_status() {
    let err = serde_json::from_value::<ProbeFileResult>(serde_json::json!({
        "status": "made_up",
        "provider": "ffprobe",
        "provider_version": "7.0",
        "pre_probe": { "size_bytes": 1, "content_hash": "blake3:aa" },
        "post_probe": { "size_bytes": 1, "content_hash": "blake3:aa" },
        "snapshot": { "format": "sprint10-v1" }
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown variant"));
}
