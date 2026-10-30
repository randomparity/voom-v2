use super::*;

const HASH: &str = "blake3:0000000000000000000000000000000000000000000000000000000000000000";

fn evidence(hash_source: Option<HashSource>) -> ScanObservationEvidence {
    ScanObservationEvidence {
        content_hash: HASH.to_owned(),
        size_bytes: 3,
        modified_at: "2026-10-30T00:00:00.000000000Z".to_owned(),
        file_key: Some(FileKeyFacts {
            dev: 1,
            ino: 2,
            nlink: 1,
        }),
        sidecars: Vec::new(),
        probe_snapshot: serde_json::json!({"format": "v1"}),
        hash_source,
    }
}

// G8: rows written with a recomputed hash must keep exactly the bytes the
// protocol-5 binary wrote, so a fleet downgrade can still read them.
#[test]
fn recomputed_evidence_encodes_without_hash_source() {
    let encoded = evidence(None).to_database_json().unwrap();
    assert_eq!(
        encoded,
        format!(
            "{{\"content_hash\":\"{HASH}\",\"size_bytes\":3,\
             \"modified_at\":\"2026-10-30T00:00:00.000000000Z\",\
             \"file_key\":{{\"dev\":1,\"ino\":2,\"nlink\":1}},\
             \"probe_snapshot\":{{\"format\":\"v1\"}}}}"
        )
    );
}

#[test]
fn reused_evidence_round_trips_through_the_database_form() {
    let encoded = evidence(Some(HashSource::Reused))
        .to_database_json()
        .unwrap();
    assert!(
        encoded.ends_with(",\"hash_source\":\"reused\"}"),
        "{encoded}"
    );
    assert_eq!(
        ScanObservationEvidence::parse_database(&encoded).unwrap(),
        evidence(Some(HashSource::Reused))
    );
}

#[test]
fn unknown_hash_source_is_a_database_error() {
    let encoded = format!(
        "{{\"content_hash\":\"{HASH}\",\"size_bytes\":3,\"modified_at\":\"t\",\
         \"probe_snapshot\":{{}},\"hash_source\":\"sampled\"}}"
    );
    assert!(ScanObservationEvidence::parse_database(&encoded).is_err());
}
