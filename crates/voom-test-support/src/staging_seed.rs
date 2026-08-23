//! Durable seeding of staged artifacts for tests.
//!
//! Replaces the retired control-plane byte copy
//! (`ControlPlane::stage_copy`, ADR 0075): tests write the staging bytes
//! themselves and then record the exact rows `stage_copy` used to record —
//! the artifact handle and the staging artifact location — through the same
//! voom-store repositories, with the same facts checks.

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;
use time::OffsetDateTime;

use voom_core::ids::{ArtifactHandleId, ArtifactLocationId, FileVersionId};
use voom_core::VoomError;
use voom_store::repo::media::artifacts::{
    ArtifactHandleAccessMode, ArtifactLocationKind, NewArtifactHandle, NewArtifactLocation,
    SqliteArtifactRepo,
};
use voom_store::repo::media::identity::{
    FileLocationRepo, FileVersion, FileVersionRepo, SqliteIdentityRepo,
};

/// What a seeded staged artifact exposes to the calling test: the ids of
/// every row created plus the observed facts of the bytes at
/// [`SeededStagedArtifact::staging_path`].
#[derive(Debug, Clone)]
pub struct SeededStagedArtifact {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub source_file_version_id: FileVersionId,
    /// The version's live rooted source location at seeding time.
    pub source_file_location_id: voom_core::ids::FileLocationId,
    pub staging_path: PathBuf,
    pub size_bytes: u64,
    pub checksum: String,
}

/// Record a staged artifact whose bytes already sit at `staging_path`.
///
/// Mirrors the durable half of the removed `ControlPlane::stage_copy`: the
/// file version must be live and its pinned facts must match the staged
/// bytes (size + blake3 checksum), then the handle and the staging
/// artifact location are recorded in one transaction.
///
/// # Errors
/// Storage errors from the underlying repositories; [`VoomError`] for a
/// missing/retired file version or a facts mismatch.
pub async fn seed_staged_artifact(
    pool: &SqlitePool,
    file_version_id: FileVersionId,
    staging_path: &Path,
) -> Result<SeededStagedArtifact, VoomError> {
    let now = OffsetDateTime::now_utc();
    let identity = SqliteIdentityRepo::new(pool.clone());
    // BEGIN IMMEDIATE: the tx writes (handle insert) after reads; a deferred
    // start deadlocks into SQLITE_BUSY under parallel test load when the
    // upgrade races a concurrent writer.
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| {
            VoomError::database_context("seed_staged_artifact begin transaction", error)
        })?;
    let version = identity
        .get_file_version_in_tx(&mut tx, file_version_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("file_versions {file_version_id} missing")))?;
    if version.retired_at.is_some() {
        return Err(VoomError::NotFound(format!(
            "file_versions {file_version_id} is retired"
        )));
    }
    let source_file_location_id = *identity
        .list_live_file_locations_by_version_in_tx(&mut tx, file_version_id)
        .await?
        .first()
        .ok_or_else(|| {
            VoomError::NotFound(format!(
                "file_versions {file_version_id} has no live source location"
            ))
        })?;

    let bytes = tokio::fs::read(staging_path)
        .await
        .map_err(|error| VoomError::ArtifactUnavailable(format!("read staged bytes: {error}")))?;
    let size_bytes = u64::try_from(bytes.len())
        .map_err(|error| VoomError::Internal(format!("staged size overflow: {error}")))?;
    let checksum = format!("blake3:{}", blake3::hash(&bytes).to_hex());
    require_matching_version_facts(&version, size_bytes, &checksum)?;

    let artifacts = SqliteArtifactRepo::new(pool.clone());
    let handle = artifacts
        .create_handle_in_tx(
            &mut tx,
            NewArtifactHandle {
                size_bytes: Some(i64::try_from(size_bytes).map_err(|error| {
                    VoomError::Internal(format!("artifact size exceeds SQLite integer: {error}"))
                })?),
                checksum: Some(checksum.clone()),
                privacy_class: "internal".to_owned(),
                durability_class: "staging".to_owned(),
                allowed_access_modes: vec![ArtifactHandleAccessMode::LocalPath],
                mutability: "immutable".to_owned(),
                source_lineage: Some(serde_json::json!({
                    "source_file_version_id": file_version_id.0,
                    "source_file_location_id": source_file_location_id.0,
                })),
                file_version_id: Some(file_version_id),
                created_at: now,
            },
        )
        .await?;
    let location = artifacts
        .record_location_in_tx(
            &mut tx,
            NewArtifactLocation {
                artifact_handle_id: handle.id,
                kind: ArtifactLocationKind::Staging,
                value: staging_path.display().to_string(),
                observed_at: now,
            },
        )
        .await?;
    tx.commit()
        .await
        .map_err(|error| VoomError::database_context("seed_staged_artifact commit", error))?;

    Ok(SeededStagedArtifact {
        artifact_handle_id: handle.id,
        artifact_location_id: location.id,
        source_file_version_id: file_version_id,
        source_file_location_id,
        staging_path: staging_path.to_path_buf(),
        size_bytes,
        checksum,
    })
}

/// The facts check `stage_copy` ran against the source version before
/// recording anything: a mismatch here is fixture corruption, not success.
fn require_matching_version_facts(
    version: &FileVersion,
    size_bytes: u64,
    checksum: &str,
) -> Result<(), VoomError> {
    if version.size_bytes != size_bytes || version.content_hash != checksum {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "staged bytes do not match file_version {}",
            version.id
        )));
    }
    Ok(())
}
