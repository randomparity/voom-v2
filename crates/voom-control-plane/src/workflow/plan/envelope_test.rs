//! Unit coverage for the byte-free envelope assembly helpers.

use super::*;
use serde_json::json;

#[test]
fn source_facts_read_scan_recorded_blocks_and_reject_gaps() {
    let facts = SourceFacts::from_source_file(&json!({
        "path": "/library/a.mkv",
        "size_bytes": 21_u64,
        "content_hash": "blake3:abc",
    }))
    .unwrap();
    assert_eq!(facts.size_bytes, 21);
    assert_eq!(facts.content_hash, "blake3:abc");

    assert!(SourceFacts::from_source_file(&json!({ "path": "/library/a.mkv" })).is_err());
    assert!(SourceFacts::from_source_file(&json!("not-an-object")).is_err());
}

#[test]
fn source_facts_pin_version_rows_without_optional_hints() {
    let version_facts = SourceFacts {
        size_bytes: 7,
        content_hash: "blake3:x".to_owned(),
    };
    let file = version_facts.clone().file();
    assert_eq!(file.size_bytes, 7);
    assert_eq!(file.modified_at, None);
    assert_eq!(version_facts.clone().audio().size_bytes, 7);
    assert_eq!(version_facts.clone().remux().size_bytes, 7);
    assert_eq!(version_facts.video().size_bytes, 7);
}

#[test]
fn parent_envelope_output_reads_single_planned_outputs() {
    let rendered = json!({
        "media_dispatch": {
            "operation": "transcode_video",
            "output": {
                "storage_root_id": 5_u64,
                "provider_relative_locator": "branch-a/file.mkv",
                "overwrite": false,
            },
        },
    });
    let (root, locator) = parent_envelope_output(&rendered).unwrap();
    assert_eq!(root, StorageRootId(5));
    assert_eq!(locator.as_str(), "branch-a/file.mkv");

    let destination = json!({
        "media_dispatch": {
            "operation": "back_up_file",
            "destination": {
                "storage_root_id": 9_u64,
                "provider_relative_locator": "v42/movie.mkv",
                "overwrite": false,
            },
        },
    });
    let (root, locator) = parent_envelope_output(&destination).unwrap();
    assert_eq!(root, StorageRootId(9));
    assert_eq!(locator.as_str(), "v42/movie.mkv");

    assert!(parent_envelope_output(&json!({})).is_none());
}

#[test]
fn observed_output_facts_read_the_first_reported_output() {
    let result = json!({
        "agent_observed": {
            "outputs": [
                {
                    "provider_relative_locator": "b/branch/out.mkv",
                    "facts": { "size_bytes": 33_u64, "content_hash": "blake3:out" },
                },
            ],
        },
    });
    let facts = observed_output_facts(&result).unwrap();
    assert_eq!(facts.size_bytes, 33);
    assert_eq!(facts.content_hash, "blake3:out");
    // The staged-output fact shape the T8 verify chain will pin.
    let expected = verify_facts(facts);
    assert_eq!(expected.size_bytes, 33);
    assert_eq!(expected.local_file_key, None);

    assert!(observed_output_facts(&json!({})).is_none());
}
