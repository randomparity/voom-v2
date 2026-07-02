//! `SqliteQualityScoringProfileRepo` — CRUD over the durable quality scoring
//! registry (migration 0004 §10.3, #285/T16).
//!
//! A quality scoring profile is named, versioned operator configuration a future
//! daemon and the retention planner read rather than invent (design doc ->
//! Quality Scoring Registry). No scorer is wired yet; this repo provides CRUD
//! only. `definition` is an open-ended passthrough JSON object (Class P in
//! docs/payload-contract-inventory.md) — validated as a JSON object here, never
//! deserialized into a typed struct. Shape and rationale: `docs/adr/0032`.

use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::VoomError;

use super::Repository;
use super::common::{iso8601, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64};

/// Mutable fields of a scoring profile, supplied on create and full-replace
/// update. `name` is the stable, `UNIQUE` key; update matches on it and never
/// renames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewQualityScoringProfile {
    pub name: String,
    pub version: u32,
    pub definition: Value,
}

/// A durable quality scoring profile row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityScoringProfile {
    pub id: u64,
    pub name: String,
    pub version: u32,
    pub definition: Value,
    pub created_at: OffsetDateTime,
    /// Soft-retire marker. `None` for active profiles; a retired profile is
    /// hidden from `list` but still resolves by name.
    pub retired_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct SqliteQualityScoringProfileRepo {
    pool: SqlitePool,
}

impl SqliteQualityScoringProfileRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteQualityScoringProfileRepo {}

const COLS: &str = "id, name, version, definition, created_at, retired_at";

/// Reject an empty name and a `definition` that is not a JSON object. The 0004
/// table has no non-empty-name CHECK, and the `definition` is a passthrough blob
/// with no typed schema yet, but an object is the only shape dimension weights
/// can take, so a scalar or array is a caller mistake.
fn validate_input(input: &NewQualityScoringProfile) -> Result<(), VoomError> {
    if input.name.trim().is_empty() {
        return Err(VoomError::Config(
            "quality scoring profile name must not be empty".to_owned(),
        ));
    }
    if !input.definition.is_object() {
        return Err(VoomError::Config(
            "quality scoring profile definition must be a JSON object".to_owned(),
        ));
    }
    Ok(())
}

impl SqliteQualityScoringProfileRepo {
    /// Insert a new scoring profile. Rejects a non-object `definition` with
    /// [`VoomError::Config`] and a duplicate `name` with [`VoomError::Conflict`].
    pub async fn create(
        &self,
        input: NewQualityScoringProfile,
        now: OffsetDateTime,
    ) -> Result<QualityScoringProfile, VoomError> {
        validate_input(&input)?;
        let definition = serialize_json(&input.definition, "quality_scoring_profiles.definition")?;
        let ts = iso8601(now)?;
        let res = sqlx::query(
            "INSERT INTO quality_scoring_profiles (name, version, definition, created_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&input.name)
        .bind(i64::from(input.version))
        .bind(&definition)
        .bind(&ts)
        .execute(&self.pool)
        .await;
        match res {
            Ok(res) => Ok(QualityScoringProfile {
                id: u64_from_i64(res.last_insert_rowid()),
                name: input.name,
                version: input.version,
                definition: input.definition,
                created_at: now,
                retired_at: None,
            }),
            Err(err) => Err(self.classify_insert_error(&input.name, err).await),
        }
    }

    async fn classify_insert_error(&self, name: &str, err: sqlx::Error) -> VoomError {
        match self.get_by_name(name).await {
            Ok(Some(_)) => {
                VoomError::Conflict(format!("quality scoring profile {name:?} already exists"))
            }
            _ => VoomError::database_context("quality_scoring_profiles create", err),
        }
    }

    /// Resolve a profile by name regardless of retire status.
    pub async fn get_by_name(
        &self,
        name: &str,
    ) -> Result<Option<QualityScoringProfile>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {COLS} FROM quality_scoring_profiles WHERE name = ?"
        ))
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("quality_scoring_profiles get_by_name", e))?;
        row.as_ref().map(row_to_profile).transpose()
    }

    /// List active (non-retired) profiles ordered by name.
    pub async fn list(&self) -> Result<Vec<QualityScoringProfile>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM quality_scoring_profiles \
             WHERE retired_at IS NULL ORDER BY name ASC"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("quality_scoring_profiles list", e))?;
        rows.iter().map(row_to_profile).collect()
    }

    /// Full-replace update keyed by `input.name`, replacing `version` and
    /// `definition`. Returns `None` when no profile has that name. Rejects a
    /// non-object `definition` with [`VoomError::Config`].
    pub async fn update(
        &self,
        input: NewQualityScoringProfile,
    ) -> Result<Option<QualityScoringProfile>, VoomError> {
        validate_input(&input)?;
        let definition = serialize_json(&input.definition, "quality_scoring_profiles.definition")?;
        let affected = sqlx::query(
            "UPDATE quality_scoring_profiles SET version = ?, definition = ? WHERE name = ?",
        )
        .bind(i64::from(input.version))
        .bind(&definition)
        .bind(&input.name)
        .execute(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("quality_scoring_profiles update", e))?
        .rows_affected();
        if affected == 0 {
            return Ok(None);
        }
        self.get_by_name(&input.name).await
    }

    /// Soft-retire the profile by name, stamping `retired_at`. Idempotent: a
    /// re-retire preserves the first stamp. Returns `None` when no profile has
    /// that name.
    pub async fn retire(
        &self,
        name: &str,
        now: OffsetDateTime,
    ) -> Result<Option<QualityScoringProfile>, VoomError> {
        let ts = iso8601(now)?;
        sqlx::query(
            "UPDATE quality_scoring_profiles SET retired_at = ? \
             WHERE name = ? AND retired_at IS NULL",
        )
        .bind(&ts)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("quality_scoring_profiles retire", e))?;
        self.get_by_name(name).await
    }
}

fn row_to_profile(row: &SqliteRow) -> Result<QualityScoringProfile, VoomError> {
    let map = |field: &'static str| {
        move |e: sqlx::Error| {
            VoomError::database_context(format!("quality_scoring_profiles.{field}"), e)
        }
    };
    let id: i64 = row.try_get("id").map_err(map("id"))?;
    let version: i64 = row.try_get("version").map_err(map("version"))?;
    let definition: String = row.try_get("definition").map_err(map("definition"))?;
    let created_at: String = row.try_get("created_at").map_err(map("created_at"))?;
    let retired_at: Option<String> = row.try_get("retired_at").map_err(map("retired_at"))?;
    let definition: Value = serde_json::from_str(&definition).map_err(|e| {
        VoomError::database(format!(
            "quality_scoring_profiles.definition invalid JSON: {e}"
        ))
    })?;
    Ok(QualityScoringProfile {
        id: u64_from_i64(id),
        name: row.try_get("name").map_err(map("name"))?,
        version: u32_from_i64(version)?,
        definition,
        created_at: parse_iso8601(&created_at)?,
        retired_at: retired_at.as_deref().map(parse_iso8601).transpose()?,
    })
}

#[cfg(test)]
#[path = "quality_scoring_profiles_test.rs"]
mod tests;
