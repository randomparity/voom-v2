use super::*;

use serde_json::json;

fn candidate(locator: &str) -> ScanCandidate {
    ScanCandidate {
        primary: primary_file(locator),
        sidecars: Vec::new(),
    }
}

fn primary_file(locator: &str) -> ScanCandidateFile {
    ScanCandidateFile {
        provider_relative_locator: locator.to_owned(),
        provider_object_identity: "dev=1;ino=2".to_owned(),
        size_bytes: 10,
        modified_at: "2026-08-22T00:00:00Z".to_owned(),
        kind: None,
    }
}

#[test]
fn request_round_trips_and_rejects_unknown_fields() {
    let request = ScanLibraryRequest {
        provider_locator: "/media/library".to_owned(),
        extension_allowlist: vec!["mkv".to_owned()],
    };
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<ScanLibraryRequest>(encoded).unwrap(),
        request
    );
    let mut unknown = serde_json::to_value(&request).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("surprise".to_owned(), json!(1));
    assert!(serde_json::from_value::<ScanLibraryRequest>(unknown).is_err());
}

#[test]
fn result_round_trips() {
    let result = ScanLibraryResult {
        discovered_count: 3,
        skipped_count: 1,
    };
    let decoded: ScanLibraryResult =
        serde_json::from_value(serde_json::to_value(result).unwrap()).unwrap();
    assert_eq!(decoded, result);
}

#[test]
fn progress_round_trip_preserves_candidates_in_order() {
    let candidates = vec![candidate("a.mkv"), candidate("b/b.mkv")];
    let payload = encode_candidate_progress(&candidates).unwrap();
    assert_eq!(decode_candidate_progress(&payload).unwrap(), candidates);
}

#[test]
fn progress_decode_rejects_unknown_fields_and_shapes() {
    assert!(decode_candidate_progress(&json!({})).is_err());
    assert!(decode_candidate_progress(&json!({ "candidates": {}, "extra": 1 })).is_err());
    assert!(decode_candidate_progress(&json!({ "candidates": [{}] })).is_err());
    let mut with_extra = json!({ "candidates": [] });
    with_extra
        .as_object_mut()
        .unwrap()
        .insert("trailing".to_owned(), json!(true));
    assert!(decode_candidate_progress(&with_extra).is_err());
}

#[test]
fn progress_encode_rejects_more_than_the_candidate_bound() {
    let oversized: Vec<ScanCandidate> = (0..=MAX_PROGRESS_CANDIDATES)
        .map(|index| candidate(&format!("f{index}.mkv")))
        .collect();
    let error = encode_candidate_progress(&oversized).unwrap_err();
    assert!(error.to_string().contains("candidate frame bound"));
}

#[test]
fn progress_encode_rejects_a_payload_over_the_byte_budget() {
    // One locator just over the 32 KiB budget in a single frame.
    let long_locator = "a".repeat(32 * 1024);
    let error = encode_candidate_progress(&[candidate(&long_locator)]).unwrap_err();
    assert!(error.to_string().contains("frame budget"));
}

#[test]
fn decode_rejects_an_oversized_serialized_payload_before_bounds() {
    let long_locator = "a".repeat(32 * 1024);
    let payload = json!({ "candidates": [candidate(&long_locator)] });
    assert!(decode_candidate_progress(&payload).is_err());
}

#[test]
fn sidecar_kind_rides_on_sidecar_entries_only() {
    let mut sidecar = primary_file("a.srt");
    sidecar.kind = Some("external_subtitle".to_owned());
    let group = ScanCandidate {
        primary: primary_file("a.mkv"),
        sidecars: vec![sidecar],
    };
    let payload = encode_candidate_progress(std::slice::from_ref(&group)).unwrap();
    assert_eq!(decode_candidate_progress(&payload).unwrap(), vec![group]);
}
