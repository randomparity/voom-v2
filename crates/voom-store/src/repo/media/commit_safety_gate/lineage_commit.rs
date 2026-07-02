//! Lineage-commit safety-gate check.
//!
//! The destructive-commit gate (`prepare_/authorize_/finalize_destructive_commit`)
//! is built around delete/replace/move `CommitTarget`s and its own
//! `BEGIN IMMEDIATE` transactions. The artifact commit path is a lineage /
//! additive operation — it produces a new `FileVersion` on an existing
//! `FileAsset` and installs bytes at a new path. This module provides the one
//! primitive that path needs: a clock-aware blocking-lease check that runs on
//! the caller's (host commit) transaction, with no nested gate transaction.
//!
//! See `docs/adr/0017-commit-gate-lineage-commit-check.md` and
//! `docs/specs/commit-safety-gate-wiring-270.md`.

use std::collections::BTreeSet;

use sqlx::Row;
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_core::ids::{BundleId, FileAssetId, FileLocationId, FileVersionId, UseLeaseId};

use crate::repo::common::{i64_from_u64, iso8601, u64_from_i64};
use crate::repo::media::identity::IdentityRepo;
use crate::repo::media::use_leases::LeaseScope;

use super::AffectedScopeClosure;

/// Result of the lineage-commit lease check.
///
/// `evaluated_lease_ids` is every live, non-expired lease (blocking or
/// advisory) whose scope overlaps the commit's affected closure — the audit
/// record of what the gate considered. `blocking` is the lowest-id overlapping
/// lease with `blocking_mode = 'blocking'`, if any; its presence fails the
/// commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageCommitLeaseCheck {
    pub closure: AffectedScopeClosure,
    pub evaluated_lease_ids: Vec<UseLeaseId>,
    pub blocking: Option<(UseLeaseId, LeaseScope)>,
}

/// Check the affected-scope closure of a lineage commit for blocking use
/// leases, on the caller's transaction.
///
/// The closure is anchored on the source version being committed: the source
/// `FileAsset`, the `source_file_version_id`, every live `FileLocation` of that
/// version, and every `AssetBundle` the asset belongs to. A lease blocks when
/// it is live (`release_reason IS NULL`), not TTL-expired against `now`, has
/// `blocking_mode = 'blocking'`, and overlaps any closure member (design
/// §1191–1243). Manual locks (`ttl_bound = 0`) are never TTL-expired and block
/// until terminal.
///
/// # Errors
///
/// Returns `VoomError::Database` on any storage failure (closure walk or lease
/// query). Callers treat an error as fail-closed — the commit must not proceed.
pub async fn check_lineage_commit_leases_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    file_asset_id: FileAssetId,
    file_version_id: FileVersionId,
    now: OffsetDateTime,
) -> Result<LineageCommitLeaseCheck, VoomError> {
    let closure = build_lineage_closure(tx, identity_repo, file_asset_id, file_version_id).await?;
    let rows = overlapping_live_leases_in_tx(tx, &closure, now).await?;
    let evaluated_lease_ids: Vec<UseLeaseId> = rows.iter().map(|(id, _, _)| *id).collect();
    let blocking = rows
        .into_iter()
        .find_map(|(id, is_blocking, scope)| is_blocking.then_some((id, scope)));
    Ok(LineageCommitLeaseCheck {
        closure,
        evaluated_lease_ids,
        blocking,
    })
}

/// Build the affected-scope closure for a lineage commit anchored on
/// `(file_asset_id, file_version_id)`. DB-internal only — no external alias
/// resolver (none is registered in production).
async fn build_lineage_closure(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    file_asset_id: FileAssetId,
    file_version_id: FileVersionId,
) -> Result<AffectedScopeClosure, VoomError> {
    let mut file_assets: BTreeSet<FileAssetId> = BTreeSet::new();
    file_assets.insert(file_asset_id);
    let mut file_versions: BTreeSet<FileVersionId> = BTreeSet::new();
    file_versions.insert(file_version_id);
    let file_locations: BTreeSet<FileLocationId> = identity_repo
        .list_live_file_locations_by_version_in_tx(tx, file_version_id)
        .await?
        .into_iter()
        .collect();

    let mut bundles: BTreeSet<BundleId> = BTreeSet::new();
    let bundle_rows: Vec<i64> =
        sqlx::query_scalar("SELECT bundle_id FROM asset_bundle_members WHERE file_asset_id = ?")
            .bind(i64_from_u64(file_asset_id.0))
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("asset_bundle_members lineage lookup", e))?;
    for raw in bundle_rows {
        bundles.insert(BundleId(u64_from_i64(raw)));
    }

    Ok(AffectedScopeClosure {
        file_assets,
        file_versions,
        file_locations,
        bundles,
        resolution_warnings: Vec::new(),
    })
}

/// Every live, non-TTL-expired lease overlapping `closure`, as
/// `(lease_id, is_blocking, scope)`, ordered by `id ASC`. Clock-aware: a
/// TTL-bound lease past `expires_at` is treated as expired and excluded even
/// before cleanup has run (design §1235–1241). Terminal leases
/// (`release_reason IS NOT NULL`) are excluded.
async fn overlapping_live_leases_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
    now: OffsetDateTime,
) -> Result<Vec<(UseLeaseId, bool, LeaseScope)>, VoomError> {
    let assets_json = serde_json::to_string(&closure.file_assets)
        .map_err(|e| VoomError::Internal(format!("encode closure.file_assets: {e}")))?;
    let bundles_json = serde_json::to_string(&closure.bundles)
        .map_err(|e| VoomError::Internal(format!("encode closure.bundles: {e}")))?;
    let versions_json = serde_json::to_string(&closure.file_versions)
        .map_err(|e| VoomError::Internal(format!("encode closure.file_versions: {e}")))?;
    let locations_json = serde_json::to_string(&closure.file_locations)
        .map_err(|e| VoomError::Internal(format!("encode closure.file_locations: {e}")))?;
    let now_iso = iso8601(now)?;

    // `json_each` produces one row per element of the bound JSON array; the
    // UNION-shaped OR across the four scope columns is the four-granularity
    // overlap check. Freshness: live (`release_reason IS NULL`) and not
    // TTL-expired. Manual locks (`ttl_bound = 0`) and TTL leases still within
    // `expires_at` pass the freshness clause.
    let rows = sqlx::query(
        "SELECT id, blocking_mode, scope_asset_id, scope_bundle_id, scope_version_id, scope_location_id \
         FROM asset_use_leases \
         WHERE release_reason IS NULL \
           AND (ttl_bound = 0 OR expires_at IS NULL OR expires_at >= ?) \
           AND ( \
               scope_asset_id    IN (SELECT value FROM json_each(?)) \
            OR scope_bundle_id   IN (SELECT value FROM json_each(?)) \
            OR scope_version_id  IN (SELECT value FROM json_each(?)) \
            OR scope_location_id IN (SELECT value FROM json_each(?)) \
           ) \
         ORDER BY id ASC",
    )
    .bind(&now_iso)
    .bind(&assets_json)
    .bind(&bundles_json)
    .bind(&versions_json)
    .bind(&locations_json)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("lineage-commit blocking-lease overlap", e))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row
            .try_get("id")
            .map_err(|e| VoomError::database_context("lineage-lease row id", e))?;
        let blocking_mode: String = row
            .try_get("blocking_mode")
            .map_err(|e| VoomError::database_context("lineage-lease row blocking_mode", e))?;
        let scope = decode_scope(row)?;
        out.push((
            UseLeaseId(u64_from_i64(id)),
            blocking_mode == "blocking",
            scope,
        ));
    }
    Ok(out)
}

/// Decode the exactly-one-of four `scope_*_id` columns into a `LeaseScope`.
fn decode_scope(row: &sqlx::sqlite::SqliteRow) -> Result<LeaseScope, VoomError> {
    let sa: Option<i64> = row
        .try_get("scope_asset_id")
        .map_err(|e| VoomError::database_context("lineage-lease scope_asset_id", e))?;
    let sb: Option<i64> = row
        .try_get("scope_bundle_id")
        .map_err(|e| VoomError::database_context("lineage-lease scope_bundle_id", e))?;
    let sv: Option<i64> = row
        .try_get("scope_version_id")
        .map_err(|e| VoomError::database_context("lineage-lease scope_version_id", e))?;
    let sl: Option<i64> = row
        .try_get("scope_location_id")
        .map_err(|e| VoomError::database_context("lineage-lease scope_location_id", e))?;
    match (sa, sb, sv, sl) {
        (Some(v), None, None, None) => Ok(LeaseScope::Asset(FileAssetId(u64_from_i64(v)))),
        (None, Some(v), None, None) => Ok(LeaseScope::Bundle(BundleId(u64_from_i64(v)))),
        (None, None, Some(v), None) => Ok(LeaseScope::Version(FileVersionId(u64_from_i64(v)))),
        (None, None, None, Some(v)) => Ok(LeaseScope::Location(FileLocationId(u64_from_i64(v)))),
        other => Err(VoomError::database(format!(
            "lineage-lease row: scope_*_id columns are not exactly-one: {other:?}"
        ))),
    }
}

#[cfg(test)]
#[path = "lineage_commit_test.rs"]
mod tests;
