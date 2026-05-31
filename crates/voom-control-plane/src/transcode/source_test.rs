use super::*;

use std::path::Path;

use time::OffsetDateTime;
use voom_core::{ErrorCode, FileLocationId, FileVersionId, rng_test_support::FrozenRng};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, NewFileLocation, NewFileVersion,
    ProducedBy,
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
        FileLocationKind::LocalPath,
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

    let non_local = create_location(
        &cp,
        seeded_b.file_version_id,
        FileLocationKind::SharedMount,
        &source_b,
    )
    .await;
    let non_local_err = select_source(&cp, seeded_b.file_version_id, Some(non_local))
        .await
        .unwrap_err();
    assert_eq!(non_local_err.error_code(), ErrorCode::ConfigInvalid);
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
    tempfile::NamedTempFile,
    tempfile::TempDir,
) {
    let db = tempfile::NamedTempFile::new().unwrap();
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
                location_kind: FileLocationKind::LocalPath,
                location_value: path.display().to_string(),
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
    kind: FileLocationKind,
    path: &Path,
) -> FileLocationId {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let location = cp
        .identity()
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id,
                kind,
                value: path.display().to_string(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    location.id
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
