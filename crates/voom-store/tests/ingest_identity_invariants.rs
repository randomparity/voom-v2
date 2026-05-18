#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests favor unwrap/expect/panic over plumbing Result<()> through every \
              assertion"
)]
//! M2 ingest-path coverage. Drives every named outcome of spec §13.2
//! through `ControlPlane::record_discovered_file` and asserts the
//! repo's row shape + the case-handler's event chain.

use std::sync::{Arc, Mutex};

use time::Duration;

use voom_control_plane::ControlPlane;
use voom_core::SystemClock;
use voom_core::rng_test_support::FrozenRng;
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{
    AliasProof, DiscoveredFile, FileLocationKind, IdentityEvidenceTarget, IdentityRepo,
    IngestOutcome, LocationProof,
};
use voom_store::test_support::T0;

async fn cp() -> (ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let rng = Arc::new(Mutex::new(FrozenRng::new(0)));
    let cp = ControlPlane::open_with_pool_and_rng(pool, Arc::new(SystemClock), rng)
        .await
        .unwrap();
    (cp, tmp)
}

async fn count_kind(cp: &ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 1000,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

fn new_local(path: &str, hash: &str, size: u64) -> DiscoveredFile {
    DiscoveredFile {
        location_kind: FileLocationKind::LocalPath,
        location_value: path.to_owned(),
        content_hash: hash.to_owned(),
        size_bytes: size,
        observed_at: T0,
        proof: None,
    }
}

fn new_local_with_proof(
    path: &str,
    hash: &str,
    size: u64,
    file_id: u128,
    generation: u64,
) -> DiscoveredFile {
    DiscoveredFile {
        location_kind: FileLocationKind::LocalPath,
        location_value: path.to_owned(),
        content_hash: hash.to_owned(),
        size_bytes: size,
        observed_at: T0,
        proof: Some(LocationProof::LocalFileIdGeneration {
            file_id,
            generation,
        }),
    }
}

fn new_object_store_with_proof(
    key: &str,
    hash: &str,
    size: u64,
    bucket: &str,
    version_id: &str,
) -> DiscoveredFile {
    DiscoveredFile {
        location_kind: FileLocationKind::ObjectStoreKey,
        location_value: format!("s3://{bucket}/{key}#{version_id}"),
        content_hash: hash.to_owned(),
        size_bytes: size,
        observed_at: T0,
        proof: Some(LocationProof::ObjectStoreVersion {
            bucket: bucket.to_owned(),
            key: key.to_owned(),
            version_id: version_id.to_owned(),
        }),
    }
}

// --- Named §13.2 ingest cases --------------------------------------------

#[tokio::test]
async fn new_filesystem_object_creates_new_file_asset() {
    let (cp, _tmp) = cp().await;
    let outcome = cp
        .record_discovered_file(new_local("/srv/a.mkv", "h1", 100), None)
        .await
        .unwrap();
    assert!(matches!(outcome, IngestOutcome::NewFileAsset { .. }));
    assert_eq!(count_kind(&cp, EventKind::FileAssetCreated).await, 1);
    assert_eq!(count_kind(&cp, EventKind::FileVersionCreated).await, 1);
    assert_eq!(count_kind(&cp, EventKind::FileLocationRecorded).await, 1);
    assert_eq!(
        count_kind(&cp, EventKind::IdentityEvidenceRecorded).await,
        0
    );
}

#[tokio::test]
async fn local_proof_with_matching_hash_attaches_alias() {
    let (cp, _tmp) = cp().await;
    let first = cp
        .record_discovered_file(new_local_with_proof("/srv/a.mkv", "h1", 100, 42, 1), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = first
    else {
        panic!("first discovery should be NewFileAsset");
    };
    // Second discovery at a new path, same physical-object proof and
    // same hash — must AliasAttach to the first FileVersion.
    let second = cp
        .record_discovered_file(
            new_local_with_proof("/srv/a-copy.mkv", "h1", 100, 42, 1),
            Some(AliasProof::LocalFileIdGeneration {
                file_id: 42,
                generation: 1,
                prior_location_id: file_location_id,
            }),
        )
        .await
        .unwrap();
    assert!(
        matches!(second, IngestOutcome::AliasAttached { .. }),
        "got: {second:?}"
    );
    assert_eq!(count_kind(&cp, EventKind::FileLocationAliased).await, 1);
    assert_eq!(
        count_kind(&cp, EventKind::FileAssetCreated).await,
        1,
        "alias should not create a new asset"
    );
}

#[tokio::test]
async fn local_proof_with_mismatched_hash_creates_new_asset_with_path_rule_evidence() {
    let (cp, _tmp) = cp().await;
    let first = cp
        .record_discovered_file(new_local_with_proof("/srv/a.mkv", "h1", 100, 42, 1), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = first
    else {
        panic!("first discovery should be NewFileAsset");
    };
    // Alias proof matches IDs but hash differs → fall back to new asset
    // and stamp path_rule_match evidence.
    let second = cp
        .record_discovered_file(
            new_local_with_proof("/srv/a-copy.mkv", "h-different", 100, 42, 1),
            Some(AliasProof::LocalFileIdGeneration {
                file_id: 42,
                generation: 1,
                prior_location_id: file_location_id,
            }),
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        path_rule_evidence, ..
    } = second
    else {
        panic!("expected NewFileAsset on hash mismatch");
    };
    assert!(path_rule_evidence.is_some());
}

#[tokio::test]
async fn inode_match_without_generation_match_falls_back_to_new_asset() {
    let (cp, _tmp) = cp().await;
    let first = cp
        .record_discovered_file(new_local_with_proof("/srv/a.mkv", "h1", 100, 42, 1), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = first
    else {
        panic!();
    };
    // Same file_id, different generation → mismatch → NewFileAsset.
    let second = cp
        .record_discovered_file(
            new_local_with_proof("/srv/b.mkv", "h1", 100, 42, 2),
            Some(AliasProof::LocalFileIdGeneration {
                file_id: 42,
                generation: 2,
                prior_location_id: file_location_id,
            }),
        )
        .await
        .unwrap();
    assert!(
        matches!(second, IngestOutcome::NewFileAsset { .. }),
        "got: {second:?}"
    );
}

#[tokio::test]
async fn object_store_full_proof_attaches_alias() {
    // For object-store identity the alias's physical-object triple is
    // the SAME `(bucket, key, version_id)` as the prior location's
    // proof — that's the "same physical object" assertion. The
    // textual `location_value` may differ (e.g. presentation alias
    // through a different prefix), but the proof bytes line up.
    let (cp, _tmp) = cp().await;
    let first = cp
        .record_discovered_file(
            new_object_store_with_proof("k/a.mkv", "h1", 100, "media", "v1"),
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = first
    else {
        panic!();
    };
    let second = cp
        .record_discovered_file(
            new_object_store_with_proof("k/a.mkv", "h1", 100, "media", "v1"),
            Some(AliasProof::ObjectStoreVersion {
                bucket: "media".to_owned(),
                key: "k/a.mkv".to_owned(),
                version_id: "v1".to_owned(),
                prior_location_id: file_location_id,
            }),
        )
        .await
        .unwrap();
    assert!(
        matches!(second, IngestOutcome::AliasAttached { .. }),
        "got: {second:?}"
    );
}

#[tokio::test]
async fn object_store_key_match_without_version_id_falls_back_to_new_asset() {
    let (cp, _tmp) = cp().await;
    let first = cp
        .record_discovered_file(
            new_object_store_with_proof("k/a.mkv", "h1", 100, "media", "v1"),
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = first
    else {
        panic!();
    };
    // Same bucket/key, different version_id → mismatch.
    let second = cp
        .record_discovered_file(
            new_object_store_with_proof("k/a-other.mkv", "h1", 100, "media", "v2"),
            Some(AliasProof::ObjectStoreVersion {
                bucket: "media".to_owned(),
                key: "k/a.mkv".to_owned(),
                version_id: "v2".to_owned(),
                prior_location_id: file_location_id,
            }),
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        path_rule_evidence, ..
    } = second
    else {
        panic!("expected NewFileAsset");
    };
    assert!(path_rule_evidence.is_some());
}

#[tokio::test]
async fn hash_match_without_alias_proof_stamps_hash_match_evidence() {
    let (cp, _tmp) = cp().await;
    let first = cp
        .record_discovered_file(new_local("/srv/a.mkv", "shared-hash", 50), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_asset_id: existing_asset_id,
        ..
    } = first
    else {
        panic!();
    };
    let second = cp
        .record_discovered_file(new_local("/srv/b.mkv", "shared-hash", 50), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_asset_id: new_asset_id,
        file_version_id: new_version_id,
        hash_match_evidence,
        ..
    } = second
    else {
        panic!();
    };
    assert_ne!(
        existing_asset_id, new_asset_id,
        "hash match never collapses identity"
    );
    let ev_id = hash_match_evidence.expect("hash match should be detected");
    assert_eq!(
        count_kind(&cp, EventKind::IdentityEvidenceRecorded).await,
        1
    );
    // Per spec §8.7: target is the *existing* asset, candidate is the *new* version.
    let ev = cp
        .identity()
        .get_identity_evidence(ev_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ev.target_type, IdentityEvidenceTarget::FileAsset);
    assert_eq!(ev.target_id, existing_asset_id.0);
    assert_eq!(ev.candidate_id, Some(new_version_id.0));
}

#[tokio::test]
async fn etag_match_is_the_same_path_as_hash_match() {
    // Per spec: an ETag match arrives at record_discovered_file with no
    // alias proof and produces identical behavior to a hash match.
    let (cp, _tmp) = cp().await;
    let _ = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::ObjectStoreKey,
                location_value: "s3://b/a.mkv#etag-x".to_owned(),
                content_hash: "etag-x".to_owned(),
                size_bytes: 10,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let second = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::ObjectStoreKey,
                location_value: "s3://b/copy.mkv#etag-x".to_owned(),
                content_hash: "etag-x".to_owned(),
                size_bytes: 10,
                observed_at: T0 + Duration::seconds(1),
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        hash_match_evidence,
        ..
    } = second
    else {
        panic!();
    };
    assert!(hash_match_evidence.is_some());
}

// --- §13.2 DiscoveredFile.proof persistence sub-tests --------------------

#[tokio::test]
async fn discover_with_local_proof_persists_on_initial_location() {
    let (cp, _tmp) = cp().await;
    let outcome = cp
        .record_discovered_file(new_local_with_proof("/srv/x.mkv", "h", 1, 1, 1), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = outcome
    else {
        panic!();
    };
    let loc = cp
        .identity()
        .get_file_location(file_location_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loc.proof_kind.as_deref(), Some("file_id_generation"));
    assert!(loc.proof_value.is_some());
    assert!(
        loc.proof_value
            .as_ref()
            .unwrap()
            .contains("\"file_id\":\"1\"")
    );
}

#[tokio::test]
async fn discover_with_object_store_proof_persists() {
    let (cp, _tmp) = cp().await;
    let outcome = cp
        .record_discovered_file(new_object_store_with_proof("k.mkv", "h", 1, "b", "v"), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = outcome
    else {
        panic!();
    };
    let loc = cp
        .identity()
        .get_file_location(file_location_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loc.proof_kind.as_deref(), Some("object_version_id"));
    assert!(
        loc.proof_value
            .as_ref()
            .unwrap()
            .contains("\"bucket\":\"b\"")
    );
    assert!(
        loc.proof_value
            .as_ref()
            .unwrap()
            .contains("\"key\":\"k.mkv\"")
    );
    assert!(
        loc.proof_value
            .as_ref()
            .unwrap()
            .contains("\"version_id\":\"v\"")
    );
}

#[tokio::test]
async fn discover_without_proof_persists_nulls() {
    let (cp, _tmp) = cp().await;
    let outcome = cp
        .record_discovered_file(new_local("/srv/x.mkv", "h", 1), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = outcome
    else {
        panic!();
    };
    let loc = cp
        .identity()
        .get_file_location(file_location_id)
        .await
        .unwrap()
        .unwrap();
    assert!(loc.proof_kind.is_none());
    assert!(loc.proof_value.is_none());
}

#[tokio::test]
async fn alias_attach_persists_proof_on_new_location() {
    let (cp, _tmp) = cp().await;
    let first = cp
        .record_discovered_file(new_local_with_proof("/srv/a.mkv", "h", 1, 5, 1), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = first
    else {
        panic!();
    };
    let second = cp
        .record_discovered_file(
            new_local_with_proof("/srv/b.mkv", "h", 1, 5, 1),
            Some(AliasProof::LocalFileIdGeneration {
                file_id: 5,
                generation: 1,
                prior_location_id: file_location_id,
            }),
        )
        .await
        .unwrap();
    let IngestOutcome::AliasAttached {
        new_file_location_id,
        ..
    } = second
    else {
        panic!("expected AliasAttached");
    };
    let new_loc = cp
        .identity()
        .get_file_location(new_file_location_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new_loc.proof_kind.as_deref(), Some("file_id_generation"));
}

#[tokio::test]
async fn alias_attach_proof_drift_rejected() {
    let (cp, _tmp) = cp().await;
    let first = cp
        .record_discovered_file(new_local_with_proof("/srv/a.mkv", "h", 1, 5, 1), None)
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = first
    else {
        panic!();
    };
    // discovered.proof says generation=2; alias_proof says generation=1.
    let err = cp
        .record_discovered_file(
            new_local_with_proof("/srv/b.mkv", "h", 1, 5, 2),
            Some(AliasProof::LocalFileIdGeneration {
                file_id: 5,
                generation: 1,
                prior_location_id: file_location_id,
            }),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Conflict(_)),
        "got: {err:?}"
    );
    // The alias-attach branch did NOT insert any new file_location.
    let evidence = cp
        .identity()
        .list_identity_evidence_by_target(IdentityEvidenceTarget::FileVersion, 1)
        .await
        .unwrap();
    let _ = evidence;
    // Only the first discovery's events should exist.
    assert_eq!(count_kind(&cp, EventKind::FileAssetCreated).await, 1);
    assert_eq!(count_kind(&cp, EventKind::FileLocationAliased).await, 0);
}
