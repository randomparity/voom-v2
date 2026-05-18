//! `ArtifactRepo` — owns `artifact_handles` + `artifact_locations` + `artifact_lineage`.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{ArtifactHandleId, ArtifactLocationId, VoomError};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
};

#[derive(Debug, Clone)]
pub struct NewArtifactHandle {
    pub size_bytes: Option<i64>,
    pub checksum: Option<String>,
    pub privacy_class: String,
    pub durability_class: String,
    pub allowed_access_modes: Vec<String>,
    pub mutability: String,
    pub source_lineage: Option<JsonValue>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ArtifactHandle {
    pub id: ArtifactHandleId,
    pub privacy_class: String,
    pub durability_class: String,
    pub mutability: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct NewArtifactLocation {
    pub artifact_handle_id: ArtifactHandleId,
    pub kind: String,
    pub value: String,
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ArtifactLocation {
    pub id: ArtifactLocationId,
    pub artifact_handle_id: ArtifactHandleId,
    pub kind: String,
    pub value: String,
    pub observed_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct NewArtifactLineage {
    pub parent_artifact_id: ArtifactHandleId,
    pub child_artifact_id: ArtifactHandleId,
    pub operation: String,
    pub recorded_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ArtifactLineage {
    pub id: u64,
}

#[async_trait]
pub trait ArtifactRepo: Repository {
    async fn create_handle_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactHandle,
    ) -> Result<ArtifactHandle, VoomError>;
    async fn create_handle(&self, input: NewArtifactHandle) -> Result<ArtifactHandle, VoomError>;

    async fn record_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactLocation,
    ) -> Result<ArtifactLocation, VoomError>;
    async fn record_location(
        &self,
        input: NewArtifactLocation,
    ) -> Result<ArtifactLocation, VoomError>;

    /// Retire the given location. Returns the `ArtifactHandleId` the
    /// location belongs to, resolved from the row itself so the caller
    /// (and any event payload it builds) cannot disagree with the
    /// recorded relationship.
    async fn retire_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        location_id: ArtifactLocationId,
        now: OffsetDateTime,
    ) -> Result<ArtifactHandleId, VoomError>;
    async fn retire_location(
        &self,
        location_id: ArtifactLocationId,
        now: OffsetDateTime,
    ) -> Result<ArtifactHandleId, VoomError>;

    async fn record_lineage_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactLineage,
    ) -> Result<ArtifactLineage, VoomError>;
    async fn record_lineage(&self, input: NewArtifactLineage)
    -> Result<ArtifactLineage, VoomError>;

    async fn get_handle(&self, id: ArtifactHandleId) -> Result<Option<ArtifactHandle>, VoomError>;
    async fn list_locations_for_handle(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactLocation>, VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqliteArtifactRepo {
    pool: SqlitePool,
}

impl SqliteArtifactRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteArtifactRepo {}

#[async_trait]
impl ArtifactRepo for SqliteArtifactRepo {
    async fn create_handle_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactHandle,
    ) -> Result<ArtifactHandle, VoomError> {
        let access = serde_json::to_string(&input.allowed_access_modes)
            .map_err(|e| VoomError::Internal(format!("serialize allowed_access_modes: {e}")))?;
        let lineage = match &input.source_lineage {
            None => None,
            Some(v) => Some(serialize_json(v, "source_lineage")?),
        };
        let ts = iso8601(input.created_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_handles \
             (size_bytes, checksum, privacy_class, durability_class, \
              allowed_access_modes, mutability, source_lineage, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(input.size_bytes)
        .bind(&input.checksum)
        .bind(&input.privacy_class)
        .bind(&input.durability_class)
        .bind(access)
        .bind(&input.mutability)
        .bind(lineage)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_handles insert: {e}")))?;
        Ok(ArtifactHandle {
            id: ArtifactHandleId(u64_from_i64(res.last_insert_rowid())),
            privacy_class: input.privacy_class,
            durability_class: input.durability_class,
            mutability: input.mutability,
            created_at: input.created_at,
        })
    }

    async fn create_handle(&self, input: NewArtifactHandle) -> Result<ArtifactHandle, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.create_handle_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn record_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactLocation,
    ) -> Result<ArtifactLocation, VoomError> {
        let ts = iso8601(input.observed_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_locations \
             (artifact_handle_id, kind, value, observed_at) VALUES (?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.artifact_handle_id.0))
        .bind(&input.kind)
        .bind(&input.value)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_locations insert: {e}")))?;
        Ok(ArtifactLocation {
            id: ArtifactLocationId(u64_from_i64(res.last_insert_rowid())),
            artifact_handle_id: input.artifact_handle_id,
            kind: input.kind,
            value: input.value,
            observed_at: input.observed_at,
            retired_at: None,
        })
    }

    async fn record_location(
        &self,
        input: NewArtifactLocation,
    ) -> Result<ArtifactLocation, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.record_location_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn retire_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        location_id: ArtifactLocationId,
        now: OffsetDateTime,
    ) -> Result<ArtifactHandleId, VoomError> {
        let ts = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE artifact_locations SET retired_at = ? \
             WHERE id = ? AND retired_at IS NULL",
        )
        .bind(&ts)
        .bind(i64_from_u64(location_id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_locations retire: {e}")))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "retire rejected for location {location_id}: not live"
            )));
        }
        // Resolve the handle id from the row itself so the event payload's
        // artifact_handle_id is the location's true handle, not a caller
        // assertion ([[project_in_tx_reread_uses_tx_handle]]).
        let handle_id: i64 =
            sqlx::query_scalar("SELECT artifact_handle_id FROM artifact_locations WHERE id = ?")
                .bind(i64_from_u64(location_id.0))
                .fetch_one(&mut **tx)
                .await
                .map_err(|e| {
                    VoomError::Database(format!("artifact_locations handle lookup: {e}"))
                })?;
        Ok(ArtifactHandleId(u64_from_i64(handle_id)))
    }

    async fn retire_location(
        &self,
        location_id: ArtifactLocationId,
        now: OffsetDateTime,
    ) -> Result<ArtifactHandleId, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self
            .retire_location_in_tx(&mut tx, location_id, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn record_lineage_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactLineage,
    ) -> Result<ArtifactLineage, VoomError> {
        let ts = iso8601(input.recorded_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_lineage \
             (parent_artifact_id, child_artifact_id, operation, recorded_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.parent_artifact_id.0))
        .bind(i64_from_u64(input.child_artifact_id.0))
        .bind(&input.operation)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_lineage insert: {e}")))?;
        Ok(ArtifactLineage {
            id: u64_from_i64(res.last_insert_rowid()),
        })
    }

    async fn record_lineage(
        &self,
        input: NewArtifactLineage,
    ) -> Result<ArtifactLineage, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.record_lineage_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn get_handle(&self, id: ArtifactHandleId) -> Result<Option<ArtifactHandle>, VoomError> {
        let row = sqlx::query(
            "SELECT id, privacy_class, durability_class, mutability, created_at \
             FROM artifact_handles WHERE id = ?",
        )
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_handles get: {e}")))?;
        row.as_ref().map(row_to_handle).transpose()
    }

    async fn list_locations_for_handle(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactLocation>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, artifact_handle_id, kind, value, observed_at, retired_at \
             FROM artifact_locations WHERE artifact_handle_id = ? AND retired_at IS NULL \
             ORDER BY id ASC",
        )
        .bind(i64_from_u64(handle_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_locations list: {e}")))?;
        rows.iter().map(row_to_location).collect()
    }
}

fn row_to_handle(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactHandle, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let privacy_class: String = row
        .try_get("privacy_class")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let durability_class: String = row
        .try_get("durability_class")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let mutability: String = row
        .try_get("mutability")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let created: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("artifacts", &e))?;
    Ok(ArtifactHandle {
        id: ArtifactHandleId(u64_from_i64(id)),
        privacy_class,
        durability_class,
        mutability,
        created_at: parse_iso8601(&created)?,
    })
}

fn row_to_location(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactLocation, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let handle_id: i64 = row
        .try_get("artifact_handle_id")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let kind: String = row
        .try_get("kind")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let value: String = row
        .try_get("value")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let observed: String = row
        .try_get("observed_at")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let retired: Option<String> = row
        .try_get("retired_at")
        .map_err(|e| map_row_err("artifacts", &e))?;
    Ok(ArtifactLocation {
        id: ArtifactLocationId(u64_from_i64(id)),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(handle_id)),
        kind,
        value,
        observed_at: parse_iso8601(&observed)?,
        retired_at: retired.map(|s| parse_iso8601(&s)).transpose()?,
    })
}

#[cfg(test)]
#[path = "artifacts_test.rs"]
mod tests;
