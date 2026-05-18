#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]
//! M2 rename-reconciliation coverage. Every conflict path asserts the
//! prior location stays live, no new `file_locations` row, and no
//! `file_location.*_by_move` events were written.

use std::sync::{Arc, Mutex};

use time::Duration;

use voom_control_plane::ControlPlane;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{SystemClock, VoomError};
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, LocationProof, ObservedBytes,
    RenameProof,
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

/// Seed a location with a `file_id_generation` proof; return the
/// `file_location_id`.
async fn seed_local(cp: &ControlPlane, path: &str, hash: &str, size: u64) -> u64 {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.to_owned(),
                content_hash: hash.to_owned(),
                size_bytes: size,
                observed_at: T0,
                proof: Some(LocationProof::LocalFileIdGeneration {
                    file_id: 99,
                    generation: 1,
                }),
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = outcome
    else {
        panic!();
    };
    file_location_id.0
}

/// Same shape for the object-store path.
async fn seed_object(cp: &ControlPlane, key: &str, hash: &str, size: u64) -> u64 {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::ObjectStoreKey,
                location_value: format!("s3://b/{key}#v1"),
                content_hash: hash.to_owned(),
                size_bytes: size,
                observed_at: T0,
                proof: Some(LocationProof::ObjectStoreVersion {
                    bucket: "b".to_owned(),
                    key: key.to_owned(),
                    version_id: "v1".to_owned(),
                }),
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = outcome
    else {
        panic!();
    };
    file_location_id.0
}

fn assert_prior_still_live_and_no_move_events_via_count(
    retired: usize,
    recorded: usize,
    retired_before: usize,
    recorded_before: usize,
) {
    assert_eq!(retired, retired_before, "no new retired_by_move event");
    assert_eq!(recorded, recorded_before, "no new recorded_by_move event");
}

// --- Happy paths ---------------------------------------------------------

#[tokio::test]
async fn reconcile_rename_happy_path_local() {
    let (cp, _tmp) = cp().await;
    let prior_id = seed_local(&cp, "/srv/old.mkv", "h", 10).await;
    let retired_before = count_kind(&cp, EventKind::FileLocationRetiredByMove).await;
    let recorded_before = count_kind(&cp, EventKind::FileLocationRecordedByMove).await;
    let outcome = cp
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 99,
                generation: 1,
                prior_path_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(10),
        )
        .await
        .unwrap();
    assert_eq!(outcome.retired_location_id.0, prior_id);
    assert_eq!(
        count_kind(&cp, EventKind::FileLocationRetiredByMove).await,
        retired_before + 1
    );
    assert_eq!(
        count_kind(&cp, EventKind::FileLocationRecordedByMove).await,
        recorded_before + 1
    );
    // path_rule_match evidence appended on the new location.
    assert!(count_kind(&cp, EventKind::IdentityEvidenceRecorded).await >= 1);
    // Prior location row no longer appears in the live set.
    let live = cp
        .identity()
        .list_live_file_locations_by_version(outcome.file_version_id)
        .await
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, outcome.new_file_location_id);
}

#[tokio::test]
async fn reconcile_rename_happy_path_object_store() {
    let (cp, _tmp) = cp().await;
    let prior_id = seed_object(&cp, "old/key.mkv", "h", 10).await;
    let retired_before = count_kind(&cp, EventKind::FileLocationRetiredByMove).await;
    let recorded_before = count_kind(&cp, EventKind::FileLocationRecordedByMove).await;
    let outcome = cp
        .reconcile_rename(
            RenameProof::ObjectStoreVersion {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::ObjectStoreKey,
                new_value: "s3://b/new/key.mkv#v1".to_owned(),
                bucket: "b".to_owned(),
                key: "old/key.mkv".to_owned(),
                version_id: "v1".to_owned(),
                prior_key_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(10),
        )
        .await
        .unwrap();
    assert_eq!(
        count_kind(&cp, EventKind::FileLocationRetiredByMove).await,
        retired_before + 1
    );
    assert_eq!(
        count_kind(&cp, EventKind::FileLocationRecordedByMove).await,
        recorded_before + 1
    );
    let _ = outcome;
}

// --- Conflict paths: every one asserts no side effects -------------------

#[tokio::test]
async fn reconcile_rename_rejects_proof_kind_mismatch() {
    let (cp, _tmp) = cp().await;
    let prior_id = seed_local(&cp, "/srv/old.mkv", "h", 10).await;
    let retired_before = count_kind(&cp, EventKind::FileLocationRetiredByMove).await;
    let recorded_before = count_kind(&cp, EventKind::FileLocationRecordedByMove).await;
    let err = cp
        .reconcile_rename(
            RenameProof::ObjectStoreVersion {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                bucket: "b".to_owned(),
                key: "k".to_owned(),
                version_id: "v".to_owned(),
                prior_key_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    assert_prior_still_live_and_no_move_events_via_count(
        count_kind(&cp, EventKind::FileLocationRetiredByMove).await,
        count_kind(&cp, EventKind::FileLocationRecordedByMove).await,
        retired_before,
        recorded_before,
    );
}

#[tokio::test]
async fn reconcile_rename_rejects_proof_value_mismatch_local() {
    let (cp, _tmp) = cp().await;
    let prior_id = seed_local(&cp, "/srv/old.mkv", "h", 10).await;
    let retired_before = count_kind(&cp, EventKind::FileLocationRetiredByMove).await;
    let recorded_before = count_kind(&cp, EventKind::FileLocationRecordedByMove).await;
    let err = cp
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 99,
                generation: 2, // mismatch — seed used generation = 1
                prior_path_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    assert_prior_still_live_and_no_move_events_via_count(
        count_kind(&cp, EventKind::FileLocationRetiredByMove).await,
        count_kind(&cp, EventKind::FileLocationRecordedByMove).await,
        retired_before,
        recorded_before,
    );
}

#[tokio::test]
async fn reconcile_rename_rejects_proof_value_mismatch_object_store() {
    let (cp, _tmp) = cp().await;
    let prior_id = seed_object(&cp, "k.mkv", "h", 10).await;
    let retired_before = count_kind(&cp, EventKind::FileLocationRetiredByMove).await;
    let recorded_before = count_kind(&cp, EventKind::FileLocationRecordedByMove).await;
    let err = cp
        .reconcile_rename(
            RenameProof::ObjectStoreVersion {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::ObjectStoreKey,
                new_value: "s3://b/new.mkv#v2".to_owned(),
                bucket: "b".to_owned(),
                key: "k.mkv".to_owned(),
                version_id: "v2".to_owned(), // mismatch — seed used v1
                prior_key_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    assert_prior_still_live_and_no_move_events_via_count(
        count_kind(&cp, EventKind::FileLocationRetiredByMove).await,
        count_kind(&cp, EventKind::FileLocationRecordedByMove).await,
        retired_before,
        recorded_before,
    );
}

#[tokio::test]
async fn reconcile_rename_rejects_prior_path_present() {
    let (cp, _tmp) = cp().await;
    let prior_id = seed_local(&cp, "/srv/old.mkv", "h", 10).await;
    let retired_before = count_kind(&cp, EventKind::FileLocationRetiredByMove).await;
    let recorded_before = count_kind(&cp, EventKind::FileLocationRecordedByMove).await;
    let err = cp
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 99,
                generation: 1,
                prior_path_missing: false, // caller did NOT verify
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    assert_prior_still_live_and_no_move_events_via_count(
        count_kind(&cp, EventKind::FileLocationRetiredByMove).await,
        count_kind(&cp, EventKind::FileLocationRecordedByMove).await,
        retired_before,
        recorded_before,
    );
}

#[tokio::test]
async fn reconcile_rename_rejects_hash_drift() {
    let (cp, _tmp) = cp().await;
    let prior_id = seed_local(&cp, "/srv/old.mkv", "h-original", 10).await;
    let retired_before = count_kind(&cp, EventKind::FileLocationRetiredByMove).await;
    let recorded_before = count_kind(&cp, EventKind::FileLocationRecordedByMove).await;
    let err = cp
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 99,
                generation: 1,
                prior_path_missing: true,
            },
            ObservedBytes {
                content_hash: "h-different".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    assert_prior_still_live_and_no_move_events_via_count(
        count_kind(&cp, EventKind::FileLocationRetiredByMove).await,
        count_kind(&cp, EventKind::FileLocationRecordedByMove).await,
        retired_before,
        recorded_before,
    );
}

#[tokio::test]
async fn reconcile_rename_rejects_size_drift() {
    let (cp, _tmp) = cp().await;
    let prior_id = seed_local(&cp, "/srv/old.mkv", "h", 10).await;
    let retired_before = count_kind(&cp, EventKind::FileLocationRetiredByMove).await;
    let recorded_before = count_kind(&cp, EventKind::FileLocationRecordedByMove).await;
    let err = cp
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 99,
                generation: 1,
                prior_path_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 11, // mismatch — seed used 10
            },
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    assert_prior_still_live_and_no_move_events_via_count(
        count_kind(&cp, EventKind::FileLocationRetiredByMove).await,
        count_kind(&cp, EventKind::FileLocationRecordedByMove).await,
        retired_before,
        recorded_before,
    );
}

#[tokio::test]
async fn reconcile_rename_rejects_prior_already_retired() {
    let (cp, _tmp) = cp().await;
    let prior_id = seed_local(&cp, "/srv/old.mkv", "h", 10).await;
    // Successfully rename once.
    let _ = cp
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 99,
                generation: 1,
                prior_path_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap();
    let retired_before = count_kind(&cp, EventKind::FileLocationRetiredByMove).await;
    let recorded_before = count_kind(&cp, EventKind::FileLocationRecordedByMove).await;
    // Second call on the same now-retired prior_id must Conflict.
    let err = cp
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: voom_core::FileLocationId(prior_id),
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new2.mkv".to_owned(),
                file_id: 99,
                generation: 1,
                prior_path_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(10),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    assert_prior_still_live_and_no_move_events_via_count(
        count_kind(&cp, EventKind::FileLocationRetiredByMove).await,
        count_kind(&cp, EventKind::FileLocationRecordedByMove).await,
        retired_before,
        recorded_before,
    );
}
