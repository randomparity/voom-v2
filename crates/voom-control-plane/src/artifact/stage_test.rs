use super::*;

use std::path::Path;

use sqlx::Row;
use time::OffsetDateTime;
use voom_core::{ErrorCode, FileLocationId, FileVersionId, VoomError, rng_test_support::FrozenRng};
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, NewFileLocation, ProducedBy,
};

use crate::ControlPlane;

#[tokio::test]
async fn missing_source_version_returns_not_found() {
    let (cp, _db, dir) = fixture().await;
    let err = cp
        .stage_copy(StageCopyInput {
            file_version_id: FileVersionId(404),
            source_location_id: None,
            staging_path: dir.path().join("staged.bin"),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn implicit_source_location_requires_exactly_one_live_local_path() {
    let (cp, _db, dir) = fixture().await;
    let version_without_locations = create_version_without_locations(&cp).await;
    let zero_err = cp
        .stage_copy(StageCopyInput {
            file_version_id: version_without_locations,
            source_location_id: None,
            staging_path: dir.path().join("zero.bin"),
        })
        .await
        .unwrap_err();
    assert_eq!(zero_err.code(), ErrorCode::ConfigInvalid);

    let source = dir.path().join("source.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let extra = dir.path().join("source-alias.bin");
    std::fs::write(&extra, b"source bytes").unwrap();
    create_location(
        &cp,
        seeded.file_version_id,
        FileLocationKind::LocalPath,
        &extra,
    )
    .await;

    let multiple_err = cp
        .stage_copy(StageCopyInput {
            file_version_id: seeded.file_version_id,
            source_location_id: None,
            staging_path: dir.path().join("multiple.bin"),
        })
        .await
        .unwrap_err();
    assert_eq!(multiple_err.code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn explicit_source_location_must_match_source_version_and_be_live_local_path() {
    let (cp, _db, dir) = fixture().await;
    let source_a = dir.path().join("a.bin");
    let source_b = dir.path().join("b.bin");
    std::fs::write(&source_a, b"a").unwrap();
    std::fs::write(&source_b, b"b").unwrap();
    let seeded_a = seed_source(&cp, &source_a, b"a").await;
    let seeded_b = seed_source(&cp, &source_b, b"b").await;

    let wrong_version_err = cp
        .stage_copy(StageCopyInput {
            file_version_id: seeded_a.file_version_id,
            source_location_id: Some(seeded_b.file_location_id),
            staging_path: dir.path().join("wrong-version.bin"),
        })
        .await
        .unwrap_err();
    assert_eq!(wrong_version_err.code(), ErrorCode::ConfigInvalid);

    retire_location(&cp, seeded_a.file_location_id).await;
    let retired_err = cp
        .stage_copy(StageCopyInput {
            file_version_id: seeded_a.file_version_id,
            source_location_id: Some(seeded_a.file_location_id),
            staging_path: dir.path().join("retired.bin"),
        })
        .await
        .unwrap_err();
    assert_eq!(retired_err.code(), ErrorCode::ConfigInvalid);

    let non_local_location = create_location(
        &cp,
        seeded_b.file_version_id,
        FileLocationKind::SharedMount,
        &source_b,
    )
    .await;
    let non_local_err = cp
        .stage_copy(StageCopyInput {
            file_version_id: seeded_b.file_version_id,
            source_location_id: Some(non_local_location),
            staging_path: dir.path().join("shared-mount.bin"),
        })
        .await
        .unwrap_err();
    assert_eq!(non_local_err.code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn existing_staging_path_returns_config_without_overwrite() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    std::fs::write(&staging, b"already there").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let err = cp
        .stage_copy(StageCopyInput {
            file_version_id: seeded.file_version_id,
            source_location_id: None,
            staging_path: staging.clone(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert_eq!(std::fs::read(&staging).unwrap(), b"already there");
}

#[tokio::test]
async fn staging_path_created_after_preflight_is_not_overwritten_and_returns_config() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let err = stage_copy_with_hooks(
        &cp,
        StageCopyInput {
            file_version_id: seeded.file_version_id,
            source_location_id: None,
            staging_path: staging.clone(),
        },
        &CreateStagingPathBeforeInstall {
            bytes: b"concurrent writer",
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert_eq!(std::fs::read(&staging).unwrap(), b"concurrent writer");
}

#[cfg(unix)]
#[tokio::test]
async fn source_and_staging_symlinks_are_rejected() {
    let (cp, _db, dir) = fixture().await;
    let real_source = dir.path().join("source.bin");
    let source_link = dir.path().join("source-link.bin");
    let staging_target = dir.path().join("staged-real.bin");
    let staging_link = dir.path().join("staged-link.bin");
    std::fs::write(&real_source, b"source bytes").unwrap();
    std::fs::write(&staging_target, b"existing staging target").unwrap();
    std::os::unix::fs::symlink(&real_source, &source_link).unwrap();
    std::os::unix::fs::symlink(&staging_target, &staging_link).unwrap();

    let source_link_seeded = seed_source(&cp, &source_link, b"source bytes").await;
    let source_err = cp
        .stage_copy(StageCopyInput {
            file_version_id: source_link_seeded.file_version_id,
            source_location_id: None,
            staging_path: dir.path().join("source-link-stage.bin"),
        })
        .await
        .unwrap_err();
    assert_eq!(source_err.code(), ErrorCode::ConfigInvalid);

    let real_seeded = seed_source(&cp, &real_source, b"source bytes").await;
    let staging_err = cp
        .stage_copy(StageCopyInput {
            file_version_id: real_seeded.file_version_id,
            source_location_id: None,
            staging_path: staging_link,
        })
        .await
        .unwrap_err();
    assert_eq!(staging_err.code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn database_failure_after_copy_removes_staging_file_and_reports_cleanup() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let err = stage_copy_with_hooks(
        &cp,
        StageCopyInput {
            file_version_id: seeded.file_version_id,
            source_location_id: None,
            staging_path: staging.clone(),
        },
        &FailBeforeDatabaseTransaction,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), ErrorCode::DbUnreachable);
    assert!(
        !staging.exists(),
        "new staging file must be removed when durable recording fails"
    );
    let Some(data) = err.data() else {
        panic!("cleanup report data");
    };
    assert_eq!(data["staging_path"], staging.display().to_string());
    assert_eq!(data["cleanup_attempted"], true);
    assert_eq!(data["cleanup_succeeded"], true);
}

#[tokio::test]
async fn database_failure_reports_cleanup_failure_as_structured_data() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let err = stage_copy_with_hooks(
        &cp,
        StageCopyInput {
            file_version_id: seeded.file_version_id,
            source_location_id: None,
            staging_path: staging.clone(),
        },
        &ReplaceStagingFileWithDirectoryBeforeDatabase,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), ErrorCode::DbUnreachable);
    let Some(data) = err.data() else {
        panic!("cleanup report data");
    };
    assert_eq!(data["staging_path"], staging.display().to_string());
    assert_eq!(data["cleanup_attempted"], true);
    assert_eq!(data["cleanup_succeeded"], false);
    assert!(data["cleanup_error"].as_str().is_some());
}

#[tokio::test]
async fn database_failure_does_not_remove_replacement_file_during_cleanup() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let err = stage_copy_with_hooks(
        &cp,
        StageCopyInput {
            file_version_id: seeded.file_version_id,
            source_location_id: None,
            staging_path: staging.clone(),
        },
        &ReplaceStagingFileWithRegularFileBeforeDatabase {
            bytes: b"replacement bytes",
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), ErrorCode::DbUnreachable);
    assert_eq!(std::fs::read(&staging).unwrap(), b"replacement bytes");
    let Some(data) = err.data() else {
        panic!("cleanup report data");
    };
    assert_eq!(data["cleanup_attempted"], true);
    assert_eq!(data["cleanup_succeeded"], false);
    assert_eq!(data["cleanup_error"], "staging path changed before cleanup");
}

#[tokio::test]
async fn database_failure_does_not_remove_same_content_replacement_file() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let err = stage_copy_with_hooks(
        &cp,
        StageCopyInput {
            file_version_id: seeded.file_version_id,
            source_location_id: None,
            staging_path: staging.clone(),
        },
        &ReplaceStagingFileWithRegularFileBeforeDatabase {
            bytes: b"source bytes",
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), ErrorCode::DbUnreachable);
    assert_eq!(std::fs::read(&staging).unwrap(), b"source bytes");
    let Some(data) = err.data() else {
        panic!("cleanup report data");
    };
    assert_eq!(data["cleanup_attempted"], true);
    assert_eq!(data["cleanup_succeeded"], false);
    assert_eq!(data["cleanup_error"], "staging path changed before cleanup");
}

#[tokio::test]
async fn success_copies_bytes_records_rows_and_emits_artifact_staged() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.bin");
    let staging = dir.path().join("staged.bin");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let report = cp
        .stage_copy(StageCopyInput {
            file_version_id: seeded.file_version_id,
            source_location_id: Some(seeded.file_location_id),
            staging_path: staging.clone(),
        })
        .await
        .unwrap();

    let expected_checksum = blake3_checksum(b"source bytes");
    assert_eq!(std::fs::read(&staging).unwrap(), b"source bytes");
    assert_eq!(report.source_file_version_id, seeded.file_version_id);
    assert_eq!(report.source_location_id, seeded.file_location_id);
    assert_eq!(report.source_path, source.canonicalize().unwrap());
    assert_eq!(report.staging_path, staging.canonicalize().unwrap());
    assert_eq!(report.size_bytes, 12);
    assert_eq!(report.checksum, expected_checksum);

    let handle = cp
        .artifacts()
        .get_handle(report.artifact_handle_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(handle.file_version_id, Some(seeded.file_version_id));
    assert_eq!(handle.durability_class, "staging");
    assert_eq!(handle.mutability, "immutable");

    let row = sqlx::query(
        "SELECT size_bytes, checksum, allowed_access_modes, source_lineage \
         FROM artifact_handles WHERE id = ?",
    )
    .bind(i64::try_from(report.artifact_handle_id.0).unwrap())
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    let size_bytes: i64 = row.try_get("size_bytes").unwrap();
    let checksum: String = row.try_get("checksum").unwrap();
    let access: String = row.try_get("allowed_access_modes").unwrap();
    let lineage: String = row.try_get("source_lineage").unwrap();
    assert_eq!(size_bytes, 12);
    assert_eq!(checksum, expected_checksum);
    assert_eq!(access, r#"["local_path"]"#);
    assert!(lineage.contains(&format!(
        "\"source_file_version_id\":{}",
        seeded.file_version_id.0
    )));
    assert!(lineage.contains(&format!(
        "\"source_location_id\":{}",
        seeded.file_location_id.0
    )));

    let locations = cp
        .artifacts()
        .list_locations_for_handle(report.artifact_handle_id)
        .await
        .unwrap();
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].id, report.artifact_location_id);
    assert_eq!(locations[0].kind, "staging");
    assert_eq!(
        locations[0].value,
        staging.canonicalize().unwrap().display().to_string()
    );

    let events = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::ArtifactStaged),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(events.items.len(), 1);
    assert_eq!(
        events.items[0].envelope.subject_id,
        Some(report.artifact_handle_id.0)
    );
    let voom_events::Event::ArtifactStaged(payload) = &events.items[0].envelope.payload else {
        panic!("expected artifact.staged payload");
    };
    assert_eq!(payload.artifact_handle_id, report.artifact_handle_id.0);
    assert_eq!(payload.artifact_location_id, report.artifact_location_id.0);
    assert_eq!(payload.source_file_version_id, seeded.file_version_id.0);
    assert_eq!(
        payload.source_file_location_id,
        Some(seeded.file_location_id.0)
    );
    assert_eq!(
        payload.staging_path,
        staging.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(payload.size_bytes, 12);
    assert_eq!(payload.checksum, expected_checksum);
}

#[derive(Debug, Clone, Copy)]
struct SeededSource {
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
}

async fn fixture() -> (ControlPlane, tempfile::NamedTempFile, tempfile::TempDir) {
    let db = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, db, artifact_tempdir())
}

fn artifact_tempdir() -> tempfile::TempDir {
    tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

async fn seed_source(cp: &ControlPlane, path: &Path, bytes: &[u8]) -> SeededSource {
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

async fn create_version_without_locations(cp: &ControlPlane) -> FileVersionId {
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
            voom_store::repo::identity::NewFileVersion {
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
    cp: &ControlPlane,
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

async fn retire_location(cp: &ControlPlane, location_id: FileLocationId) {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let location = cp
        .identity()
        .get_file_location_in_tx(&mut tx, location_id)
        .await
        .unwrap()
        .unwrap();
    cp.identity()
        .retire_file_location_in_tx(
            &mut tx,
            location_id,
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
            location.epoch,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

struct CreateStagingPathBeforeInstall {
    bytes: &'static [u8],
}

impl StageCopyHooks for CreateStagingPathBeforeInstall {
    fn before_install(&self, context: StageCopyInstallContext<'_>) -> Result<(), VoomError> {
        std::fs::write(context.staging_path, self.bytes).unwrap();
        Ok(())
    }
}

struct FailBeforeDatabaseTransaction;

impl StageCopyHooks for FailBeforeDatabaseTransaction {
    fn before_database_transaction(
        &self,
        _context: StageCopyDatabaseContext<'_>,
    ) -> Result<(), VoomError> {
        Err(VoomError::Database(
            "injected stage-copy db failure".to_owned(),
        ))
    }
}

struct ReplaceStagingFileWithDirectoryBeforeDatabase;

impl StageCopyHooks for ReplaceStagingFileWithDirectoryBeforeDatabase {
    fn before_database_transaction(
        &self,
        context: StageCopyDatabaseContext<'_>,
    ) -> Result<(), VoomError> {
        std::fs::remove_file(context.staging_path).unwrap();
        std::fs::create_dir(context.staging_path).unwrap();
        Err(VoomError::Database("injected durable failure".to_owned()))
    }
}

struct ReplaceStagingFileWithRegularFileBeforeDatabase {
    bytes: &'static [u8],
}

impl StageCopyHooks for ReplaceStagingFileWithRegularFileBeforeDatabase {
    fn before_database_transaction(
        &self,
        context: StageCopyDatabaseContext<'_>,
    ) -> Result<(), VoomError> {
        std::fs::remove_file(context.staging_path).unwrap();
        std::fs::write(context.staging_path, self.bytes).unwrap();
        Err(VoomError::Database("injected durable failure".to_owned()))
    }
}
