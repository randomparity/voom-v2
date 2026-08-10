use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::{
    FileLocationId, NodeId, NodeIncarnationId, ProviderRelativeLocator, ScanSessionId,
    ScanSessionStatus, ScanTerminalReason, StorageRootId, VoomError,
};

use super::super::Repository;
use super::super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};

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

    pub async fn latest_succeeded_for_root(
        &self,
        storage_root_id: StorageRootId,
    ) -> Result<Option<ScanSession>, VoomError> {
        let row = sqlx::query(
            "SELECT last_scan_session_id, \
             (SELECT MAX(id) FROM scan_sessions WHERE storage_root_id = library_roots.id \
              AND status = 'succeeded') AS latest_succeeded_id \
             FROM library_roots WHERE id = ?",
        )
        .bind(i64_from_u64(storage_root_id.0, "library_roots.id")?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("scan session latest root", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let pointer = optional_scan_session_id(&row, "last_scan_session_id")?;
        let latest = optional_scan_session_id(&row, "latest_succeeded_id")?;
        if pointer != latest {
            return Err(VoomError::database(format!(
                "library root {storage_root_id} has invalid latest scan session pointer"
            )));
        }
        let Some(id) = pointer else {
            return Ok(None);
        };
        let session = self.get(id).await?.ok_or_else(|| {
            VoomError::database(format!(
                "library root {storage_root_id} scan session pointer {id} is missing"
            ))
        })?;
        if session.storage_root_id != storage_root_id
            || session.status != ScanSessionStatus::Succeeded
        {
            return Err(VoomError::database(format!(
                "library root {storage_root_id} scan session pointer {id} is invalid"
            )));
        }
        Ok(Some(session))
    }

    pub async fn insert_requested_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewScanSession,
    ) -> Result<ScanSession, VoomError> {
        validate_new_session(&input)?;
        let row = sqlx::query(&format!(
            "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
             idle_timeout_seconds, progress_deadline_at, requested_at) \
             VALUES (?, ?, ?, 'requested', ?, ?, ?) RETURNING {SCAN_SESSION_COLS}"
        ))
        .bind(i64_from_u64(
            input.storage_root_id.0,
            "scan_sessions.storage_root_id",
        )?)
        .bind(i64_from_u64(input.root_epoch, "scan_sessions.root_epoch")?)
        .bind(i64_from_u64(
            input.owner_node_id.0,
            "scan_sessions.owner_node_id",
        )?)
        .bind(i64::from(input.idle_timeout_seconds))
        .bind(iso8601(input.progress_deadline_at)?)
        .bind(iso8601(input.requested_at)?)
        .fetch_one(&mut **tx)
        .await;
        match row {
            Ok(row) => row_to_scan_session(&row),
            Err(error) if is_unique_violation(&error) => {
                let active = self
                    .active_for_root_in_tx(tx, input.storage_root_id)
                    .await?;
                let detail = active.map_or_else(
                    || {
                        format!(
                            "scan session already active for root {}",
                            input.storage_root_id
                        )
                    },
                    |session| {
                        format!(
                            "scan session {} is already active for root {}",
                            session.id, input.storage_root_id
                        )
                    },
                );
                Err(VoomError::Conflict(detail))
            }
            Err(error) => Err(VoomError::database_context(
                "scan_sessions insert requested",
                error,
            )),
        }
    }

    pub async fn get_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: ScanSessionId,
    ) -> Result<Option<ScanSession>, VoomError> {
        let row = sqlx::query(SELECT_SCAN_SESSION_COLS)
            .bind(i64_from_u64(id.0, "scan_sessions.id")?)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| {
                VoomError::database_context("scan_sessions get in transaction", error)
            })?;
        row.as_ref().map(row_to_scan_session).transpose()
    }

    pub async fn active_for_root_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        storage_root_id: StorageRootId,
    ) -> Result<Option<ScanSession>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {SCAN_SESSION_COLS} FROM scan_sessions WHERE storage_root_id = ? \
             AND status IN ('requested', 'running') ORDER BY id ASC"
        ))
        .bind(i64_from_u64(
            storage_root_id.0,
            "scan_sessions.storage_root_id",
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan_sessions active root", error))?;
        row.as_ref().map(row_to_scan_session).transpose()
    }

    pub async fn stale_expired_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        now: OffsetDateTime,
    ) -> Result<Vec<ScanSession>, VoomError> {
        let active_rows = sqlx::query(&format!(
            "SELECT {SCAN_SESSION_COLS} FROM scan_sessions \
             WHERE status IN ('requested', 'running') ORDER BY id ASC"
        ))
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan_sessions active expiry", error))?;
        let active = active_rows
            .iter()
            .map(row_to_scan_session)
            .collect::<Result<Vec<_>, _>>()?;
        let expired_ids = active
            .iter()
            .filter(|session| session.progress_deadline_at <= now)
            .map(|session| i64_from_u64(session.id.0, "scan_sessions.id"))
            .collect::<Result<Vec<_>, _>>()?;
        if expired_ids.is_empty() {
            return Ok(Vec::new());
        }
        let expired_count = expired_ids.len();
        let now = iso8601(now)?;
        let expired_ids = serialize_json(&expired_ids, "expired scan session IDs")?;
        let mut sessions = sqlx::query(&format!(
            "UPDATE scan_sessions SET status = 'stale', terminal_at = ?, \
             terminal_reason = 'scan session progress deadline expired' \
             WHERE status IN ('requested', 'running') \
             AND id IN (SELECT value FROM json_each(?)) \
             RETURNING {SCAN_SESSION_COLS}"
        ))
        .bind(&now)
        .bind(expired_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan_sessions stale expired", error))?
        .iter()
        .map(row_to_scan_session)
        .collect::<Result<Vec<_>, _>>()?;
        if sessions.len() != expired_count {
            return Err(VoomError::database(format!(
                "scan session expiry expected {expired_count} rows but updated {}",
                sessions.len()
            )));
        }
        sessions.sort_by_key(|session| session.id);
        Ok(sessions)
    }

    pub async fn start_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: ScanSessionId,
        incarnation_id: NodeIncarnationId,
        deadline: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<ScanSession, VoomError> {
        let Some(session) = self.get_in_tx(tx, id).await? else {
            return Err(VoomError::NotFound(format!("scan session {id} not found")));
        };
        if session.status != ScanSessionStatus::Requested {
            return Err(VoomError::Conflict(format!(
                "scan session {id} cannot start from {}",
                session.status.as_str()
            )));
        }
        let high_watermark = sqlx::query(
            "SELECT MAX(id) AS id FROM file_locations WHERE storage_root_id = ? \
             AND address_state = 'rooted' AND retired_at IS NULL",
        )
        .bind(i64_from_u64(
            session.storage_root_id.0,
            "file_locations.storage_root_id",
        )?)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan_sessions start high watermark", error))?
        .try_get::<Option<i64>, _>("id")
        .map_err(|error| map_row_err("file_locations high watermark", error))?
        .map(|value| u64_from_i64(value, "file_locations.id").map(FileLocationId))
        .transpose()?;
        let row = sqlx::query(&format!(
            "UPDATE scan_sessions SET status = 'running', owner_incarnation_id = ?, \
             location_high_watermark_id = ?, progress_deadline_at = ?, started_at = ? \
             WHERE id = ? AND status = 'requested' RETURNING {SCAN_SESSION_COLS}"
        ))
        .bind(incarnation_id.to_string())
        .bind(
            high_watermark
                .map(|value| i64_from_u64(value.0, "file_locations.id"))
                .transpose()?,
        )
        .bind(iso8601(deadline)?)
        .bind(iso8601(now)?)
        .bind(i64_from_u64(id.0, "scan_sessions.id")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan_sessions start", error))?;
        row.as_ref()
            .map(row_to_scan_session)
            .transpose()?
            .ok_or_else(|| VoomError::Conflict(format!("scan session {id} start raced")))
    }

    pub async fn accepted_batch_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewScanObservationBatch,
    ) -> Result<ScanBatchOutcome, VoomError> {
        let input = PreparedBatch::new(input)?;
        if let Some(outcome) = batch_replay_in_tx(tx, &input).await? {
            return Ok(outcome);
        }
        let Some(session) = self.get_in_tx(tx, input.scan_session_id).await? else {
            return Err(VoomError::NotFound(format!(
                "scan session {} not found",
                input.scan_session_id
            )));
        };
        if session.status != ScanSessionStatus::Running || session.next_sequence != input.sequence {
            return Err(VoomError::Conflict(format!(
                "scan session {} expects running batch {}",
                session.id, session.next_sequence
            )));
        }
        ensure_new_locators_in_tx(tx, &input).await?;
        let cumulative_count = session
            .observation_count
            .checked_add(input.observation_count)
            .ok_or_else(|| {
                VoomError::database("scan session observation count overflow".to_owned())
            })?;
        let next_sequence = session
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| VoomError::database("scan session sequence overflow".to_owned()))?;
        let batch_count = session
            .batch_count
            .checked_add(1)
            .ok_or_else(|| VoomError::database("scan session batch count overflow".to_owned()))?;
        let cumulative_count_i64 = i64_from_u64(
            cumulative_count,
            "scan_observation_batches.cumulative_observation_count",
        )?;
        let next_sequence_i64 = i64_from_u64(next_sequence, "scan_sessions.next_sequence")?;
        let batch_count_i64 = i64_from_u64(batch_count, "scan_sessions.batch_count")?;
        insert_batch_in_tx(tx, &input, cumulative_count_i64).await?;
        insert_observations_in_tx(tx, &input).await?;
        let updated = update_batch_progress_in_tx(
            tx,
            &input,
            next_sequence_i64,
            batch_count_i64,
            cumulative_count_i64,
        )
        .await?;
        if updated.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "scan session {} batch {} raced",
                input.scan_session_id, input.sequence
            )));
        }
        Ok(ScanBatchOutcome {
            scan_session_id: input.scan_session_id,
            sequence: input.sequence,
            accepted_observation_count: input.observation_count,
            cumulative_observation_count: cumulative_count,
        })
    }

    pub async fn terminalize_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: ScanSessionId,
        status: ScanSessionStatus,
        reason: ScanTerminalReason,
        now: OffsetDateTime,
    ) -> Result<ScanSession, VoomError> {
        match status {
            ScanSessionStatus::Failed | ScanSessionStatus::Cancelled | ScanSessionStatus::Stale => {
            }
            ScanSessionStatus::Requested
            | ScanSessionStatus::Running
            | ScanSessionStatus::Succeeded => {
                return Err(VoomError::Config(format!(
                    "scan session terminalize does not accept {}",
                    status.as_str()
                )));
            }
        }
        let row = sqlx::query(&format!(
            "UPDATE scan_sessions SET status = ?, terminal_reason = ?, terminal_at = ? \
             WHERE id = ? AND status IN ('requested', 'running') RETURNING {SCAN_SESSION_COLS}"
        ))
        .bind(status.as_str())
        .bind(reason.as_str())
        .bind(iso8601(now)?)
        .bind(i64_from_u64(id.0, "scan_sessions.id")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan session terminalize", error))?;
        row.as_ref()
            .map(row_to_scan_session)
            .transpose()?
            .ok_or_else(|| {
                VoomError::Conflict(format!("scan session {id} is already terminal or missing"))
            })
    }

    pub async fn list(&self, query: ScanSessionListQuery) -> Result<ScanSessionPage, VoomError> {
        let limit = checked_page_limit(query.limit, "scan session list")?;
        let mut builder = QueryBuilder::<Sqlite>::new(format!(
            "SELECT {SCAN_SESSION_COLS} FROM scan_sessions WHERE 1 = 1"
        ));
        if let Some(storage_root_id) = query.storage_root_id {
            builder
                .push(" AND storage_root_id = ")
                .push_bind(i64_from_u64(
                    storage_root_id.0,
                    "scan_sessions.storage_root_id",
                )?);
        }
        if let Some(status) = query.status {
            builder.push(" AND status = ").push_bind(status.as_str());
        }
        if let Some(after_id) = query.after_id {
            builder
                .push(" AND id > ")
                .push_bind(i64_from_u64(after_id.0, "scan_sessions.id")?);
        }
        builder.push(" ORDER BY id ASC LIMIT ").push_bind(limit + 1);
        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| VoomError::database_context("scan session list", error))?;
        let mut items = rows
            .iter()
            .map(row_to_scan_session)
            .collect::<Result<Vec<_>, _>>()?;
        let limit = usize::try_from(limit)
            .map_err(|error| VoomError::database_context("scan session list limit", error))?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_after_id = has_more
            .then(|| items.last().map(|session| session.id))
            .flatten();
        Ok(ScanSessionPage {
            items,
            next_after_id,
        })
    }

    pub async fn reconciliation_page(
        &self,
        query: ScanReconciliationQuery,
    ) -> Result<ScanReconciliationPage, VoomError> {
        let limit = checked_page_limit(query.limit, "scan reconciliation")?;
        let Some(session) = self.get(query.scan_session_id).await? else {
            return Err(VoomError::NotFound(format!(
                "scan session {} not found",
                query.scan_session_id
            )));
        };
        self.validate_reconciliation_integrity(&session).await?;
        let rows = self.reconciliation_page_rows(&query, limit + 1).await?;
        let mut items = rows
            .iter()
            .map(|row| reconciliation_item(&session, row))
            .collect::<Result<Vec<_>, _>>()?;
        let limit = usize::try_from(limit)
            .map_err(|error| VoomError::database_context("scan reconciliation limit", error))?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_after_id = has_more
            .then(|| items.last().map(|item| item.file_location_id))
            .flatten();
        Ok(ScanReconciliationPage {
            items,
            next_after_id,
        })
    }

    async fn validate_reconciliation_integrity(
        &self,
        session: &ScanSession,
    ) -> Result<(), VoomError> {
        let session_id = i64_from_u64(session.id.0, "file_locations.retired_by_scan_session_id")?;
        let attributed_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM file_locations WHERE retired_by_scan_session_id = ?",
        )
        .bind(session_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("scan reconciliation count", error))?;
        let attributed_count = u64_from_i64(attributed_count, "scan reconciliation count")?;
        if session.status != ScanSessionStatus::Succeeded {
            return if attributed_count == 0 {
                Err(VoomError::Conflict(format!(
                    "scan session {} has not succeeded",
                    session.id
                )))
            } else {
                Err(VoomError::database(format!(
                    "non-succeeded scan session {} has attributed locations",
                    session.id
                )))
            };
        }
        if attributed_count != session.retired_location_count {
            return Err(VoomError::database(format!(
                "scan session {} retired count {} does not match {attributed_count} attributed locations",
                session.id, session.retired_location_count
            )));
        }
        self.reject_invalid_reconciliation_locations(session, session_id)
            .await
    }

    async fn reject_invalid_reconciliation_locations(
        &self,
        session: &ScanSession,
        session_id: i64,
    ) -> Result<(), VoomError> {
        let terminal_at = session.terminal_at.ok_or_else(|| {
            VoomError::database(format!(
                "succeeded scan session {} has no terminal timestamp",
                session.id
            ))
        })?;
        let high_watermark = session
            .location_high_watermark_id
            .map(|id| i64_from_u64(id.0, "scan_sessions.location_high_watermark_id"))
            .transpose()?;
        let invalid: i64 = sqlx::query_scalar(RECONCILIATION_INVALID_SQL)
            .bind(session_id)
            .bind(i64_from_u64(
                session.storage_root_id.0,
                "scan_sessions.storage_root_id",
            )?)
            .bind(iso8601(terminal_at)?)
            .bind(high_watermark)
            .bind(high_watermark)
            .bind(session_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| VoomError::database_context("scan reconciliation integrity", error))?;
        if invalid != 0 {
            return Err(VoomError::database(format!(
                "scan session {} has invalid reconciliation locations",
                session.id
            )));
        }
        Ok(())
    }

    async fn reconciliation_page_rows(
        &self,
        query: &ScanReconciliationQuery,
        fetch_limit: i64,
    ) -> Result<Vec<sqlx::sqlite::SqliteRow>, VoomError> {
        let session_id = i64_from_u64(
            query.scan_session_id.0,
            "file_locations.retired_by_scan_session_id",
        )?;
        let rows = if let Some(after_id) = query.after_id {
            sqlx::query(RECONCILIATION_PAGE_AFTER_SQL)
                .bind(session_id)
                .bind(i64_from_u64(after_id.0, "file_locations.id")?)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await
        } else {
            sqlx::query(RECONCILIATION_PAGE_FIRST_SQL)
                .bind(session_id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await
        };
        rows.map_err(|error| VoomError::database_context("scan reconciliation page", error))
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

#[derive(Debug, Clone)]
pub struct NewScanSession {
    pub storage_root_id: StorageRootId,
    pub root_epoch: u64,
    pub owner_node_id: NodeId,
    pub idle_timeout_seconds: u32,
    pub progress_deadline_at: OffsetDateTime,
    pub requested_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct NewScanObservationBatch {
    pub scan_session_id: ScanSessionId,
    pub sequence: u64,
    pub request_hash: String,
    pub observations: Vec<ScanObservation>,
    pub accepted_at: OffsetDateTime,
    pub next_progress_deadline_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSessionListQuery {
    pub storage_root_id: Option<StorageRootId>,
    pub status: Option<ScanSessionStatus>,
    pub after_id: Option<ScanSessionId>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSessionPage {
    pub items: Vec<ScanSession>,
    pub next_after_id: Option<ScanSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReconciliationQuery {
    pub scan_session_id: ScanSessionId,
    pub after_id: Option<FileLocationId>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReconciliationPage {
    pub items: Vec<ScanReconciliationEvidence>,
    pub next_after_id: Option<FileLocationId>,
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

const SCAN_SESSION_COLS: &str = "id, storage_root_id, root_epoch, owner_node_id, \
    owner_incarnation_id, status, next_sequence, batch_count, observation_count, \
    idle_timeout_seconds, progress_deadline_at, location_high_watermark_id, requested_at, \
    started_at, terminal_at, terminal_reason, retired_location_count";

const SELECT_SCAN_SESSION_COLS: &str = "SELECT id, storage_root_id, root_epoch, owner_node_id, \
    owner_incarnation_id, status, next_sequence, batch_count, observation_count, \
    idle_timeout_seconds, progress_deadline_at, location_high_watermark_id, requested_at, \
    started_at, terminal_at, terminal_reason, retired_location_count FROM scan_sessions WHERE id = ?";

const RECONCILIATION_PAGE_FIRST_SQL: &str = "SELECT l.id, l.storage_root_id, l.provider_relative_locator, l.retired_at, l.epoch \
     FROM file_locations AS l WHERE l.retired_by_scan_session_id = ? ORDER BY l.id ASC LIMIT ?";

const RECONCILIATION_PAGE_AFTER_SQL: &str = "SELECT l.id, l.storage_root_id, l.provider_relative_locator, l.retired_at, l.epoch \
     FROM file_locations AS l WHERE l.retired_by_scan_session_id = ? AND l.id > ? \
     ORDER BY l.id ASC LIMIT ?";

const RECONCILIATION_INVALID_SQL: &str = "SELECT EXISTS(SELECT 1 FROM file_locations AS l \
     WHERE l.retired_by_scan_session_id = ? AND (l.storage_root_id != ? \
     OR julianday(l.retired_at) IS NULL OR julianday(l.retired_at) != julianday(?) \
     OR ? IS NULL OR l.id > ? OR l.epoch < 1 \
     OR l.provider_relative_locator IS NULL \
     OR length(CAST(l.provider_relative_locator AS BLOB)) NOT BETWEEN 1 AND 4096 \
     OR instr(l.provider_relative_locator, char(0)) != 0 \
     OR instr(l.provider_relative_locator, char(92)) != 0 \
     OR substr(l.provider_relative_locator, 1, 1) = '/' \
     OR substr(l.provider_relative_locator, -1, 1) = '/' \
     OR instr(l.provider_relative_locator, '//') != 0 \
     OR instr('/' || l.provider_relative_locator || '/', '/./') != 0 \
     OR instr('/' || l.provider_relative_locator || '/', '/../') != 0 \
     OR EXISTS(SELECT 1 FROM scan_observations AS o WHERE o.scan_session_id = ? \
     AND o.provider_relative_locator = l.provider_relative_locator)))";

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
    if session.status != ScanSessionStatus::Succeeded && session.retired_location_count != 0 {
        return Err(VoomError::database(format!(
            "scan_sessions {} has retired locations before succeeding",
            session.id.0
        )));
    }
    if has_valid_lifecycle_shape(session) {
        Ok(())
    } else {
        Err(VoomError::database(format!(
            "scan_sessions {} has invalid lifecycle shape for {}",
            session.id.0,
            session.status.as_str()
        )))
    }
}

fn has_valid_lifecycle_shape(session: &ScanSession) -> bool {
    match session.status {
        ScanSessionStatus::Requested => is_active(session) && is_unbound(session),
        ScanSessionStatus::Running => is_active(session) && has_start_bindings(session),
        ScanSessionStatus::Succeeded => has_success_shape(session),
        ScanSessionStatus::Failed | ScanSessionStatus::Cancelled | ScanSessionStatus::Stale => {
            has_unsuccessful_terminal_shape(session)
        }
    }
}

fn is_active(session: &ScanSession) -> bool {
    session.terminal_at.is_none() && session.terminal_reason.is_none()
}

fn has_start_bindings(session: &ScanSession) -> bool {
    session.owner_incarnation_id.is_some() && session.started_at.is_some()
}

fn is_unbound(session: &ScanSession) -> bool {
    session.owner_incarnation_id.is_none()
        && session.started_at.is_none()
        && session.location_high_watermark_id.is_none()
}

fn has_success_shape(session: &ScanSession) -> bool {
    has_start_bindings(session)
        && session.terminal_at.is_some()
        && session.terminal_reason.is_none()
}

fn has_unsuccessful_terminal_shape(session: &ScanSession) -> bool {
    session.terminal_at.is_some()
        && session.terminal_reason.is_some()
        && (has_start_bindings(session) || is_unbound(session))
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

fn validate_new_session(input: &NewScanSession) -> Result<(), VoomError> {
    if !(1..=86_400).contains(&input.idle_timeout_seconds) {
        return Err(VoomError::Config(format!(
            "scan session idle timeout {} outside 1..=86400",
            input.idle_timeout_seconds
        )));
    }
    Ok(())
}

struct PreparedBatch {
    scan_session_id: ScanSessionId,
    session_id: i64,
    sequence: u64,
    sequence_i64: i64,
    request_hash: String,
    observations: Vec<PreparedObservation>,
    observation_count: u64,
    observation_count_i64: i64,
    accepted_at: String,
    next_progress_deadline_at: String,
}

impl PreparedBatch {
    fn new(input: NewScanObservationBatch) -> Result<Self, VoomError> {
        validate_batch_shape(&input)?;
        let session_id = input_i64(input.scan_session_id.0, "scan session ID")?;
        let sequence_i64 = input_i64(input.sequence, "scan batch sequence")?;
        let observation_count = u64::try_from(input.observations.len())
            .map_err(|error| VoomError::Config(format!("scan batch count invalid: {error}")))?;
        let observation_count_i64 = input_i64(observation_count, "scan batch count")?;
        let mut locators = std::collections::BTreeSet::new();
        let mut observations = Vec::with_capacity(input.observations.len());
        for (ordinal, observation) in input.observations.into_iter().enumerate() {
            let locator = observation.provider_relative_locator.as_str();
            if !locators.insert(locator.to_owned()) {
                return Err(VoomError::Conflict(format!(
                    "scan session batch repeats provider-relative locator {locator}"
                )));
            }
            observations.push(PreparedObservation::new(ordinal, observation)?);
        }
        Ok(Self {
            scan_session_id: input.scan_session_id,
            session_id,
            sequence: input.sequence,
            sequence_i64,
            request_hash: input.request_hash,
            observations,
            observation_count,
            observation_count_i64,
            accepted_at: iso8601(input.accepted_at)?,
            next_progress_deadline_at: iso8601(input.next_progress_deadline_at)?,
        })
    }
}

struct PreparedObservation {
    ordinal: i64,
    provider_relative_locator: ProviderRelativeLocator,
    provider_object_identity: String,
    size_bytes: i64,
    modified_at: String,
    stability_started_at: String,
    stability_confirmed_at: String,
}

impl PreparedObservation {
    fn new(ordinal: usize, observation: ScanObservation) -> Result<Self, VoomError> {
        validate_batch_observation(&observation)?;
        let ordinal = u64::try_from(ordinal)
            .map_err(|error| VoomError::Config(format!("scan observation ordinal: {error}")))?;
        Ok(Self {
            ordinal: input_i64(ordinal, "scan observation ordinal")?,
            provider_relative_locator: observation.provider_relative_locator,
            provider_object_identity: observation.provider_object_identity,
            size_bytes: input_i64(observation.size_bytes, "scan observation size")?,
            modified_at: iso8601(observation.modified_at)?,
            stability_started_at: iso8601(observation.stability_started_at)?,
            stability_confirmed_at: iso8601(observation.stability_confirmed_at)?,
        })
    }
}

fn validate_batch_shape(input: &NewScanObservationBatch) -> Result<(), VoomError> {
    if !(1..=1_000).contains(&input.observations.len()) {
        return Err(VoomError::Config(format!(
            "scan session batch observation count {} outside 1..=1000",
            input.observations.len()
        )));
    }
    if !is_lowercase_sha256(&input.request_hash) {
        return Err(VoomError::Config(
            "scan session batch request hash must be lowercase SHA-256".to_owned(),
        ));
    }
    Ok(())
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|character| character.is_ascii_digit() || (b'a'..=b'f').contains(&character))
}

fn validate_batch_observation(observation: &ScanObservation) -> Result<(), VoomError> {
    let identity = &observation.provider_object_identity;
    if identity.is_empty() || identity.len() > 4_096 || identity.as_bytes().contains(&0) {
        return Err(VoomError::Config(
            "scan session batch provider object identity must be 1..=4096 bytes without NUL"
                .to_owned(),
        ));
    }
    if observation.stability_confirmed_at < observation.stability_started_at {
        return Err(VoomError::Config(
            "scan session batch stability confirmation precedes start".to_owned(),
        ));
    }
    Ok(())
}

fn input_i64(value: u64, field: &str) -> Result<i64, VoomError> {
    i64::try_from(value)
        .map_err(|error| VoomError::Config(format!("{field} {value} exceeds storage: {error}")))
}

async fn batch_replay_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &PreparedBatch,
) -> Result<Option<ScanBatchOutcome>, VoomError> {
    let row = sqlx::query(
        "SELECT request_hash, observation_count, cumulative_observation_count \
         FROM scan_observation_batches WHERE scan_session_id = ? AND sequence = ?",
    )
    .bind(input.session_id)
    .bind(input.sequence_i64)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("scan batch replay lookup", error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let request_hash: String = row
        .try_get("request_hash")
        .map_err(|error| map_row_err("scan_observation_batches", error))?;
    if !is_lowercase_sha256(&request_hash) {
        return Err(VoomError::database(format!(
            "scan session {} batch {} has invalid persisted request hash",
            input.scan_session_id, input.sequence
        )));
    }
    if request_hash != input.request_hash {
        return Err(VoomError::Conflict(format!(
            "scan session {} batch {} request hash conflicts with accepted batch",
            input.scan_session_id, input.sequence
        )));
    }
    batch_outcome_from_row(&row, input.scan_session_id, input.sequence).map(Some)
}

async fn ensure_new_locators_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &PreparedBatch,
) -> Result<(), VoomError> {
    for observation in &input.observations {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(SELECT 1 FROM scan_observations WHERE scan_session_id = ? \
             AND provider_relative_locator = ?)",
        )
        .bind(input.session_id)
        .bind(observation.provider_relative_locator.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan observation locator check", error))?;
        if exists != 0 {
            return Err(VoomError::Conflict(format!(
                "scan session {} already contains provider-relative locator {}",
                input.scan_session_id,
                observation.provider_relative_locator.as_str()
            )));
        }
    }
    Ok(())
}

async fn insert_batch_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &PreparedBatch,
    cumulative_count: i64,
) -> Result<(), VoomError> {
    sqlx::query(
        "INSERT INTO scan_observation_batches (scan_session_id, sequence, request_hash, \
         observation_count, accepted_at, cumulative_observation_count) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(input.session_id)
    .bind(input.sequence_i64)
    .bind(&input.request_hash)
    .bind(input.observation_count_i64)
    .bind(&input.accepted_at)
    .bind(cumulative_count)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("scan batch insert", error))?;
    Ok(())
}

async fn insert_observations_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &PreparedBatch,
) -> Result<(), VoomError> {
    for observation in &input.observations {
        let inserted = sqlx::query(
            "INSERT INTO scan_observations (scan_session_id, batch_sequence, ordinal, \
             provider_relative_locator, provider_object_identity, size_bytes, modified_at, \
             stability_started_at, stability_confirmed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(input.session_id)
        .bind(input.sequence_i64)
        .bind(observation.ordinal)
        .bind(observation.provider_relative_locator.as_str())
        .bind(&observation.provider_object_identity)
        .bind(observation.size_bytes)
        .bind(&observation.modified_at)
        .bind(&observation.stability_started_at)
        .bind(&observation.stability_confirmed_at)
        .execute(&mut **tx)
        .await;
        if let Err(error) = inserted {
            if is_unique_violation(&error) {
                return Err(VoomError::Conflict(format!(
                    "scan session {} already contains provider-relative locator {}",
                    input.scan_session_id,
                    observation.provider_relative_locator.as_str()
                )));
            }
            return Err(VoomError::database_context(
                "scan observation insert",
                error,
            ));
        }
    }
    Ok(())
}

async fn update_batch_progress_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &PreparedBatch,
    next_sequence: i64,
    batch_count: i64,
    cumulative_count: i64,
) -> Result<sqlx::sqlite::SqliteQueryResult, VoomError> {
    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = ?, batch_count = ?, observation_count = ?, \
         progress_deadline_at = ? WHERE id = ? AND status = 'running' AND next_sequence = ?",
    )
    .bind(next_sequence)
    .bind(batch_count)
    .bind(cumulative_count)
    .bind(&input.next_progress_deadline_at)
    .bind(input.session_id)
    .bind(input.sequence_i64)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("scan session batch progress", error))
}

fn batch_outcome_from_row(
    row: &sqlx::sqlite::SqliteRow,
    scan_session_id: ScanSessionId,
    sequence: u64,
) -> Result<ScanBatchOutcome, VoomError> {
    let outcome = ScanBatchOutcome {
        scan_session_id,
        sequence,
        accepted_observation_count: checked_u64(row, "observation_count")?,
        cumulative_observation_count: checked_u64(row, "cumulative_observation_count")?,
    };
    if !(1..=1_000).contains(&outcome.accepted_observation_count)
        || outcome.cumulative_observation_count < outcome.accepted_observation_count
    {
        return Err(VoomError::database(format!(
            "scan session {scan_session_id} batch {sequence} has invalid persisted outcome"
        )));
    }
    Ok(outcome)
}

fn reconciliation_item(
    session: &ScanSession,
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ScanReconciliationEvidence, VoomError> {
    let file_location_id = FileLocationId(checked_u64(row, "id")?);
    let storage_root_id = StorageRootId(checked_u64(row, "storage_root_id")?);
    ProviderRelativeLocator::parse_database(
        "file_locations.provider_relative_locator",
        &string_column(row, "provider_relative_locator")?,
    )?;
    let retired_at = timestamp_column(row, "retired_at")?;
    let retired_epoch = checked_u64(row, "epoch")?;
    let Some(high_watermark) = session.location_high_watermark_id else {
        return Err(invalid_reconciliation_location(
            session.id,
            file_location_id,
        ));
    };
    if storage_root_id != session.storage_root_id
        || Some(retired_at) != session.terminal_at
        || file_location_id > high_watermark
        || retired_epoch == 0
    {
        return Err(invalid_reconciliation_location(
            session.id,
            file_location_id,
        ));
    }
    Ok(ScanReconciliationEvidence {
        file_location_id,
        retired_at,
        prior_epoch: retired_epoch
            .checked_sub(1)
            .ok_or_else(|| VoomError::database("scan reconciliation epoch underflow".to_owned()))?,
        retired_epoch,
    })
}

fn invalid_reconciliation_location(
    session_id: ScanSessionId,
    location_id: FileLocationId,
) -> VoomError {
    VoomError::database(format!(
        "scan session {session_id} has invalid reconciliation location {location_id}"
    ))
}

fn checked_page_limit(limit: u32, operation: &str) -> Result<i64, VoomError> {
    if !(1..=100).contains(&limit) {
        return Err(VoomError::Config(format!(
            "{operation} limit {limit} outside 1..=100"
        )));
    }
    Ok(i64::from(limit))
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(error) => error.is_unique_violation(),
        _ => false,
    }
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

fn optional_scan_session_id(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<Option<ScanSessionId>, VoomError> {
    let value: Option<i64> = row
        .try_get(column)
        .map_err(|error| map_row_err("scan session root pointer", error))?;
    value
        .map(|value| u64_from_i64(value, column).map(ScanSessionId))
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
