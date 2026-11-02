//! Unit coverage for the byte-free envelope assembly helpers.

use super::*;
use serde_json::json;

#[tokio::test]
async fn snapshot_for_keeps_pin_after_refresh() {
    let (cp, _tmp) = crate::cases::cp().await;
    let outcome = cp
        .record_discovered_file(
            voom_store::repo::media::identity::DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_core::ProviderRelativeLocator::new(
                    "pin.mp4".to_owned(),
                )
                .unwrap(),
                content_hash: "hash-pinned-file".to_owned(),
                size_bytes: 1024,
                observed_at: time::OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let voom_store::repo::media::identity::IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = outcome
    else {
        panic!("expected a new file asset");
    };
    let original = cp
        .record_media_snapshot(
            file_version_id,
            None,
            json!({"provider_generation": 1}),
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    let refreshed = cp
        .record_media_snapshot(
            file_version_id,
            None,
            json!({"provider_generation": 2}),
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    assert_ne!(original.id, refreshed.id);

    let loaded = snapshot_for(
        &cp,
        file_version_id,
        &json!({"source_media_snapshot_id": original.id.0}),
    )
    .await
    .unwrap();

    assert_eq!(
        loaded.id, original.id,
        "an existing operation retains its snapshot pin"
    );
    assert_eq!(loaded.file_version_id, original.file_version_id);
    assert_eq!(loaded.payload, original.payload);
}

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
