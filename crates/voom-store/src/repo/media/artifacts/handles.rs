use super::{
    ArtifactExpectedFacts, ArtifactHandle, ArtifactHandleFacts, ArtifactHandleId, ArtifactLineage,
    ArtifactLocation, ArtifactLocationId, ArtifactLocationKind, FileVersionId,
    LiveArtifactLocation, NewArtifactHandle, NewArtifactLineage, NewArtifactLocation,
    OffsetDateTime, SqliteArtifactRepo, VoomError, checked_sqlite_id,
};
use sqlx::Row;

use super::super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
};

impl SqliteArtifactRepo {
    pub async fn create_handle_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewArtifactHandle,
    ) -> Result<ArtifactHandle, VoomError> {
        let access = serialize_json(
            &input.allowed_access_modes,
            "artifact_handles.allowed_access_modes",
        )?;
        let lineage = match &input.source_lineage {
            None => None,
            Some(v) => Some(serialize_json(v, "source_lineage")?),
        };
        let ts = iso8601(input.created_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_handles \
             (size_bytes, checksum, privacy_class, durability_class, \
              allowed_access_modes, mutability, source_lineage, file_version_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(input.size_bytes)
        .bind(&input.checksum)
        .bind(&input.privacy_class)
        .bind(&input.durability_class)
        .bind(access)
        .bind(&input.mutability)
        .bind(lineage)
        .bind(
            input
                .file_version_id
                .map(|id| i64_from_u64(id.0, concat!(module_path!(), ": ", stringify!(id.0))))
                .transpose()?,
        )
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_handles insert", e))?;
        Ok(ArtifactHandle {
            id: ArtifactHandleId(u64_from_i64(
                res.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res.last_insert_rowid())),
            )?),
            file_version_id: input.file_version_id,
            privacy_class: input.privacy_class,
            durability_class: input.durability_class,
            allowed_access_modes: input.allowed_access_modes,
            mutability: input.mutability,
            created_at: input.created_at,
        })
    }

    pub async fn create_handle(
        &self,
        input: NewArtifactHandle,
    ) -> Result<ArtifactHandle, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.create_handle_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    pub async fn record_location_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewArtifactLocation,
    ) -> Result<ArtifactLocation, VoomError> {
        let ts = iso8601(input.observed_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_locations \
             (artifact_handle_id, kind, value, observed_at) VALUES (?, ?, ?, ?)",
        )
        .bind(i64_from_u64(
            input.artifact_handle_id.0,
            concat!(module_path!(), ": ", stringify!(input.artifact_handle_id.0)),
        )?)
        .bind(input.kind.as_str())
        .bind(&input.value)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_locations insert", e))?;
        Ok(ArtifactLocation {
            id: ArtifactLocationId(u64_from_i64(
                res.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res.last_insert_rowid())),
            )?),
            artifact_handle_id: input.artifact_handle_id,
            kind: input.kind,
            value: input.value,
            observed_at: input.observed_at,
            retired_at: None,
        })
    }

    pub async fn record_location(
        &self,
        input: NewArtifactLocation,
    ) -> Result<ArtifactLocation, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.record_location_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    pub async fn retire_location_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        location_id: ArtifactLocationId,
        now: OffsetDateTime,
    ) -> Result<ArtifactHandleId, VoomError> {
        let ts = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE artifact_locations SET retired_at = ? \
             WHERE id = ? AND retired_at IS NULL",
        )
        .bind(&ts)
        .bind(i64_from_u64(
            location_id.0,
            concat!(module_path!(), ": ", stringify!(location_id.0)),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_locations retire", e))?;
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
                .bind(i64_from_u64(
                    location_id.0,
                    concat!(module_path!(), ": ", stringify!(location_id.0)),
                )?)
                .fetch_one(&mut **tx)
                .await
                .map_err(|e| VoomError::database_context("artifact_locations handle lookup", e))?;
        Ok(ArtifactHandleId(u64_from_i64(
            handle_id,
            concat!(module_path!(), ": ", stringify!(handle_id)),
        )?))
    }

    pub async fn retire_location(
        &self,
        location_id: ArtifactLocationId,
        now: OffsetDateTime,
    ) -> Result<ArtifactHandleId, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self
            .retire_location_in_tx(&mut tx, location_id, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    pub async fn record_lineage_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewArtifactLineage,
    ) -> Result<ArtifactLineage, VoomError> {
        let ts = iso8601(input.recorded_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_lineage \
             (parent_artifact_id, child_artifact_id, operation, recorded_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(i64_from_u64(
            input.parent_artifact_id.0,
            concat!(module_path!(), ": ", stringify!(input.parent_artifact_id.0)),
        )?)
        .bind(i64_from_u64(
            input.child_artifact_id.0,
            concat!(module_path!(), ": ", stringify!(input.child_artifact_id.0)),
        )?)
        .bind(&input.operation)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_lineage insert", e))?;
        Ok(ArtifactLineage {
            id: u64_from_i64(
                res.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res.last_insert_rowid())),
            )?,
        })
    }

    pub async fn record_lineage(
        &self,
        input: NewArtifactLineage,
    ) -> Result<ArtifactLineage, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::database_context("begin", e))?;
        let out = self.record_lineage_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::database_context("commit", e))?;
        Ok(out)
    }

    pub async fn get_handle(
        &self,
        id: ArtifactHandleId,
    ) -> Result<Option<ArtifactHandle>, VoomError> {
        let row = sqlx::query(
            "SELECT id, file_version_id, privacy_class, durability_class, allowed_access_modes, \
                    mutability, created_at FROM artifact_handles WHERE id = ?",
        )
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("artifact_handles get", e))?;
        row.as_ref().map(row_to_handle).transpose()
    }

    /// List handle ids newest first using an exclusive keyset cursor.
    pub async fn list_handle_ids(
        &self,
        after_id: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<ArtifactHandleId>, VoomError> {
        let after_id = after_id
            .map(|id| checked_sqlite_id(id, "artifact_handles after_id"))
            .transpose()?;
        let rows: Vec<i64> = match limit {
            Some(limit) => {
                sqlx::query_scalar(
                    "SELECT id FROM artifact_handles \
                     WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2",
                )
                .bind(after_id)
                .bind(i64::from(limit))
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_scalar(
                    "SELECT id FROM artifact_handles \
                     WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC",
                )
                .bind(after_id)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|error| VoomError::database_context("artifact_handles list", error))?;
        rows.into_iter()
            .map(|id| {
                u64::try_from(id).map(ArtifactHandleId).map_err(|error| {
                    VoomError::database_context("artifact_handles.id negative", error)
                })
            })
            .collect()
    }

    /// Return handle metadata while preserving optional inspection facts.
    pub async fn handle_facts(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<ArtifactHandleFacts, VoomError> {
        let row = sqlx::query(
            "SELECT id, file_version_id, privacy_class, durability_class, allowed_access_modes, \
                    mutability, created_at, size_bytes, checksum \
             FROM artifact_handles WHERE id = ?",
        )
        .bind(checked_sqlite_id(handle_id.0, "artifact handle id")?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("artifact_handles facts", error))?
        .ok_or_else(|| VoomError::NotFound(format!("artifact_handles {handle_id} missing")))?;
        let size_bytes: Option<i64> = row
            .try_get("size_bytes")
            .map_err(|error| map_row_err("artifact_handles.size_bytes", error))?;
        let checksum = row
            .try_get("checksum")
            .map_err(|error| map_row_err("artifact_handles.checksum", error))?;
        Ok(ArtifactHandleFacts {
            handle: row_to_handle(&row)?,
            size_bytes: size_bytes
                .map(|size| {
                    u64::try_from(size).map_err(|error| {
                        VoomError::database_context("artifact_handles.size_bytes negative", error)
                    })
                })
                .transpose()?,
            checksum,
        })
    }

    /// Return the complete expected artifact facts required for verification.
    pub async fn require_expected_facts(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<ArtifactExpectedFacts, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| VoomError::database_context("begin", error))?;
        let facts = self
            .require_expected_facts_in_tx(&mut tx, handle_id)
            .await?;
        tx.commit()
            .await
            .map_err(|error| VoomError::database_context("commit", error))?;
        Ok(facts)
    }

    /// Return complete expected artifact facts in the caller's transaction.
    pub async fn require_expected_facts_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        handle_id: ArtifactHandleId,
    ) -> Result<ArtifactExpectedFacts, VoomError> {
        let row: Option<(Option<i64>, Option<i64>, Option<String>)> = sqlx::query_as(
            "SELECT file_version_id, size_bytes, checksum \
             FROM artifact_handles WHERE id = ?",
        )
        .bind(checked_sqlite_id(handle_id.0, "artifact handle id")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("artifact_handles expected facts", error))?;
        let Some((source_file_version_id, size_bytes, checksum)) = row else {
            return Err(VoomError::NotFound(format!(
                "artifact_handles {handle_id} missing"
            )));
        };
        let size_bytes = size_bytes.ok_or_else(|| {
            VoomError::Config(format!(
                "artifact_handle {handle_id} missing expected size_bytes"
            ))
        })?;
        let size_bytes = u64::try_from(size_bytes).map_err(|error| {
            VoomError::database_context("artifact_handles.size_bytes negative", error)
        })?;
        let checksum = checksum.ok_or_else(|| {
            VoomError::Config(format!(
                "artifact_handle {handle_id} missing expected checksum"
            ))
        })?;
        Ok(ArtifactExpectedFacts {
            source_file_version_id: source_file_version_id
                .map(|id| {
                    u64::try_from(id).map(FileVersionId).map_err(|error| {
                        VoomError::database_context(
                            "artifact_handles.file_version_id negative",
                            error,
                        )
                    })
                })
                .transpose()?,
            size_bytes,
            checksum,
        })
    }

    /// Return the sole live location of `kind`, conflicting on ambiguity.
    pub async fn live_location_of_kind_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        handle_id: ArtifactHandleId,
        kind: ArtifactLocationKind,
    ) -> Result<Option<LiveArtifactLocation>, VoomError> {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, kind, value FROM artifact_locations \
             WHERE artifact_handle_id = ? AND kind = ? AND retired_at IS NULL \
             ORDER BY id ASC",
        )
        .bind(checked_sqlite_id(handle_id.0, "artifact handle id")?)
        .bind(kind.as_str())
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("artifact_locations live kind", error))?;
        match rows.as_slice() {
            [] => Ok(None),
            [(id, stored_kind, value)] => Ok(Some(LiveArtifactLocation {
                id: ArtifactLocationId(u64::try_from(*id).map_err(|error| {
                    VoomError::database_context("artifact_locations.id negative", error)
                })?),
                kind: ArtifactLocationKind::parse_database(stored_kind)?,
                value: value.clone(),
            })),
            _ => Err(VoomError::Conflict(format!(
                "artifact_handle {handle_id} must have at most one live {kind} location; found {}",
                rows.len()
            ))),
        }
    }

    /// Require one selected location to retain its exact live identity.
    pub async fn require_live_location_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        handle_id: ArtifactHandleId,
        location_id: ArtifactLocationId,
        kind: ArtifactLocationKind,
        value: &str,
    ) -> Result<(), VoomError> {
        let selected: Option<(i64, String, String, Option<String>)> = sqlx::query_as(
            "SELECT artifact_handle_id, kind, value, retired_at \
             FROM artifact_locations WHERE id = ?",
        )
        .bind(checked_sqlite_id(location_id.0, "artifact location id")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("artifact_locations require live", error))?;
        let Some((owner_id, stored_kind, stored_value, retired_at)) = selected else {
            return Err(VoomError::NotFound(format!(
                "artifact_locations {location_id} missing"
            )));
        };
        let owner_id = u64::try_from(owner_id).map_err(|error| {
            VoomError::database_context("artifact_locations.artifact_handle_id negative", error)
        })?;
        let stored_kind = ArtifactLocationKind::parse_database(&stored_kind)?;
        if owner_id != handle_id.0 || stored_kind != kind || stored_value != value {
            return Err(VoomError::Conflict(format!(
                "artifact_locations {location_id} no longer matches artifact_handle {handle_id}"
            )));
        }
        if retired_at.is_some() {
            return Err(VoomError::Config(format!(
                "artifact_location {location_id} is no longer live {kind}"
            )));
        }
        Ok(())
    }

    pub async fn list_locations_for_handle(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactLocation>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, artifact_handle_id, kind, value, observed_at, retired_at \
             FROM artifact_locations WHERE artifact_handle_id = ? AND retired_at IS NULL \
             ORDER BY id ASC",
        )
        .bind(i64_from_u64(
            handle_id.0,
            concat!(module_path!(), ": ", stringify!(handle_id.0)),
        )?)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("artifact_locations list", e))?;
        rows.iter().map(row_to_location).collect()
    }
}

pub(super) fn row_to_handle(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactHandle, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("artifacts", e))?;
    let file_version_id: Option<i64> = row
        .try_get("file_version_id")
        .map_err(|e| map_row_err("artifacts", e))?;
    let privacy_class: String = row
        .try_get("privacy_class")
        .map_err(|e| map_row_err("artifacts", e))?;
    let durability_class: String = row
        .try_get("durability_class")
        .map_err(|e| map_row_err("artifacts", e))?;
    let allowed_access_modes: String = row
        .try_get("allowed_access_modes")
        .map_err(|e| map_row_err("artifacts", e))?;
    let mutability: String = row
        .try_get("mutability")
        .map_err(|e| map_row_err("artifacts", e))?;
    let created: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("artifacts", e))?;
    Ok(ArtifactHandle {
        id: ArtifactHandleId(
            u64::try_from(id).map_err(|error| {
                VoomError::database_context("artifact_handles.id negative", error)
            })?,
        ),
        file_version_id: file_version_id
            .map(|value| {
                u64::try_from(value).map(FileVersionId).map_err(|error| {
                    VoomError::database_context("artifact_handles.file_version_id negative", error)
                })
            })
            .transpose()?,
        privacy_class,
        durability_class,
        allowed_access_modes: serde_json::from_str(&allowed_access_modes).map_err(|error| {
            VoomError::database_context("artifact_handles.allowed_access_modes", error)
        })?,
        mutability,
        created_at: parse_iso8601(&created)?,
    })
}

pub(super) fn row_to_location(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ArtifactLocation, VoomError> {
    let id: i64 = row.try_get("id").map_err(|e| map_row_err("artifacts", e))?;
    let handle_id: i64 = row
        .try_get("artifact_handle_id")
        .map_err(|e| map_row_err("artifacts", e))?;
    let kind: String = row
        .try_get("kind")
        .map_err(|e| map_row_err("artifacts", e))?;
    let value: String = row
        .try_get("value")
        .map_err(|e| map_row_err("artifacts", e))?;
    let observed: String = row
        .try_get("observed_at")
        .map_err(|e| map_row_err("artifacts", e))?;
    let retired: Option<String> = row
        .try_get("retired_at")
        .map_err(|e| map_row_err("artifacts", e))?;
    Ok(ArtifactLocation {
        id: ArtifactLocationId(u64_from_i64(
            id,
            concat!(module_path!(), ": ", stringify!(id)),
        )?),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(
            handle_id,
            concat!(module_path!(), ": ", stringify!(handle_id)),
        )?),
        kind: ArtifactLocationKind::parse_database(&kind)?,
        value,
        observed_at: parse_iso8601(&observed)?,
        retired_at: retired.map(|s| parse_iso8601(&s)).transpose()?,
    })
}
