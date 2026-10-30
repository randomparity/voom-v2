use super::*;

use serde_json::json;

fn result() -> HashFileResult {
    HashFileResult {
        content_hash: "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            .to_owned(),
        size_bytes: 42,
        modified_at: "2026-08-22T00:00:00Z".to_owned(),
        file_key: Some(FileKeyFacts {
            dev: 1,
            ino: 2,
            nlink: 1,
        }),
        changed_at_ns: None,
        stability_started_at: "2026-08-22T00:00:00Z".to_owned(),
        stability_confirmed_at: "2026-08-22T00:00:01Z".to_owned(),
        sidecars: vec![HashedSidecar {
            provider_relative_locator: "a.srt".to_owned(),
            role: "external_subtitle".to_owned(),
            blake3_hex: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_owned(),
            size_bytes: 7,
        }],
    }
}

#[test]
fn result_round_trips_with_file_key_and_sidecars() {
    let encoded = serde_json::to_value(result()).unwrap();
    assert_eq!(
        serde_json::from_value::<HashFileResult>(encoded).unwrap(),
        result()
    );
}

#[test]
fn result_rejects_unknown_fields() {
    let mut unknown = serde_json::to_value(result()).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("drifted_facts".to_owned(), json!(true));
    assert!(serde_json::from_value::<HashFileResult>(unknown).is_err());
}

#[test]
fn request_round_trips_and_rejects_unknown_fields() {
    let request = HashFileRequest {
        provider_locator: "/media/library".to_owned(),
        provider_relative_locator: "movie/movie.mkv".to_owned(),
    };
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<HashFileRequest>(encoded).unwrap(),
        request
    );
    let mut unknown = serde_json::to_value(&request).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("path".to_owned(), json!("/abs"));
    assert!(serde_json::from_value::<HashFileRequest>(unknown).is_err());
}

#[test]
fn empty_sidecar_list_omits_the_field_on_encode() {
    let mut bare = result();
    bare.sidecars = Vec::new();
    let encoded = serde_json::to_value(&bare).unwrap();
    assert!(encoded.get("sidecars").is_none());
}

#[test]
fn result_round_trips_changed_at_ns_and_omits_it_when_absent() {
    let with_ctime = HashFileResult {
        changed_at_ns: Some(1_793_000_000_123_456_789),
        ..result()
    };
    let encoded = serde_json::to_value(&with_ctime).unwrap();
    assert_eq!(
        encoded["changed_at_ns"],
        json!(1_793_000_000_123_456_789_i64)
    );
    assert_eq!(
        serde_json::from_value::<HashFileResult>(encoded).unwrap(),
        with_ctime
    );
    assert!(
        serde_json::to_value(result())
            .unwrap()
            .get("changed_at_ns")
            .is_none()
    );
}
