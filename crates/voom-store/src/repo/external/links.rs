//! `external_system_links` rows: external-ref ↔ internal-target correspondences
//! a read-only sync records. The durable primitives the Sprint 20 sync loop
//! reconciles.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{ExternalSystemId, ExternalSystemLinkId, VoomError};

use super::super::common::{i64_from_u64, iso8601, parse_iso8601, u64_from_i64};
use super::SqliteExternalSystemRepo;

/// Internal entity an external ref maps to. Mirrors the
/// `external_system_links.target_type` CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalLinkTargetType {
    MediaWork,
    MediaVariant,
    AssetBundle,
    FileAsset,
}

str_enum!(ExternalLinkTargetType, "external_system_links.target_type", {
    MediaWork => "media_work",
    MediaVariant => "media_variant",
    AssetBundle => "asset_bundle",
    FileAsset => "file_asset",
});

/// Input for recording a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExternalLink {
    pub external_system_id: ExternalSystemId,
    pub target_type: ExternalLinkTargetType,
    pub target_id: u64,
    pub external_ref: String,
}

/// A durable link row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSystemLink {
    pub id: ExternalSystemLinkId,
    pub external_system_id: ExternalSystemId,
    pub target_type: ExternalLinkTargetType,
    pub target_id: u64,
    pub external_ref: String,
    pub created_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
}

const COLS: &str =
    "id, external_system_id, target_type, target_id, external_ref, created_at, retired_at";

impl SqliteExternalSystemRepo {
    /// Record a link inside the caller's transaction.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn record_link_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewExternalLink,
        now: OffsetDateTime,
    ) -> Result<ExternalSystemLink, VoomError> {
        let ts = iso8601(now)?;
        let res = sqlx::query(
            "INSERT INTO external_system_links \
             (external_system_id, target_type, target_id, external_ref, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.external_system_id.0))
        .bind(input.target_type.as_str())
        .bind(i64_from_u64(input.target_id))
        .bind(&input.external_ref)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("external_system_links insert", e))?;
        Ok(ExternalSystemLink {
            id: ExternalSystemLinkId(u64_from_i64(res.last_insert_rowid())),
            external_system_id: input.external_system_id,
            target_type: input.target_type,
            target_id: input.target_id,
            external_ref: input.external_ref,
            created_at: now,
            retired_at: None,
        })
    }

    /// List active links for a system in id order.
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn list_links(
        &self,
        system_id: ExternalSystemId,
    ) -> Result<Vec<ExternalSystemLink>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM external_system_links \
             WHERE external_system_id = ? AND retired_at IS NULL ORDER BY id ASC"
        ))
        .bind(i64_from_u64(system_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("external_system_links list", e))?;
        rows.iter().map(row_to_link).collect()
    }

    /// List active links for a system inside the caller's transaction.
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn list_links_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        system_id: ExternalSystemId,
    ) -> Result<Vec<ExternalSystemLink>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM external_system_links \
             WHERE external_system_id = ? AND retired_at IS NULL ORDER BY id ASC"
        ))
        .bind(i64_from_u64(system_id.0))
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("external_system_links list_in_tx", e))?;
        rows.iter().map(row_to_link).collect()
    }

    /// Retire an active link inside the caller's transaction. Returns the
    /// retired row, or `None` when no active link has that id.
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn retire_link_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: ExternalSystemLinkId,
        now: OffsetDateTime,
    ) -> Result<Option<ExternalSystemLink>, VoomError> {
        let ts = iso8601(now)?;
        let affected = sqlx::query(
            "UPDATE external_system_links SET retired_at = ? WHERE id = ? AND retired_at IS NULL",
        )
        .bind(&ts)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("external_system_links retire", e))?
        .rows_affected();
        if affected == 0 {
            return Ok(None);
        }
        let row = sqlx::query(&format!(
            "SELECT {COLS} FROM external_system_links WHERE id = ?"
        ))
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("external_system_links retire get", e))?;
        row.as_ref().map(row_to_link).transpose()
    }
}

fn row_to_link(row: &SqliteRow) -> Result<ExternalSystemLink, VoomError> {
    let map = |field: &'static str| {
        move |e: sqlx::Error| {
            VoomError::database_context(format!("external_system_links.{field}"), e)
        }
    };
    let id: i64 = row.try_get("id").map_err(map("id"))?;
    let system_id: i64 = row
        .try_get("external_system_id")
        .map_err(map("external_system_id"))?;
    let target_type: String = row.try_get("target_type").map_err(map("target_type"))?;
    let target_id: i64 = row.try_get("target_id").map_err(map("target_id"))?;
    let created_at: String = row.try_get("created_at").map_err(map("created_at"))?;
    let retired_at: Option<String> = row.try_get("retired_at").map_err(map("retired_at"))?;
    Ok(ExternalSystemLink {
        id: ExternalSystemLinkId(u64_from_i64(id)),
        external_system_id: ExternalSystemId(u64_from_i64(system_id)),
        target_type: ExternalLinkTargetType::parse(&target_type)?,
        target_id: u64_from_i64(target_id),
        external_ref: row.try_get("external_ref").map_err(map("external_ref"))?,
        created_at: parse_iso8601(&created_at)?,
        retired_at: retired_at.as_deref().map(parse_iso8601).transpose()?,
    })
}

#[cfg(test)]
#[path = "links_test.rs"]
mod tests;
