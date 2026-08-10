use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{
    FileLocationId, NodeId, NodeIncarnationId, ProviderRelativeLocator, ScanSessionId,
    ScanSessionStatus, ScanTerminalReason, StorageRootId, VoomError,
};

use super::super::Repository;
use super::super::common::{i64_from_u64, map_row_err, parse_iso8601, u32_from_i64, u64_from_i64};

#[derive(Debug, Clone)]
pub struct SqliteScanSessionRepo {
    pool: SqlitePool,
}

impl SqliteScanSessionRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, id: ScanSessionId) -> Result<Option<ScanSession>, VoomError> {
        let row = sqlx::query(SELECT_SCAN_SESSION_COLS)
            .bind(i64_from_u64(id.0, "scan_sessions.id")?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| VoomError::database_context("scan_sessions get", error))?;
        row.as_ref().map(row_to_scan_session).transpose()
    }
}

impl Repository for SqliteScanSessionRepo {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSession {
    pub id: ScanSessionId,
    pub storage_root_id: StorageRootId,
    pub root_epoch: u64,
    pub owner_node_id: NodeId,
    pub owner_incarnation_id: Option<NodeIncarnationId>,
    pub status: ScanSessionStatus,
    pub next_sequence: u64,
    pub batch_count: u64,
    pub observation_count: u64,
    pub idle_timeout_seconds: u32,
    pub progress_deadline_at: OffsetDateTime,
    pub location_high_watermark_id: Option<FileLocationId>,
    pub requested_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub terminal_at: Option<OffsetDateTime>,
    pub terminal_reason: Option<ScanTerminalReason>,
    pub retired_location_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanObservation {
    pub provider_relative_locator: ProviderRelativeLocator,
    pub provider_object_identity: String,
    pub size_bytes: u64,
    pub modified_at: OffsetDateTime,
    pub stability_started_at: OffsetDateTime,
    pub stability_confirmed_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanBatchOutcome {
    pub scan_session_id: ScanSessionId,
    pub sequence: u64,
    pub accepted_observation_count: u64,
    pub cumulative_observation_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReconciliationEvidence {
    pub file_location_id: FileLocationId,
    pub retired_at: OffsetDateTime,
    pub prior_epoch: u64,
    pub retired_epoch: u64,
}

const SELECT_SCAN_SESSION_COLS: &str = "SELECT id, storage_root_id, root_epoch, owner_node_id, \
    owner_incarnation_id, status, next_sequence, batch_count, observation_count, \
    idle_timeout_seconds, progress_deadline_at, location_high_watermark_id, requested_at, \
    started_at, terminal_at, terminal_reason, retired_location_count FROM scan_sessions WHERE id = ?";

fn row_to_scan_session(row: &sqlx::sqlite::SqliteRow) -> Result<ScanSession, VoomError> {
    let session = ScanSession {
        id: ScanSessionId(checked_u64(row, "id")?),
        storage_root_id: StorageRootId(checked_u64(row, "storage_root_id")?),
        root_epoch: checked_u64(row, "root_epoch")?,
        owner_node_id: NodeId(checked_u64(row, "owner_node_id")?),
        owner_incarnation_id: optional_incarnation_id(row, "owner_incarnation_id")?,
        status: ScanSessionStatus::parse_database(
            "scan_sessions.status",
            string_column(row, "status")?,
        )?,
        next_sequence: checked_u64(row, "next_sequence")?,
        batch_count: checked_u64(row, "batch_count")?,
        observation_count: checked_u64(row, "observation_count")?,
        idle_timeout_seconds: checked_u32(row, "idle_timeout_seconds")?,
        progress_deadline_at: timestamp_column(row, "progress_deadline_at")?,
        location_high_watermark_id: optional_file_location_id(row, "location_high_watermark_id")?,
        requested_at: timestamp_column(row, "requested_at")?,
        started_at: optional_timestamp_column(row, "started_at")?,
        terminal_at: optional_timestamp_column(row, "terminal_at")?,
        terminal_reason: optional_terminal_reason(row, "terminal_reason")?,
        retired_location_count: checked_u64(row, "retired_location_count")?,
    };
    validate_scan_session(&session)?;
    Ok(session)
}

pub fn decode_observation_row(row: &sqlx::sqlite::SqliteRow) -> Result<ScanObservation, VoomError> {
    let provider_object_identity = string_column(row, "provider_object_identity")?;
    validate_provider_object_identity(&provider_object_identity)?;
    let observation = ScanObservation {
        provider_relative_locator: ProviderRelativeLocator::parse_database(
            "scan_observations.provider_relative_locator",
            &string_column(row, "provider_relative_locator")?,
        )?,
        provider_object_identity,
        size_bytes: checked_u64(row, "size_bytes")?,
        modified_at: timestamp_column(row, "modified_at")?,
        stability_started_at: timestamp_column(row, "stability_started_at")?,
        stability_confirmed_at: timestamp_column(row, "stability_confirmed_at")?,
    };
    if observation.stability_confirmed_at < observation.stability_started_at {
        return Err(VoomError::database(
            "scan_observations stability confirmation precedes start".to_owned(),
        ));
    }
    Ok(observation)
}

fn validate_scan_session(session: &ScanSession) -> Result<(), VoomError> {
    if !(1..=86_400).contains(&session.idle_timeout_seconds) {
        return Err(VoomError::database(format!(
            "scan_sessions.idle_timeout_seconds {} outside 1..=86400",
            session.idle_timeout_seconds
        )));
    }
    let active = session.terminal_at.is_none() && session.terminal_reason.is_none();
    let bindings = session.owner_incarnation_id.is_some() && session.started_at.is_some();
    let unbound = session.owner_incarnation_id.is_none()
        && session.started_at.is_none()
        && session.location_high_watermark_id.is_none();
    let valid = match session.status {
        ScanSessionStatus::Requested => active && unbound,
        ScanSessionStatus::Running => active && bindings,
        ScanSessionStatus::Succeeded => {
            bindings && session.terminal_at.is_some() && session.terminal_reason.is_none()
        }
        ScanSessionStatus::Failed | ScanSessionStatus::Cancelled | ScanSessionStatus::Stale => {
            session.terminal_at.is_some()
                && session.terminal_reason.is_some()
                && (bindings || unbound)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(VoomError::database(format!(
            "scan_sessions {} has invalid lifecycle shape for {}",
            session.id.0,
            session.status.as_str()
        )))
    }
}

fn validate_provider_object_identity(value: &str) -> Result<(), VoomError> {
    if value.is_empty() || value.len() > 4_096 || value.as_bytes().contains(&0) {
        return Err(VoomError::database(
            "scan_observations.provider_object_identity must be 1..=4096 bytes without NUL"
                .to_owned(),
        ));
    }
    Ok(())
}

fn checked_u64(row: &sqlx::sqlite::SqliteRow, column: &'static str) -> Result<u64, VoomError> {
    let value: i64 = row
        .try_get(column)
        .map_err(|error| map_row_err("scan_sessions", error))?;
    u64_from_i64(value, format!("scan_sessions.{column}"))
}

fn checked_u32(row: &sqlx::sqlite::SqliteRow, column: &'static str) -> Result<u32, VoomError> {
    let value: i64 = row
        .try_get(column)
        .map_err(|error| map_row_err("scan_sessions", error))?;
    u32_from_i64(value)
}

fn string_column(row: &sqlx::sqlite::SqliteRow, column: &'static str) -> Result<String, VoomError> {
    row.try_get(column)
        .map_err(|error| map_row_err("scan_sessions", error))
}

fn timestamp_column(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<OffsetDateTime, VoomError> {
    parse_iso8601(&string_column(row, column)?)
}

fn optional_timestamp_column(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<Option<OffsetDateTime>, VoomError> {
    let value: Option<String> = row
        .try_get(column)
        .map_err(|error| map_row_err("scan_sessions", error))?;
    value.as_deref().map(parse_iso8601).transpose()
}

fn optional_file_location_id(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<Option<FileLocationId>, VoomError> {
    let value: Option<i64> = row
        .try_get(column)
        .map_err(|error| map_row_err("scan_sessions", error))?;
    value
        .map(|value| u64_from_i64(value, format!("scan_sessions.{column}")).map(FileLocationId))
        .transpose()
}

fn optional_incarnation_id(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<Option<NodeIncarnationId>, VoomError> {
    let value: Option<String> = row
        .try_get(column)
        .map_err(|error| map_row_err("scan_sessions", error))?;
    value
        .map(|value| {
            value.parse().map_err(|error| {
                VoomError::database(format!(
                    "scan_sessions.{column} invalid incarnation ID: {error}"
                ))
            })
        })
        .transpose()
}

fn optional_terminal_reason(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<Option<ScanTerminalReason>, VoomError> {
    let value: Option<String> = row
        .try_get(column)
        .map_err(|error| map_row_err("scan_sessions", error))?;
    value
        .map(|value| ScanTerminalReason::parse_database("scan_sessions.terminal_reason", value))
        .transpose()
}

#[cfg(test)]
#[path = "sessions_test.rs"]
mod tests;
