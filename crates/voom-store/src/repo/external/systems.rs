//! `external_systems` rows: registration and health status.

use serde_json::Value as JsonValue;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{ExternalSystemId, VoomError};

use super::super::common::{i64_from_u64, iso8601, parse_iso8601, u64_from_i64};
use super::SqliteExternalSystemRepo;

/// Kind of external system. Mirrors the `external_systems.kind` CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSystemKind {
    Plex,
    Jellyfin,
    Emby,
    Radarr,
    Sonarr,
    Bazarr,
    S3,
    Filesystem,
    Custom,
}

str_enum!(ExternalSystemKind, "external_systems.kind", {
    Plex => "plex",
    Jellyfin => "jellyfin",
    Emby => "emby",
    Radarr => "radarr",
    Sonarr => "sonarr",
    Bazarr => "bazarr",
    S3 => "s3",
    Filesystem => "filesystem",
    Custom => "custom",
});

/// Recorded health of an external system. Mirrors the
/// `external_systems.health_status` CHECK. A newly registered system is always
/// `Unknown` until the first probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSystemHealth {
    Unknown,
    Healthy,
    Degraded,
    Unreachable,
}

str_enum!(ExternalSystemHealth, "external_systems.health_status", {
    Unknown => "unknown",
    Healthy => "healthy",
    Degraded => "degraded",
    Unreachable => "unreachable",
});

/// Input for registering an external system. Health starts `Unknown` and epoch
/// starts at 0; both are owned by the store, not the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExternalSystem {
    pub kind: ExternalSystemKind,
    pub display_name: String,
    /// Opaque connection details (host, port, library sections, …). Validated
    /// as JSON on write; never interpreted by the store.
    pub connection_profile: JsonValue,
    /// Reference to an out-of-band secret (never the secret itself).
    pub auth_ref: String,
    /// Opaque rate-limit config. Validated as JSON on write.
    pub rate_limit_config: JsonValue,
}

/// A durable external-system row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSystem {
    pub id: ExternalSystemId,
    pub kind: ExternalSystemKind,
    pub display_name: String,
    pub connection_profile: JsonValue,
    pub auth_ref: String,
    pub health_status: ExternalSystemHealth,
    pub rate_limit_config: JsonValue,
    pub created_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
    pub epoch: u64,
}

const COLS: &str = "id, kind, display_name, connection_profile, auth_ref, health_status, \
    rate_limit_config, created_at, retired_at, epoch";

impl SqliteExternalSystemRepo {
    /// Register a system with `Unknown` health, inside the caller's transaction.
    ///
    /// # Errors
    /// Propagates JSON-encode and database errors.
    pub async fn register_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewExternalSystem,
        now: OffsetDateTime,
    ) -> Result<ExternalSystem, VoomError> {
        let ts = iso8601(now)?;
        let profile = encode_json(&input.connection_profile, "connection_profile")?;
        let rate = encode_json(&input.rate_limit_config, "rate_limit_config")?;
        let res = sqlx::query(
            "INSERT INTO external_systems \
             (kind, display_name, connection_profile, auth_ref, health_status, \
              rate_limit_config, created_at) \
             VALUES (?, ?, ?, ?, 'unknown', ?, ?)",
        )
        .bind(input.kind.as_str())
        .bind(&input.display_name)
        .bind(&profile)
        .bind(&input.auth_ref)
        .bind(&rate)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("external_systems insert", e))?;
        Ok(ExternalSystem {
            id: ExternalSystemId(u64_from_i64(res.last_insert_rowid())),
            kind: input.kind,
            display_name: input.display_name,
            connection_profile: input.connection_profile,
            auth_ref: input.auth_ref,
            health_status: ExternalSystemHealth::Unknown,
            rate_limit_config: input.rate_limit_config,
            created_at: now,
            retired_at: None,
            epoch: 0,
        })
    }

    /// Fetch a system by id (retired rows included).
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn get(&self, id: ExternalSystemId) -> Result<Option<ExternalSystem>, VoomError> {
        let row = sqlx::query(&format!("SELECT {COLS} FROM external_systems WHERE id = ?"))
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("external_systems get", e))?;
        row.as_ref().map(row_to_system).transpose()
    }

    /// Fetch a system by id inside the caller's transaction.
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn get_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: ExternalSystemId,
    ) -> Result<Option<ExternalSystem>, VoomError> {
        let row = sqlx::query(&format!("SELECT {COLS} FROM external_systems WHERE id = ?"))
            .bind(i64_from_u64(id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("external_systems get_in_tx", e))?;
        row.as_ref().map(row_to_system).transpose()
    }

    /// List active (non-retired) systems in id order.
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn list(&self) -> Result<Vec<ExternalSystem>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM external_systems WHERE retired_at IS NULL ORDER BY id ASC"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("external_systems list", e))?;
        rows.iter().map(row_to_system).collect()
    }

    /// Set the health status of an active system inside the caller's
    /// transaction. Returns the updated row, or `None` when no active system
    /// has that id.
    ///
    /// # Errors
    /// Propagates database and row-decode errors.
    pub async fn set_health_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: ExternalSystemId,
        health: ExternalSystemHealth,
    ) -> Result<Option<ExternalSystem>, VoomError> {
        let affected = sqlx::query(
            "UPDATE external_systems SET health_status = ? WHERE id = ? AND retired_at IS NULL",
        )
        .bind(health.as_str())
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("external_systems set_health", e))?
        .rows_affected();
        if affected == 0 {
            return Ok(None);
        }
        self.get_in_tx(tx, id).await
    }
}

fn encode_json(value: &JsonValue, field: &str) -> Result<String, VoomError> {
    serde_json::to_string(value)
        .map_err(|e| VoomError::Internal(format!("encode external_systems.{field}: {e}")))
}

fn decode_json(json: &str, field: &str) -> Result<JsonValue, VoomError> {
    serde_json::from_str(json)
        .map_err(|e| VoomError::database(format!("external_systems.{field} decode: {e}")))
}

fn row_to_system(row: &SqliteRow) -> Result<ExternalSystem, VoomError> {
    let map = |field: &'static str| {
        move |e: sqlx::Error| VoomError::database_context(format!("external_systems.{field}"), e)
    };
    let id: i64 = row.try_get("id").map_err(map("id"))?;
    let kind: String = row.try_get("kind").map_err(map("kind"))?;
    let health: String = row.try_get("health_status").map_err(map("health_status"))?;
    let profile: String = row
        .try_get("connection_profile")
        .map_err(map("connection_profile"))?;
    let rate: String = row
        .try_get("rate_limit_config")
        .map_err(map("rate_limit_config"))?;
    let created_at: String = row.try_get("created_at").map_err(map("created_at"))?;
    let retired_at: Option<String> = row.try_get("retired_at").map_err(map("retired_at"))?;
    let epoch: i64 = row.try_get("epoch").map_err(map("epoch"))?;
    Ok(ExternalSystem {
        id: ExternalSystemId(u64_from_i64(id)),
        kind: ExternalSystemKind::parse(&kind)?,
        display_name: row.try_get("display_name").map_err(map("display_name"))?,
        connection_profile: decode_json(&profile, "connection_profile")?,
        auth_ref: row.try_get("auth_ref").map_err(map("auth_ref"))?,
        health_status: ExternalSystemHealth::parse(&health)?,
        rate_limit_config: decode_json(&rate, "rate_limit_config")?,
        created_at: parse_iso8601(&created_at)?,
        retired_at: retired_at.as_deref().map(parse_iso8601).transpose()?,
        epoch: u64_from_i64(epoch),
    })
}

#[cfg(test)]
#[path = "systems_test.rs"]
mod tests;
