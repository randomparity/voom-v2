//! `external_path_mappings` rows: internal↔external prefix translation. Pure
//! operator config — no events.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;
use voom_core::{ExternalPathMappingId, ExternalSystemId, VoomError};

use super::super::common::{i64_from_u64, iso8601, parse_iso8601, u64_from_i64};
use super::SqliteExternalSystemRepo;

/// Whether a mapping is read-only or read-write. Mirrors the
/// `external_path_mappings.visibility` CHECK. V1 external writes are out of
/// scope, but the column exists for the policy-gated write path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathVisibility {
    ReadOnly,
    ReadWrite,
}

str_enum!(PathVisibility, "external_path_mappings.visibility", {
    ReadOnly => "read_only",
    ReadWrite => "read_write",
});

/// Input for a new path mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExternalPathMapping {
    pub external_system_id: ExternalSystemId,
    pub internal_prefix: String,
    pub external_prefix: String,
    pub visibility: PathVisibility,
}

/// Mutable fields of a path mapping. `None` leaves a field unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathMappingUpdate {
    pub internal_prefix: Option<String>,
    pub external_prefix: Option<String>,
    pub visibility: Option<PathVisibility>,
}

/// A durable path-mapping row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalPathMapping {
    pub id: ExternalPathMappingId,
    pub external_system_id: ExternalSystemId,
    pub internal_prefix: String,
    pub external_prefix: String,
    pub visibility: PathVisibility,
    pub created_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
}

const COLS: &str =
    "id, external_system_id, internal_prefix, external_prefix, visibility, created_at, retired_at";

impl SqliteExternalSystemRepo {
    /// Create a path mapping. Rejects an unknown parent system with `NotFound`.
    ///
    /// # Errors
    /// Returns `NotFound` when `external_system_id` does not exist; propagates
    /// database and row-decode errors.
    pub async fn create_path_mapping(
        &self,
        input: NewExternalPathMapping,
        now: OffsetDateTime,
    ) -> Result<ExternalPathMapping, VoomError> {
        let ts = iso8601(now)?;
        let res = sqlx::query(
            "INSERT INTO external_path_mappings \
             (external_system_id, internal_prefix, external_prefix, visibility, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.external_system_id.0))
        .bind(&input.internal_prefix)
        .bind(&input.external_prefix)
        .bind(input.visibility.as_str())
        .bind(&ts)
        .execute(&self.pool)
        .await;
        let res = match res {
            Ok(res) => res,
            Err(err) => {
                return Err(self
                    .classify_mapping_insert(input.external_system_id, err)
                    .await);
            }
        };
        Ok(ExternalPathMapping {
            id: ExternalPathMappingId(u64_from_i64(res.last_insert_rowid())),
            external_system_id: input.external_system_id,
            internal_prefix: input.internal_prefix,
            external_prefix: input.external_prefix,
            visibility: input.visibility,
            created_at: now,
            retired_at: None,
        })
    }

    async fn classify_mapping_insert(
        &self,
        system_id: ExternalSystemId,
        err: sqlx::Error,
    ) -> VoomError {
        match self.get(system_id).await {
            Ok(None) => VoomError::NotFound(format!(
                "external path mapping: system id={system_id} not found"
            )),
            _ => VoomError::database_context("external_path_mappings insert", err),
        }
    }

    /// Fetch a path mapping by id (retired rows included).
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn get_path_mapping(
        &self,
        id: ExternalPathMappingId,
    ) -> Result<Option<ExternalPathMapping>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {COLS} FROM external_path_mappings WHERE id = ?"
        ))
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("external_path_mappings get", e))?;
        row.as_ref().map(row_to_mapping).transpose()
    }

    /// List active path mappings for a system in id order.
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn list_path_mappings(
        &self,
        system_id: ExternalSystemId,
    ) -> Result<Vec<ExternalPathMapping>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM external_path_mappings \
             WHERE external_system_id = ? AND retired_at IS NULL ORDER BY id ASC"
        ))
        .bind(i64_from_u64(system_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("external_path_mappings list", e))?;
        rows.iter().map(row_to_mapping).collect()
    }

    /// Apply a partial update to an active path mapping. Returns `None` when no
    /// active row has that id.
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn update_path_mapping(
        &self,
        id: ExternalPathMappingId,
        update: PathMappingUpdate,
    ) -> Result<Option<ExternalPathMapping>, VoomError> {
        let affected = sqlx::query(
            "UPDATE external_path_mappings \
             SET internal_prefix = COALESCE(?, internal_prefix), \
                 external_prefix = COALESCE(?, external_prefix), \
                 visibility = COALESCE(?, visibility) \
             WHERE id = ? AND retired_at IS NULL",
        )
        .bind(update.internal_prefix.as_deref())
        .bind(update.external_prefix.as_deref())
        .bind(update.visibility.map(PathVisibility::as_str))
        .bind(i64_from_u64(id.0))
        .execute(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("external_path_mappings update", e))?
        .rows_affected();
        if affected == 0 {
            return Ok(None);
        }
        self.get_path_mapping(id).await
    }

    /// Retire (soft-delete) an active path mapping. Returns `true` when a row
    /// was retired.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn retire_path_mapping(
        &self,
        id: ExternalPathMappingId,
        now: OffsetDateTime,
    ) -> Result<bool, VoomError> {
        let ts = iso8601(now)?;
        let affected = sqlx::query(
            "UPDATE external_path_mappings SET retired_at = ? WHERE id = ? AND retired_at IS NULL",
        )
        .bind(&ts)
        .bind(i64_from_u64(id.0))
        .execute(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("external_path_mappings retire", e))?
        .rows_affected();
        Ok(affected > 0)
    }
}

fn row_to_mapping(row: &SqliteRow) -> Result<ExternalPathMapping, VoomError> {
    let map = |field: &'static str| {
        move |e: sqlx::Error| {
            VoomError::database_context(format!("external_path_mappings.{field}"), e)
        }
    };
    let id: i64 = row.try_get("id").map_err(map("id"))?;
    let system_id: i64 = row
        .try_get("external_system_id")
        .map_err(map("external_system_id"))?;
    let visibility: String = row.try_get("visibility").map_err(map("visibility"))?;
    let created_at: String = row.try_get("created_at").map_err(map("created_at"))?;
    let retired_at: Option<String> = row.try_get("retired_at").map_err(map("retired_at"))?;
    Ok(ExternalPathMapping {
        id: ExternalPathMappingId(u64_from_i64(id)),
        external_system_id: ExternalSystemId(u64_from_i64(system_id)),
        internal_prefix: row
            .try_get("internal_prefix")
            .map_err(map("internal_prefix"))?,
        external_prefix: row
            .try_get("external_prefix")
            .map_err(map("external_prefix"))?,
        visibility: PathVisibility::parse(&visibility)?,
        created_at: parse_iso8601(&created_at)?,
        retired_at: retired_at.as_deref().map(parse_iso8601).transpose()?,
    })
}

#[cfg(test)]
#[path = "path_mappings_test.rs"]
mod tests;
