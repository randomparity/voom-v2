use super::super::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, SqliteIdentityRepo,
};
use super::*;
use crate::test_support::{T0, fresh_initialized_pool_at};
use sqlx::SqlitePool;
use voom_core::FileVersionId;

async fn fresh() -> (SqliteIdentityRepo, SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (SqliteIdentityRepo::new(pool.clone()), pool, tmp)
}

/// Ingest a fresh local file and return its (version, location) ids.
async fn ingest(
    repo: &SqliteIdentityRepo,
    pool: &SqlitePool,
    path: &str,
    content_hash: &str,
    size_bytes: u64,
) -> (FileVersionId, FileLocationId) {
    let mut tx = pool.begin().await.unwrap();
    let outcome = repo
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.to_owned(),
                content_hash: content_hash.to_owned(),
                size_bytes,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id,
        file_location_id,
        ..
    } = outcome
    else {
        panic!("expected NewFileAsset");
    };
    (file_version_id, file_location_id)
}

#[tokio::test]
async fn record_and_find_hardlink_by_dev_ino() {
    let (repo, pool, _tmp) = fresh().await;
    let (version_id, location_id) = ingest(&repo, &pool, "/srv/a.mkv", "hash-1", 1024).await;

    let mut tx = pool.begin().await.unwrap();
    record_scan_fact_in_tx(&mut tx, location_id, 42, 7, 2, T0)
        .await
        .unwrap();
    let found = find_live_hardlink_location_in_tx(&mut tx, 42, 7)
        .await
        .unwrap()
        .expect("same dev/ino resolves to the recorded location");
    tx.commit().await.unwrap();

    assert_eq!(found.file_location_id, location_id);
    assert_eq!(found.file_version_id, version_id);
    assert_eq!(found.content_hash, "hash-1");
    assert_eq!(found.size_bytes, 1024);
}

#[tokio::test]
async fn no_match_for_unknown_dev_ino_or_copy_on_different_inode() {
    let (repo, pool, _tmp) = fresh().await;
    let (_v, location_id) = ingest(&repo, &pool, "/srv/a.mkv", "hash-1", 1024).await;

    let mut tx = pool.begin().await.unwrap();
    record_scan_fact_in_tx(&mut tx, location_id, 42, 7, 1, T0)
        .await
        .unwrap();
    // A byte-identical copy has the same hash but a different inode: no match.
    assert!(
        find_live_hardlink_location_in_tx(&mut tx, 42, 8)
            .await
            .unwrap()
            .is_none()
    );
    // A different device with the same inode number is also not a hardlink.
    assert!(
        find_live_hardlink_location_in_tx(&mut tx, 99, 7)
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn attach_hardlink_adds_second_live_location_to_same_version() {
    let (repo, pool, _tmp) = fresh().await;
    let (version_id, first_location) = ingest(&repo, &pool, "/srv/a.mkv", "hash-1", 1024).await;

    let mut tx = pool.begin().await.unwrap();
    record_scan_fact_in_tx(&mut tx, first_location, 42, 7, 2, T0)
        .await
        .unwrap();
    // A second path with the same (dev, ino) attaches to the existing version.
    let second_location = repo
        .attach_local_hardlink_location_in_tx(&mut tx, version_id, "/srv/b.mkv", T0)
        .await
        .unwrap();
    record_scan_fact_in_tx(&mut tx, second_location, 42, 7, 2, T0)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_ne!(second_location, first_location);
    // Both locations are live on the one version.
    let locations = repo
        .list_live_file_locations_by_version(version_id)
        .await
        .unwrap();
    assert_eq!(locations.len(), 2);
    // The lookup returns the earliest live location for the shared inode.
    let mut tx = pool.begin().await.unwrap();
    let found = find_live_hardlink_location_in_tx(&mut tx, 42, 7)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(found.file_location_id, first_location);
}
