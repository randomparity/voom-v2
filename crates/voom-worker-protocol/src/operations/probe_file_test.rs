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
        guard: None,
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

fn expected() -> ExpectedFileFacts {
    ExpectedFileFacts {
        size_bytes: 12,
        content_hash: "blake3:012345".to_owned(),
        modified_at: Some("2026-05-24T00:00:00.000000000Z".to_owned()),
        local_file_key: None,
    }
}

fn guard() -> ProbeGuard {
    ProbeGuard {
        changed_at_ns: 1_793_000_000_123_456_789,
        file_key: FileKeyFacts {
            dev: 1,
            ino: 2,
            nlink: 1,
        },
    }
}

// Output, media-child and setting-off probes send no guard, and recomputed
// results carry no source, so both keep their protocol-5 bytes (G8).
#[test]
fn unguarded_request_and_recomputed_result_keep_protocol_5_shape() {
    let request = serde_json::to_value(ProbeFileRequest {
        path: "/media/movie.mkv".to_owned(),
        expected: expected(),
        guard: None,
    })
    .unwrap();
    assert!(request.get("guard").is_none(), "{request}");

    let facts = ObservedFileFacts {
        size_bytes: 12,
        content_hash: "blake3:012345".to_owned(),
        modified_at: None,
        local_file_key: None,
    };
    let result = serde_json::to_value(ProbeFileResult {
        status: ProbeFileStatus::Probed,
        provider: "ffprobe".to_owned(),
        provider_version: "7.0".to_owned(),
        pre_probe: facts.clone(),
        post_probe: facts,
        snapshot: serde_json::json!({}),
        hash_source: None,
    })
    .unwrap();
    assert!(result.get("hash_source").is_none(), "{result}");
    assert_eq!(
        serde_json::from_value::<ProbeFileResult>(result)
            .unwrap()
            .hash_source,
        None
    );
}

#[test]
fn guarded_request_and_reused_result_round_trip() {
    let request = ProbeFileRequest {
        path: "/media/movie.mkv".to_owned(),
        expected: expected(),
        guard: Some(guard()),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(
        json["guard"],
        serde_json::json!({
            "changed_at_ns": 1_793_000_000_123_456_789_i64,
            "file_key": {"dev": 1, "ino": 2, "nlink": 1}
        })
    );
    assert_eq!(
        serde_json::from_value::<ProbeFileRequest>(json).unwrap(),
        request
    );

    let result = serde_json::from_value::<ProbeFileResult>(serde_json::json!({
        "status": "probed",
        "provider": "ffprobe",
        "provider_version": "7.0",
        "pre_probe": { "size_bytes": 1, "content_hash": "blake3:aa" },
        "post_probe": { "size_bytes": 1, "content_hash": "blake3:aa" },
        "snapshot": {},
        "hash_source": "reused"
    }))
    .unwrap();
    assert_eq!(result.hash_source, Some(HashSource::Reused));
}

#[test]
fn guard_rejects_unknown_fields() {
    let err = serde_json::from_value::<ProbeGuard>(serde_json::json!({
        "changed_at_ns": 1,
        "file_key": {"dev": 1, "ino": 2, "nlink": 1},
        "mtime_ns": 1
    }))
    .unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");
}
