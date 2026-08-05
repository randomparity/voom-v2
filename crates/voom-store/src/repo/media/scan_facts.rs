//! Hardlink inode facts captured at scan time (`scan_file_facts`, #249).
//!
//! One row per ingested local `file_locations` row, recording the physical
//! object's `(dev, ino)` and link count. Two live local locations sharing a
//! `(dev, ino)` are hardlinks to one physical file; the scan-persist layer uses
//! [`find_live_hardlink_location_in_tx`] to resolve a discovered hardlink to the
//! already-ingested `file_version` instead of minting a second asset. `(dev,
//! ino)` is the physical-object key; the caller additionally confirms content
//! identity before attaching, because filesystems recycle inode numbers.

use sqlx::{Row, Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{
    FileAssetId, FileLocationId, FileVersionId, ProviderRelativeLocator, StorageRootId, VoomError,
};

use super::super::common::{i64_from_u64, iso8601, u64_from_i64};

/// A live prior local location that shares a `(dev, ino)` with a discovered
/// candidate — a hardlink match. `content_hash`/`size_bytes` are the owning
/// `file_version`'s, so the caller can confirm the bytes match before attaching
/// (guarding against a recycled inode or an in-place edit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFactMatch {
    pub file_location_id: FileLocationId,
    pub file_version_id: FileVersionId,
    pub content_hash: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedAddressMatch {
    pub file_asset_id: FileAssetId,
    pub file_location_id: FileLocationId,
    pub file_version_id: FileVersionId,
    pub content_hash: String,
    pub size_bytes: u64,
}

/// Find the live file identity currently occupying one exact rooted address.
pub async fn find_live_scanned_address_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    storage_root_id: StorageRootId,
    provider_relative_locator: &ProviderRelativeLocator,
) -> Result<Option<ScannedAddressMatch>, VoomError> {
    let row = sqlx::query(
        "SELECT fv.file_asset_id, fl.id AS file_location_id, fl.file_version_id, \
                fv.content_hash, fv.size_bytes \
         FROM file_locations fl \
         JOIN file_versions fv ON fv.id = fl.file_version_id \
         WHERE fl.storage_root_id = ? AND fl.provider_relative_locator = ? \
           AND fl.address_state = 'rooted' AND fl.retired_at IS NULL \
           AND fv.retired_at IS NULL",
    )
    .bind(i64_from_u64(storage_root_id.0, "scan storage root id")?)
    .bind(provider_relative_locator.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("scan rooted-address lookup", error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let file_asset_id: i64 = row
        .try_get("file_asset_id")
        .map_err(|error| VoomError::database_context("scan address asset id", error))?;
    let file_location_id: i64 = row
        .try_get("file_location_id")
        .map_err(|error| VoomError::database_context("scan address location id", error))?;
    let file_version_id: i64 = row
        .try_get("file_version_id")
        .map_err(|error| VoomError::database_context("scan address version id", error))?;
    let content_hash: String = row
        .try_get("content_hash")
        .map_err(|error| VoomError::database_context("scan address content hash", error))?;
    let size_bytes: i64 = row
        .try_get("size_bytes")
        .map_err(|error| VoomError::database_context("scan address size bytes", error))?;
    Ok(Some(ScannedAddressMatch {
        file_asset_id: FileAssetId(u64_from_i64(file_asset_id, "scan address asset id")?),
        file_location_id: FileLocationId(u64_from_i64(
            file_location_id,
            "scan address location id",
        )?),
        file_version_id: FileVersionId(u64_from_i64(file_version_id, "scan address version id")?),
        content_hash,
        size_bytes: u64_from_i64(size_bytes, "scan address size bytes")?,
    }))
}

/// Record the inode facts for one ingested local `file_location`. `dev`/`ino`
/// are stored as the signed reinterpretation of the OS-reported `u64`
/// identifiers (lossless).
pub async fn record_scan_fact_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    file_location_id: FileLocationId,
    dev: u64,
    ino: u64,
    nlink: u64,
    observed_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let ts = iso8601(observed_at)?;
    sqlx::query(
        "INSERT INTO scan_file_facts (file_location_id, dev, ino, nlink, observed_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(i64_from_u64(
        file_location_id.0,
        concat!(module_path!(), ": ", stringify!(file_location_id.0)),
    )?)
    .bind(i64_from_u64(
        dev,
        concat!(module_path!(), ": ", stringify!(dev)),
    )?)
    .bind(i64_from_u64(
        ino,
        concat!(module_path!(), ": ", stringify!(ino)),
    )?)
    .bind(i64_from_u64(
        nlink,
        concat!(module_path!(), ": ", stringify!(nlink)),
    )?)
    .bind(&ts)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("scan_file_facts insert", e))?;
    Ok(())
}

/// Find the earliest live local `file_location` at a **different path** that
/// shares this `(dev, ino)` — a hardlink to the same physical file — along with
/// its owning live `file_version`'s content identity. `path` is the discovered
/// candidate's own rooted address; excluding it scopes resolution to genuine
/// hardlinks (distinct locations, one inode) and leaves same-location re-scans to the
/// normal ingest path. Returns `None` when no other live local location shares
/// the physical object (a first sighting, a same-path re-scan, or a
/// byte-identical copy on a different inode).
pub async fn find_live_hardlink_location_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    dev: u64,
    ino: u64,
    storage_root_id: StorageRootId,
    provider_relative_locator: &ProviderRelativeLocator,
) -> Result<Option<ScanFactMatch>, VoomError> {
    let row = sqlx::query(
        "SELECT sff.file_location_id, fl.file_version_id, fv.content_hash, fv.size_bytes \
         FROM scan_file_facts sff \
         JOIN file_locations fl ON fl.id = sff.file_location_id \
         JOIN file_versions fv ON fv.id = fl.file_version_id \
         JOIN library_roots existing_root ON existing_root.id = fl.storage_root_id \
         JOIN library_roots candidate_root ON candidate_root.id = ? \
         WHERE sff.dev = ? AND sff.ino = ? \
           AND (fl.storage_root_id != ? OR fl.provider_relative_locator != ?) \
           AND existing_root.owner_node_id = candidate_root.owner_node_id \
         AND fl.retired_at IS NULL \
           AND fl.address_state = 'rooted' \
         AND fv.retired_at IS NULL \
         ORDER BY fl.id ASC \
         LIMIT 1",
    )
    .bind(i64_from_u64(
        storage_root_id.0,
        "candidate storage root id",
    )?)
    .bind(i64_from_u64(
        dev,
        concat!(module_path!(), ": ", stringify!(dev)),
    )?)
    .bind(i64_from_u64(
        ino,
        concat!(module_path!(), ": ", stringify!(ino)),
    )?)
    .bind(i64_from_u64(
        storage_root_id.0,
        "candidate storage root id",
    )?)
    .bind(provider_relative_locator.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("scan_file_facts hardlink lookup", e))?;

    let Some(row) = row else {
        return Ok(None);
    };
    let file_location_id: i64 = row
        .try_get("file_location_id")
        .map_err(|e| VoomError::database_context("scan_file_facts location id", e))?;
    let file_version_id: i64 = row
        .try_get("file_version_id")
        .map_err(|e| VoomError::database_context("scan_file_facts version id", e))?;
    let content_hash: String = row
        .try_get("content_hash")
        .map_err(|e| VoomError::database_context("scan_file_facts content hash", e))?;
    let size_bytes: i64 = row
        .try_get("size_bytes")
        .map_err(|e| VoomError::database_context("scan_file_facts size bytes", e))?;
    Ok(Some(ScanFactMatch {
        file_location_id: FileLocationId(u64_from_i64(
            file_location_id,
            concat!(module_path!(), ": ", stringify!(file_location_id)),
        )?),
        file_version_id: FileVersionId(u64_from_i64(
            file_version_id,
            concat!(module_path!(), ": ", stringify!(file_version_id)),
        )?),
        content_hash,
        size_bytes: u64_from_i64(
            size_bytes,
            concat!(module_path!(), ": ", stringify!(size_bytes)),
        )?,
    }))
}

#[cfg(test)]
#[path = "scan_facts_test.rs"]
mod tests;
