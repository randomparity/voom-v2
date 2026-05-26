use super::*;

use std::path::Path;

use time::OffsetDateTime;
use voom_core::{ErrorCode, FileVersionId, rng_test_support::FrozenRng};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, NewFileLocation,
};

#[tokio::test]
async fn source_selection_rejects_ambiguous_live_locations() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.mp4");
    let alias = dir.path().join("alias.mp4");
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

    let err = select_source(&cp, seeded.file_version_id, None)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("multiple live local source locations")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn source_selection_rejects_final_path_symlink() {
    let (cp, _db, dir) = fixture().await;
    let real = dir.path().join("real.mp4");
    let link = dir.path().join("link.mp4");
    std::fs::write(&real, b"source bytes").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let seeded = seed_source(&cp, &link, b"source bytes").await;

    let err = select_source(&cp, seeded.file_version_id, None)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("symlink"));
}

#[derive(Debug, Clone, Copy)]
struct SeededSource {
    file_version_id: FileVersionId,
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
        file_location_id: _,
        ..
    } = outcome
    else {
        panic!("seed_source should create a new file asset");
    };
    SeededSource { file_version_id }
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
