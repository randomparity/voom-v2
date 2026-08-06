use super::*;

use std::path::Path;

use time::OffsetDateTime;
use voom_core::{ErrorCode, FileLocationId, FileVersionId, rng_test_support::FrozenRng};
use voom_store::repo::media::identity::{
    DiscoveredFile, FileAssetRepo, FileLocationRepo, FileVersionRepo, IngestOutcome,
    NewFileLocation, NewFileVersion, ProducedBy,
};

#[tokio::test]
async fn missing_source_version_returns_not_found() {
    let (cp, _db, _dir) = fixture().await;

    let err = select_source(&cp, FileVersionId(404), None)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn implicit_source_requires_exactly_one_live_local_location() {
    let (cp, _db, dir) = fixture().await;
    let version_without_locations = create_version_without_locations(&cp).await;

    let zero_err = select_source(&cp, version_without_locations, None)
        .await
        .unwrap_err();
    assert_eq!(zero_err.error_code(), ErrorCode::ConfigInvalid);

    let root = dir.path().canonicalize().unwrap();
    let source = root.join("source.bin");
    let alias = root.join("alias.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    std::fs::write(&alias, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    create_location(
        &cp,
        seeded.file_version_id,
        voom_store::test_support::TEST_STORAGE_ROOT_ID,
        &alias,
    )
    .await;

    let ambiguous_err = select_source(&cp, seeded.file_version_id, None)
        .await
        .unwrap_err();
    assert_eq!(ambiguous_err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn explicit_source_location_must_match_and_be_live_local() {
    let (cp, _db, dir) = fixture().await;
    let root = dir.path().canonicalize().unwrap();
    let source_a = root.join("a.bin");
    let source_b = root.join("b.bin");
    std::fs::write(&source_a, b"a").unwrap();
    std::fs::write(&source_b, b"b").unwrap();
    let seeded_a = seed_source(&cp, &source_a, b"a").await;
    let seeded_b = seed_source(&cp, &source_b, b"b").await;

    let wrong_version_err = select_source(
        &cp,
        seeded_a.file_version_id,
        Some(seeded_b.file_location_id),
    )
    .await
    .unwrap_err();
    assert_eq!(wrong_version_err.error_code(), ErrorCode::ConfigInvalid);

    let foreign_root_id = seed_foreign_storage_root(&cp).await;
    let non_local =
        create_location(&cp, seeded_b.file_version_id, foreign_root_id, &source_b).await;
    let non_local_err = select_source(&cp, seeded_b.file_version_id, Some(non_local))
        .await
        .unwrap_err();
    assert_eq!(non_local_err.error_code(), ErrorCode::ArtifactUnavailable);
}

#[cfg(unix)]
#[tokio::test]
async fn source_selection_rejects_final_path_symlink() {
    let (cp, _db, dir) = fixture().await;
    let root = dir.path().canonicalize().unwrap();
    let real = root.join("real.mkv");
    let link = root.join("link.mkv");
    std::fs::write(&real, b"source bytes").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let seeded = seed_source(&cp, &link, b"source bytes").await;

    let err = select_source(&cp, seeded.file_version_id, None)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("symlink"));
}

#[tokio::test]
async fn valid_live_local_location_is_selected() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().canonicalize().unwrap().join("source.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let selected = select_source(&cp, seeded.file_version_id, Some(seeded.file_location_id))
        .await
        .unwrap();

    assert_eq!(selected.version.id, seeded.file_version_id);
    assert_eq!(selected.location.id, seeded.file_location_id);
}

#[derive(Debug, Clone, Copy)]
struct SeededSource {
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
}

async fn fixture() -> (
    crate::ControlPlane,
    voom_test_support::TempDatabase,
    tempfile::TempDir,
) {
    let db = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, db, tempfile::TempDir::new().unwrap())
}

async fn seed_source(cp: &crate::ControlPlane, path: &Path, bytes: &[u8]) -> SeededSource {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    &path.display().to_string(),
                ),
                content_hash: blake3_checksum(bytes),
                size_bytes: u64::try_from(bytes.len()).unwrap(),
                observed_at: OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id,
        file_location_id,
        ..
    } = outcome
    else {
        panic!("seed_source should create a new file asset");
    };
    SeededSource {
        file_version_id,
        file_location_id,
    }
}

async fn create_version_without_locations(cp: &crate::ControlPlane) -> FileVersionId {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let asset = cp
        .identity()
        .create_file_asset_in_tx(&mut tx, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    let version = cp
        .identity()
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: asset.id,
                content_hash: blake3_checksum(b"unused"),
                size_bytes: 6,
                produced_by: ProducedBy::Ingest,
                produced_from_version_id: None,
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    version.id
}

async fn create_location(
    cp: &crate::ControlPlane,
    file_version_id: FileVersionId,
    storage_root_id: voom_core::StorageRootId,
    path: &Path,
) -> FileLocationId {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let location = cp
        .identity()
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id,
                storage_root_id,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    &path.display().to_string(),
                ),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    location.id
}

async fn seed_foreign_storage_root(cp: &crate::ControlPlane) -> voom_core::StorageRootId {
    sqlx::query(
        "INSERT INTO nodes \
         (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (9000002, 'foreign-transcode-owner', 'local', 'active', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_roots \
         (id, library_id, owner_node_id, provider_kind, provider_locator, display_locator, \
          state, root_epoch, activation_identity, include_globs, exclude_globs, \
          extension_allowlist, scan_mode, symlink_policy, hidden_file_policy, \
          stability_seconds, debounce_seconds, enabled, created_at, updated_at) \
         VALUES (9000002, 9000001, 9000002, 'local_filesystem', '/', '/', 'active', 1, \
                 'foreign-transcode-root', '[]', '[]', '[]', 'manual_recursive', 'reject', \
                 'ignore', 0, 0, 1, '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z')",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    voom_core::StorageRootId(9_000_002)
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
