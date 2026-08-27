use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::{
    FileLocationId, NodeId, NodeIncarnationId, ProviderRelativeLocator, ScanObservationEvidence,
    ScanSessionId, ScanSessionStatus, ScanTerminalReason, StorageRootId, VoomError,
};

use super::super::Repository;
use super::super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};
use crate::repo::media::artifact_commit_intents::consult_scan_reconciliation_artifact_intent_lock_in_tx;
use crate::repo::media::commit_safety_gate::consult_scan_reconciliation_commit_lock_in_tx;
use crate::tx::begin_read_only;

pub const MAX_SCAN_SESSION_OBSERVATIONS: u64 = 100_000;

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
        row.as_ref().map(row_to_inspected_scan_session).transpose()
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
        row.as_ref().map(row_to_inspected_scan_session).transpose()
    }

    async fn mutation_session_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: ScanSessionId,
    ) -> Result<Option<ScanSession>, VoomError> {
        let row = sqlx::query(MUTATION_SELECT_SCAN_SESSION)
            .bind(i64_from_u64(id.0, "scan_sessions.id")?)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| VoomError::database_context("scan session mutation read", error))?;
        row.as_ref().map(row_to_mutation_session).transpose()
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
            "SELECT {MUTATION_SCAN_SESSION_COLS} FROM scan_sessions AS session \
             LEFT JOIN node_incarnations AS incarnation \
               ON incarnation.incarnation_id = session.owner_incarnation_id \
             WHERE session.status IN ('requested', 'running') ORDER BY session.id ASC"
        ))
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan_sessions active expiry", error))?;
        let active = active_rows
            .iter()
            .map(row_to_mutation_session)
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

    pub async fn stale_running_for_incarnation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        incarnation_id: NodeIncarnationId,
        reason: ScanTerminalReason,
        now: OffsetDateTime,
    ) -> Result<Vec<ScanSession>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {MUTATION_SCAN_SESSION_COLS} FROM scan_sessions AS session \
             LEFT JOIN node_incarnations AS incarnation \
               ON incarnation.incarnation_id = session.owner_incarnation_id \
             WHERE session.status = 'running' ORDER BY session.id ASC"
        ))
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan_sessions running incarnation", error))?;
        let running = rows
            .iter()
            .map(row_to_mutation_session)
            .collect::<Result<Vec<_>, _>>()?;
        let session_ids = running
            .iter()
            .filter(|session| session.owner_incarnation_id == Some(incarnation_id))
            .map(|session| i64_from_u64(session.id.0, "scan_sessions.id"))
            .collect::<Result<Vec<_>, _>>()?;
        if session_ids.is_empty() {
            return Ok(Vec::new());
        }
        let expected_count = session_ids.len();
        let session_ids = serialize_json(&session_ids, "incarnation scan session IDs")?;
        let mut sessions = sqlx::query(&format!(
            "UPDATE scan_sessions SET status = 'stale', terminal_at = ?, terminal_reason = ? \
             WHERE status = 'running' AND id IN (SELECT value FROM json_each(?)) \
             RETURNING {SCAN_SESSION_COLS}"
        ))
        .bind(iso8601(now)?)
        .bind(reason.as_str())
        .bind(session_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan_sessions stale incarnation", error))?
        .iter()
        .map(row_to_scan_session)
        .collect::<Result<Vec<_>, _>>()?;
        if sessions.len() != expected_count {
            return Err(VoomError::database(format!(
                "scan incarnation staleness expected {expected_count} rows but updated {}",
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
        let Some(session) = self.mutation_session_in_tx(tx, id).await? else {
            return Err(VoomError::NotFound(format!("scan session {id} not found")));
        };
        if session.status != ScanSessionStatus::Requested {
            return Err(VoomError::Conflict(format!(
                "scan session {id} cannot start from {}",
                session.status.as_str()
            )));
        }
        validate_incarnation_owner_in_tx(tx, incarnation_id, session.owner_node_id).await?;
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
        let Some(session) = self
            .mutation_session_in_tx(tx, input.scan_session_id)
            .await?
        else {
            return Err(VoomError::NotFound(format!(
                "scan session {} not found",
                input.scan_session_id
            )));
        };
        if let Some(outcome) = batch_replay_in_tx(tx, &input).await? {
            validate_batch_replay_parent_frontier_in_tx(tx, &session, &outcome).await?;
            return Ok(outcome);
        }
        if input.sequence < session.next_sequence {
            return Err(VoomError::database(format!(
                "scan session {} is missing accepted batch ledger sequence {} below next sequence {}",
                session.id, input.sequence, session.next_sequence
            )));
        }
        if session.status != ScanSessionStatus::Running || session.next_sequence != input.sequence {
            return Err(VoomError::Conflict(format!(
                "scan session {} expects running batch {}",
                session.id, session.next_sequence
            )));
        }
        validate_new_batch_predecessor_in_tx(tx, &session, input.sequence).await?;
        let cumulative_count = session
            .observation_count
            .checked_add(input.observation_count)
            .ok_or_else(|| {
                VoomError::database("scan session observation count overflow".to_owned())
            })?;
        if cumulative_count > MAX_SCAN_SESSION_OBSERVATIONS {
            return Err(VoomError::Conflict(format!(
                "scan session {} observation capacity exceeded: maximum {}, current {}, incoming {}",
                session.id,
                MAX_SCAN_SESSION_OBSERVATIONS,
                session.observation_count,
                input.observation_count
            )));
        }
        ensure_new_locators_in_tx(tx, &input).await?;
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
        sqlx::query("SAVEPOINT scan_batch_accept")
            .execute(&mut **tx)
            .await
            .map_err(|error| VoomError::database_context("begin scan batch savepoint", error))?;
        let persisted = persist_new_batch_in_tx(
            tx,
            &input,
            next_sequence_i64,
            batch_count_i64,
            cumulative_count_i64,
            i64_from_u64(session.observation_count, "scan_sessions.observation_count")?,
        )
        .await;
        if let Err(error) = persisted {
            rollback_batch_savepoint(tx).await?;
            return Err(error);
        }
        release_batch_savepoint(tx).await?;
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
        let Some(session) = self.mutation_session_in_tx(tx, id).await? else {
            return Err(VoomError::Conflict(format!(
                "scan session {id} is already terminal or missing"
            )));
        };
        if !matches!(
            session.status,
            ScanSessionStatus::Requested | ScanSessionStatus::Running
        ) {
            return Err(VoomError::Conflict(format!(
                "scan session {id} is already terminal or missing"
            )));
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

    pub async fn complete_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: CompleteScanSessionInput,
    ) -> Result<ScanCompletionRecord, VoomError> {
        let session = completion_session_in_tx(tx, &input).await?;
        validate_completion_high_watermark_in_tx(tx, &session).await?;
        validate_completion_authority_binding(&session, &input)?;
        validate_completion_ledger_in_tx(tx, &session).await?;
        validate_completion_request_watermark(&session, &input)?;
        let candidates = completion_candidates_in_tx(tx, &session).await?;
        if let Some((commit_id, location_id)) = consult_scan_reconciliation_commit_lock_in_tx(
            tx,
            session.storage_root_id,
            session.id,
            session.location_high_watermark_id,
        )
        .await?
        {
            return Err(completion_commit_lock_conflict(
                session.id,
                commit_id,
                location_id,
            ));
        }
        if let Some((intent_id, state, location_id)) =
            consult_scan_reconciliation_artifact_intent_lock_in_tx(
                tx,
                session.storage_root_id,
                session.id,
                session.location_high_watermark_id,
            )
            .await?
        {
            return Err(VoomError::Conflict(format!(
                "{COMPLETION_COMMIT_LOCK_PREFIX}scan session {} cannot retire location {} \
                 while fenced artifact_commit_intent {} ({}) pins it",
                session.id, location_id, intent_id, state
            )));
        }
        retire_completion_candidates_in_tx(tx, &session, &input, candidates.len()).await?;
        let session =
            mark_completion_succeeded_in_tx(tx, &session, &input, candidates.len()).await?;
        update_completion_root_pointer_in_tx(tx, &session, &input).await?;
        Ok(ScanCompletionRecord {
            session,
            retired_location_ids: candidates,
        })
    }

    /// Load every observation of one session in discovery order.
    ///
    /// Publication input for completion (ADR 0077): evidence-bearing rows
    /// carry their strict evidence payload, decoded and validated here so the
    /// control plane never trusts raw persisted JSON.
    pub async fn session_observations_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        scan_session_id: ScanSessionId,
    ) -> Result<Vec<ScanObservation>, VoomError> {
        let session_id = i64_from_u64(scan_session_id.0, "scan session ID")?;
        let mut after_sequence: Option<i64> = None;
        let mut after_ordinal: Option<i64> = None;
        let mut observations = Vec::new();
        loop {
            let rows = sqlx::query(COMPLETION_OBSERVATION_PAGE_SQL)
                .bind(session_id)
                .bind(after_sequence)
                .bind(after_sequence)
                .bind(after_sequence)
                .bind(after_ordinal)
                .bind(COMPLETION_LEDGER_PAGE_SIZE)
                .fetch_all(&mut **tx)
                .await
                .map_err(|error| {
                    VoomError::database_context("scan session observation page", error)
                })?;
            if rows.is_empty() {
                return Ok(observations);
            }
            for row in &rows {
                let sequence = checked_u64(row, "batch_sequence")?;
                let ordinal = checked_u64(row, "ordinal")?;
                after_sequence = Some(i64_from_u64(sequence, "scan observation batch sequence")?);
                after_ordinal = Some(i64_from_u64(ordinal, "scan observation ordinal")?);
                observations.push(decode_observation_row(row)?);
            }
        }
    }

    pub async fn list(&self, query: ScanSessionListQuery) -> Result<ScanSessionPage, VoomError> {
        let limit = checked_page_limit(query.limit, "scan session list")?;
        let mut builder = QueryBuilder::<Sqlite>::new(format!(
            "SELECT {SCAN_SESSION_COLS}, \
             (SELECT COUNT(*) FROM file_locations \
              WHERE retired_by_scan_session_id = scan_sessions.id) AS attributed_location_count \
             FROM scan_sessions WHERE 1 = 1"
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
            .map(row_to_inspected_scan_session)
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
        let mut tx = begin_read_only(&self.pool, "scan_sessions: reconciliation_page").await?;
        let page = self.reconciliation_page_in_tx(&mut tx, query).await?;
        tx.commit().await.map_err(|error| {
            VoomError::database_context("commit scan reconciliation transaction", error)
        })?;
        Ok(page)
    }

    pub async fn reconciliation_page_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        query: ScanReconciliationQuery,
    ) -> Result<ScanReconciliationPage, VoomError> {
        let limit = checked_page_limit(query.limit, "scan reconciliation")?;
        let Some(session) = self.get_in_tx(tx, query.scan_session_id).await? else {
            return Err(VoomError::NotFound(format!(
                "scan session {} not found",
                query.scan_session_id
            )));
        };
        self.validate_reconciliation_integrity_in_tx(tx, &session)
            .await?;
        let rows = self
            .reconciliation_page_rows_in_tx(tx, &query, limit + 1)
            .await?;
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

    async fn validate_reconciliation_integrity_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        session: &ScanSession,
    ) -> Result<(), VoomError> {
        let session_id = i64_from_u64(session.id.0, "file_locations.retired_by_scan_session_id")?;
        if session.status != ScanSessionStatus::Succeeded {
            return Err(VoomError::Conflict(format!(
                "scan session {} has not succeeded",
                session.id
            )));
        }
        self.reject_invalid_reconciliation_locations_in_tx(tx, session, session_id)
            .await
    }

    async fn reject_invalid_reconciliation_locations_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        session: &ScanSession,
        session_id: i64,
    ) -> Result<(), VoomError> {
        if session.terminal_at.is_none() {
            return Err(VoomError::database(format!(
                "succeeded scan session {} has no terminal timestamp",
                session.id
            )));
        }
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
            .bind(session_id)
            .bind(high_watermark)
            .bind(high_watermark)
            .bind(session_id)
            .fetch_one(&mut **tx)
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

    async fn reconciliation_page_rows_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
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
                .fetch_all(&mut **tx)
                .await
        } else {
            sqlx::query(RECONCILIATION_PAGE_FIRST_SQL)
                .bind(session_id)
                .bind(fetch_limit)
                .fetch_all(&mut **tx)
                .await
        };
        rows.map_err(|error| VoomError::database_context("scan reconciliation page", error))
    }
}

async fn validate_incarnation_owner_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    incarnation_id: NodeIncarnationId,
    owner_node_id: NodeId,
) -> Result<(), VoomError> {
    let valid: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM node_incarnations \
         WHERE incarnation_id = ? AND node_id = ?)",
    )
    .bind(incarnation_id.to_string())
    .bind(i64_from_u64(owner_node_id.0, "node_incarnations.node_id")?)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("scan start incarnation owner", error))?;
    if valid != 1 {
        return Err(VoomError::database(format!(
            "scan start incarnation {incarnation_id} does not belong to owner node {owner_node_id}"
        )));
    }
    Ok(())
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
    /// Agreed hash+probe identity facts (ADR 0077). `None` records existence
    /// without publishing identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ScanObservationEvidence>,
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

#[derive(Debug, Clone)]
pub struct CompleteScanSessionInput {
    pub scan_session_id: ScanSessionId,
    pub expected_storage_root_id: StorageRootId,
    pub expected_root_epoch: u64,
    pub expected_owner_node_id: NodeId,
    pub expected_owner_incarnation_id: NodeIncarnationId,
    pub last_sequence: Option<u64>,
    pub observation_count: u64,
    pub completed_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCompletionRecord {
    pub session: ScanSession,
    pub retired_location_ids: Vec<FileLocationId>,
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
    started_at, terminal_at, terminal_reason, retired_location_count, \
    (SELECT COUNT(*) FROM file_locations \
     WHERE retired_by_scan_session_id = scan_sessions.id) AS attributed_location_count \
    FROM scan_sessions WHERE id = ?";

const MUTATION_SCAN_SESSION_COLS: &str = "session.id AS id, \
    session.storage_root_id AS storage_root_id, session.root_epoch AS root_epoch, \
    session.owner_node_id AS owner_node_id, \
    session.owner_incarnation_id AS owner_incarnation_id, session.status AS status, \
    session.next_sequence AS next_sequence, session.batch_count AS batch_count, \
    session.observation_count AS observation_count, \
    session.idle_timeout_seconds AS idle_timeout_seconds, \
    session.progress_deadline_at AS progress_deadline_at, \
    session.location_high_watermark_id AS location_high_watermark_id, \
    session.requested_at AS requested_at, session.started_at AS started_at, \
    session.terminal_at AS terminal_at, session.terminal_reason AS terminal_reason, \
    session.retired_location_count AS retired_location_count, \
    CASE WHEN session.owner_incarnation_id IS NULL OR incarnation.node_id = session.owner_node_id \
         THEN 1 ELSE 0 END AS owner_incarnation_valid";

const MUTATION_SELECT_SCAN_SESSION: &str = "SELECT \
    session.id AS id, session.storage_root_id AS storage_root_id, \
    session.root_epoch AS root_epoch, session.owner_node_id AS owner_node_id, \
    session.owner_incarnation_id AS owner_incarnation_id, session.status AS status, \
    session.next_sequence AS next_sequence, session.batch_count AS batch_count, \
    session.observation_count AS observation_count, \
    session.idle_timeout_seconds AS idle_timeout_seconds, \
    session.progress_deadline_at AS progress_deadline_at, \
    session.location_high_watermark_id AS location_high_watermark_id, \
    session.requested_at AS requested_at, session.started_at AS started_at, \
    session.terminal_at AS terminal_at, session.terminal_reason AS terminal_reason, \
    session.retired_location_count AS retired_location_count, \
    CASE WHEN session.owner_incarnation_id IS NULL OR incarnation.node_id = session.owner_node_id \
         THEN 1 ELSE 0 END AS owner_incarnation_valid \
    FROM scan_sessions AS session LEFT JOIN node_incarnations AS incarnation \
      ON incarnation.incarnation_id = session.owner_incarnation_id WHERE session.id = ?";

const RECONCILIATION_PAGE_FIRST_SQL: &str = "SELECT l.id, l.storage_root_id, l.provider_relative_locator, l.retired_at, l.epoch \
     FROM file_locations AS l WHERE l.retired_by_scan_session_id = ? ORDER BY l.id ASC LIMIT ?";

const RECONCILIATION_PAGE_AFTER_SQL: &str = "SELECT l.id, l.storage_root_id, l.provider_relative_locator, l.retired_at, l.epoch \
     FROM file_locations AS l WHERE l.retired_by_scan_session_id = ? AND l.id > ? \
     ORDER BY l.id ASC LIMIT ?";

const RECONCILIATION_INVALID_SQL: &str = "SELECT EXISTS(SELECT 1 FROM file_locations AS l \
     WHERE l.retired_by_scan_session_id = ? AND (l.storage_root_id != ? \
     OR l.retired_at IS NULL OR l.retired_at != \
     (SELECT terminal_at FROM scan_sessions WHERE id = ?) \
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

const COMPLETION_CANDIDATES_SQL: &str = "WITH completion_scope(storage_root_id, scan_session_id, high_watermark_id) AS \
     (VALUES (?, ?, ?)) \
     SELECT l.id, l.provider_relative_locator FROM file_locations AS l \
     CROSS JOIN completion_scope AS scope \
     WHERE l.storage_root_id = scope.storage_root_id \
       AND l.address_state = 'rooted' \
       AND l.retired_at IS NULL \
       AND scope.high_watermark_id IS NOT NULL \
       AND l.id <= scope.high_watermark_id \
       AND NOT EXISTS ( \
           SELECT 1 FROM scan_observations AS observation \
           WHERE observation.scan_session_id = scope.scan_session_id \
             AND observation.provider_relative_locator = l.provider_relative_locator \
       ) \
     ORDER BY l.id ASC";

const RETIRE_COMPLETION_CANDIDATES_SQL: &str = "WITH completion_scope(storage_root_id, scan_session_id, high_watermark_id) AS \
     (VALUES (?, ?, ?)) \
     UPDATE file_locations SET retired_at = ?, retired_by_scan_session_id = ?, epoch = epoch + 1 \
     WHERE storage_root_id = (SELECT storage_root_id FROM completion_scope) \
       AND address_state = 'rooted' \
       AND retired_at IS NULL \
       AND (SELECT high_watermark_id FROM completion_scope) IS NOT NULL \
       AND id <= (SELECT high_watermark_id FROM completion_scope) \
       AND NOT EXISTS ( \
           SELECT 1 FROM scan_observations AS observation \
           WHERE observation.scan_session_id = \
                 (SELECT scan_session_id FROM completion_scope) \
             AND observation.provider_relative_locator = file_locations.provider_relative_locator \
       )";

const COMPLETION_LEDGER_PAGE_SIZE: i64 = 256;
const COMPLETION_BATCH_PAGE_SQL: &str = "SELECT sequence, request_hash, observation_count, \
     previous_sequence, accepted_at, cumulative_observation_count FROM scan_observation_batches \
     WHERE scan_session_id = ? AND (? IS NULL OR sequence > ?) \
     ORDER BY sequence ASC LIMIT ?";
const COMPLETION_OBSERVATION_PAGE_SQL: &str = "SELECT batch_sequence, ordinal, \
     provider_relative_locator, provider_object_identity, size_bytes, modified_at, \
     stability_started_at, stability_confirmed_at, evidence_json FROM scan_observations \
     WHERE scan_session_id = ? AND (? IS NULL OR batch_sequence > ? \
       OR (batch_sequence = ? AND ordinal > ?)) \
     ORDER BY batch_sequence ASC, ordinal ASC LIMIT ?";
const COMPLETION_OBSERVATION_DISTRIBUTION_SQL: &str = "SELECT (EXISTS( \
     SELECT 1 FROM scan_observation_batches AS batch \
     LEFT JOIN scan_observations AS observation \
       ON observation.scan_session_id = batch.scan_session_id \
      AND observation.batch_sequence = batch.sequence \
     WHERE batch.scan_session_id = ? \
     GROUP BY batch.sequence, batch.observation_count \
     HAVING COUNT(observation.ordinal) != batch.observation_count \
        OR MIN(observation.ordinal) != 0 \
        OR MAX(observation.ordinal) != batch.observation_count - 1 \
     ) OR EXISTS( \
     SELECT 1 FROM scan_observations AS observation \
     LEFT JOIN scan_observation_batches AS batch \
       ON batch.scan_session_id = observation.scan_session_id \
      AND batch.sequence = observation.batch_sequence \
     WHERE observation.scan_session_id = ? AND batch.sequence IS NULL))";

const COMPLETION_COMMIT_LOCK_PREFIX: &str = "transient scan completion commit lock: ";

async fn completion_session_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &CompleteScanSessionInput,
) -> Result<ScanSession, VoomError> {
    let row = sqlx::query(MUTATION_SELECT_SCAN_SESSION)
        .bind(i64_from_u64(input.scan_session_id.0, "scan_sessions.id")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan completion session read", error))?;
    let Some(row) = row else {
        return Err(VoomError::database(format!(
            "scan completion session {} disappeared",
            input.scan_session_id
        )));
    };
    row_to_mutation_session(&row)
}

fn validate_completion_authority_binding(
    session: &ScanSession,
    input: &CompleteScanSessionInput,
) -> Result<(), VoomError> {
    if session.storage_root_id != input.expected_storage_root_id
        || session.root_epoch != input.expected_root_epoch
        || session.owner_node_id != input.expected_owner_node_id
        || session.owner_incarnation_id != Some(input.expected_owner_incarnation_id)
    {
        return Err(VoomError::database(format!(
            "scan session {} completion authority binding changed",
            session.id
        )));
    }
    Ok(())
}

fn validate_completion_request_watermark(
    session: &ScanSession,
    input: &CompleteScanSessionInput,
) -> Result<(), VoomError> {
    let expected_next_sequence = completion_next_sequence(input.last_sequence)?;
    if session.next_sequence != expected_next_sequence
        || session.observation_count != input.observation_count
    {
        return Err(VoomError::Conflict(format!(
            "scan session {} completion watermark does not match accepted observations",
            session.id
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct CompletionLedgerSummary {
    batch_count: u64,
    observation_count: u64,
}

async fn validate_completion_ledger_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
) -> Result<(), VoomError> {
    if session.batch_count != session.next_sequence {
        return Err(invalid_completion_ledger(
            session,
            "batch count does not equal next sequence",
        ));
    }
    let summary = validate_completion_batches_in_tx(tx, session).await?;
    if summary.batch_count != session.batch_count
        || summary.observation_count != session.observation_count
    {
        return Err(invalid_completion_ledger(
            session,
            "batch totals do not equal session counters",
        ));
    }
    validate_completion_observation_distribution_in_tx(tx, session).await?;
    let actual = validate_completion_observations_in_tx(tx, session).await?;
    if actual != summary.observation_count {
        return Err(invalid_completion_ledger(
            session,
            "observation rows do not equal accepted batch totals",
        ));
    }
    Ok(())
}

async fn validate_completion_batches_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
) -> Result<CompletionLedgerSummary, VoomError> {
    let session_id = i64_from_u64(session.id.0, "scan completion session ID")?;
    let mut after_sequence = None;
    let mut summary = CompletionLedgerSummary {
        batch_count: 0,
        observation_count: 0,
    };
    loop {
        let rows = sqlx::query(COMPLETION_BATCH_PAGE_SQL)
            .bind(session_id)
            .bind(after_sequence)
            .bind(after_sequence)
            .bind(COMPLETION_LEDGER_PAGE_SIZE)
            .fetch_all(&mut **tx)
            .await
            .map_err(|error| VoomError::database_context("scan completion batch ledger", error))?;
        if rows.is_empty() {
            return Ok(summary);
        }
        for row in &rows {
            let sequence = checked_u64(row, "sequence")?;
            if sequence != summary.batch_count {
                return Err(invalid_completion_ledger(
                    session,
                    "batch sequence is not contiguous",
                ));
            }
            validate_completion_predecessor(row, session, sequence)?;
            validate_completion_batch_row(row, session, &mut summary)?;
            after_sequence = Some(i64_from_u64(sequence, "scan batch sequence")?);
        }
    }
}

fn validate_completion_predecessor(
    row: &sqlx::sqlite::SqliteRow,
    session: &ScanSession,
    sequence: u64,
) -> Result<(), VoomError> {
    let previous = optional_u64_column(row, "previous_sequence")?;
    let expected = sequence.checked_sub(1);
    if previous != expected {
        return Err(invalid_completion_ledger(
            session,
            "batch predecessor is invalid",
        ));
    }
    Ok(())
}

fn validate_completion_batch_row(
    row: &sqlx::sqlite::SqliteRow,
    session: &ScanSession,
    summary: &mut CompletionLedgerSummary,
) -> Result<(), VoomError> {
    let request_hash = string_column(row, "request_hash")?;
    if !is_lowercase_sha256(&request_hash) {
        return Err(invalid_completion_ledger(
            session,
            "batch request hash is invalid",
        ));
    }
    timestamp_column(row, "accepted_at")?;
    let count = checked_u64(row, "observation_count")?;
    if !(1..=1_000).contains(&count) {
        return Err(invalid_completion_ledger(
            session,
            "batch observation count is invalid",
        ));
    }
    summary.observation_count = summary
        .observation_count
        .checked_add(count)
        .ok_or_else(|| invalid_completion_ledger(session, "observation count overflow"))?;
    let cumulative = checked_u64(row, "cumulative_observation_count")?;
    if cumulative != summary.observation_count {
        return Err(invalid_completion_ledger(
            session,
            "batch cumulative count is invalid",
        ));
    }
    summary.batch_count = summary
        .batch_count
        .checked_add(1)
        .ok_or_else(|| invalid_completion_ledger(session, "batch count overflow"))?;
    Ok(())
}

async fn validate_completion_observation_distribution_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
) -> Result<(), VoomError> {
    let invalid: i64 = sqlx::query_scalar(COMPLETION_OBSERVATION_DISTRIBUTION_SQL)
        .bind(i64_from_u64(session.id.0, "scan completion session ID")?)
        .bind(i64_from_u64(session.id.0, "scan completion session ID")?)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| {
            VoomError::database_context("scan completion observation distribution", error)
        })?;
    if invalid != 0 {
        return Err(invalid_completion_ledger(
            session,
            "observation rows do not agree with their batches",
        ));
    }
    Ok(())
}

async fn validate_completion_observations_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
) -> Result<u64, VoomError> {
    let session_id = i64_from_u64(session.id.0, "scan completion session ID")?;
    let mut after_sequence = None;
    let mut after_ordinal = None;
    let mut actual = 0_u64;
    loop {
        let rows = sqlx::query(COMPLETION_OBSERVATION_PAGE_SQL)
            .bind(session_id)
            .bind(after_sequence)
            .bind(after_sequence)
            .bind(after_sequence)
            .bind(after_ordinal)
            .bind(COMPLETION_LEDGER_PAGE_SIZE)
            .fetch_all(&mut **tx)
            .await
            .map_err(|error| {
                VoomError::database_context("scan completion observation ledger", error)
            })?;
        if rows.is_empty() {
            return Ok(actual);
        }
        for row in &rows {
            decode_observation_row(row)?;
            let sequence = checked_u64(row, "batch_sequence")?;
            let ordinal = checked_u64(row, "ordinal")?;
            after_sequence = Some(i64_from_u64(sequence, "scan observation batch sequence")?);
            after_ordinal = Some(i64_from_u64(ordinal, "scan observation ordinal")?);
            actual = actual.checked_add(1).ok_or_else(|| {
                invalid_completion_ledger(session, "actual observation count overflow")
            })?;
        }
    }
}

fn invalid_completion_ledger(session: &ScanSession, detail: &str) -> VoomError {
    VoomError::database(format!(
        "scan session {} has invalid completion ledger: {detail}",
        session.id
    ))
}

fn completion_commit_lock_conflict(
    session_id: ScanSessionId,
    commit_id: voom_core::CommitId,
    location_id: FileLocationId,
) -> VoomError {
    VoomError::Conflict(format!(
        "{COMPLETION_COMMIT_LOCK_PREFIX}scan session {session_id} cannot reconcile location \
         {location_id} while commit {commit_id} is in flight"
    ))
}

#[must_use]
pub fn is_completion_commit_lock_conflict(error: &VoomError) -> bool {
    matches!(error, VoomError::Conflict(detail) if detail.starts_with(COMPLETION_COMMIT_LOCK_PREFIX))
}

fn completion_next_sequence(last_sequence: Option<u64>) -> Result<u64, VoomError> {
    last_sequence.map_or(Ok(0), |sequence| {
        sequence.checked_add(1).ok_or_else(|| {
            VoomError::Conflict("scan completion last sequence overflows".to_owned())
        })
    })
}

async fn validate_completion_high_watermark_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
) -> Result<(), VoomError> {
    let Some(high_watermark_id) = session.location_high_watermark_id else {
        return Ok(());
    };
    let row = sqlx::query("SELECT storage_root_id FROM file_locations WHERE id = ?")
        .bind(i64_from_u64(
            high_watermark_id.0,
            "scan completion high-water location ID",
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| {
            VoomError::database_context("scan completion high-water root binding", error)
        })?;
    let Some(row) = row else {
        return Err(VoomError::database(format!(
            "scan session {} high-water location {high_watermark_id} is missing",
            session.id
        )));
    };
    let storage_root_id = row
        .try_get::<Option<i64>, _>("storage_root_id")
        .map_err(|error| map_row_err("scan completion high-water root binding", error))?
        .ok_or_else(|| {
            VoomError::database(format!(
                "scan session {} high-water location {high_watermark_id} is not rooted",
                session.id
            ))
        })?;
    let storage_root_id = StorageRootId(u64_from_i64(
        storage_root_id,
        "scan completion high-water storage root ID",
    )?);
    if storage_root_id != session.storage_root_id {
        return Err(VoomError::database(format!(
            "scan session {} high-water location {high_watermark_id} belongs to root \
             {storage_root_id}, not {}",
            session.id, session.storage_root_id
        )));
    }
    Ok(())
}

async fn completion_candidates_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
) -> Result<Vec<FileLocationId>, VoomError> {
    let rows = sqlx::query(COMPLETION_CANDIDATES_SQL)
        .bind(i64_from_u64(
            session.storage_root_id.0,
            "scan completion storage root ID",
        )?)
        .bind(i64_from_u64(session.id.0, "scan completion session ID")?)
        .bind(completion_high_watermark(session)?)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("scan completion candidates", error))?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row
            .try_get("id")
            .map_err(|error| map_row_err("scan completion candidate", error))?;
        let locator: String = row
            .try_get("provider_relative_locator")
            .map_err(|error| map_row_err("scan completion candidate", error))?;
        ProviderRelativeLocator::parse_database(
            "file_locations.provider_relative_locator",
            &locator,
        )?;
        candidates.push(FileLocationId(u64_from_i64(
            id,
            "scan completion candidate location ID",
        )?));
    }
    Ok(candidates)
}

fn completion_high_watermark(session: &ScanSession) -> Result<Option<i64>, VoomError> {
    session
        .location_high_watermark_id
        .map(|id| i64_from_u64(id.0, "scan completion high-water location ID"))
        .transpose()
}

async fn retire_completion_candidates_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
    input: &CompleteScanSessionInput,
    candidate_count: usize,
) -> Result<(), VoomError> {
    let result = sqlx::query(RETIRE_COMPLETION_CANDIDATES_SQL)
        .bind(i64_from_u64(
            session.storage_root_id.0,
            "scan completion storage root ID",
        )?)
        .bind(i64_from_u64(session.id.0, "scan completion session ID")?)
        .bind(completion_high_watermark(session)?)
        .bind(iso8601(input.completed_at)?)
        .bind(i64_from_u64(session.id.0, "scan completion session ID")?)
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("retire scan completion candidates", error))?;
    let expected = u64::try_from(candidate_count)
        .map_err(|error| VoomError::database_context("scan completion candidate count", error))?;
    if result.rows_affected() != expected {
        return Err(VoomError::database(format!(
            "scan completion expected to retire {expected} locations but retired {}",
            result.rows_affected()
        )));
    }
    Ok(())
}

async fn mark_completion_succeeded_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
    input: &CompleteScanSessionInput,
    candidate_count: usize,
) -> Result<ScanSession, VoomError> {
    let candidate_count = u64::try_from(candidate_count)
        .map_err(|error| VoomError::database_context("scan completion candidate count", error))?;
    let row = sqlx::query(&format!(
        "UPDATE scan_sessions SET status = 'succeeded', terminal_at = ?, \
         retired_location_count = ? WHERE id = ? AND status = 'running' \
         AND storage_root_id = ? AND root_epoch = ? AND owner_node_id = ? \
         AND owner_incarnation_id = ? AND next_sequence = ? AND observation_count = ? \
         AND location_high_watermark_id IS ? RETURNING {SCAN_SESSION_COLS}"
    ))
    .bind(iso8601(input.completed_at)?)
    .bind(i64_from_u64(
        candidate_count,
        "scan completion retired count",
    )?)
    .bind(i64_from_u64(session.id.0, "scan completion session ID")?)
    .bind(i64_from_u64(
        input.expected_storage_root_id.0,
        "scan completion storage root ID",
    )?)
    .bind(i64_from_u64(
        input.expected_root_epoch,
        "scan completion root epoch",
    )?)
    .bind(i64_from_u64(
        input.expected_owner_node_id.0,
        "scan completion owner node ID",
    )?)
    .bind(input.expected_owner_incarnation_id.to_string())
    .bind(i64_from_u64(
        completion_next_sequence(input.last_sequence)?,
        "scan completion next sequence",
    )?)
    .bind(i64_from_u64(
        input.observation_count,
        "scan completion observation count",
    )?)
    .bind(completion_high_watermark(session)?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("mark scan session succeeded", error))?;
    let Some(row) = row else {
        return Err(VoomError::database(format!(
            "scan session {} completion compare-and-set affected no row",
            session.id
        )));
    };
    row_to_scan_session(&row)
}

async fn update_completion_root_pointer_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
    input: &CompleteScanSessionInput,
) -> Result<(), VoomError> {
    let result = sqlx::query(
        "UPDATE library_roots SET last_scan_session_id = ? \
         WHERE id = ? AND root_epoch = ? AND owner_node_id = ?",
    )
    .bind(i64_from_u64(session.id.0, "scan completion session ID")?)
    .bind(i64_from_u64(
        input.expected_storage_root_id.0,
        "scan completion storage root ID",
    )?)
    .bind(i64_from_u64(
        input.expected_root_epoch,
        "scan completion root epoch",
    )?)
    .bind(i64_from_u64(
        input.expected_owner_node_id.0,
        "scan completion owner node ID",
    )?)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("update scan completion root pointer", error))?;
    if result.rows_affected() != 1 {
        return Err(VoomError::database(format!(
            "scan session {} completion root binding changed",
            session.id
        )));
    }
    Ok(())
}

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

fn row_to_inspected_scan_session(row: &sqlx::sqlite::SqliteRow) -> Result<ScanSession, VoomError> {
    let session = row_to_scan_session(row)?;
    let attributed_location_count = checked_u64(row, "attributed_location_count")?;
    validate_scan_session_integrity(&session, attributed_location_count)?;
    Ok(session)
}

fn row_to_mutation_session(row: &sqlx::sqlite::SqliteRow) -> Result<ScanSession, VoomError> {
    let session = row_to_scan_session(row)?;
    let valid: i64 = row
        .try_get("owner_incarnation_valid")
        .map_err(|error| map_row_err("scan session owner incarnation binding", error))?;
    if valid != 1 {
        return Err(VoomError::database(format!(
            "scan session {} owner incarnation does not belong to owner node",
            session.id
        )));
    }
    Ok(session)
}

pub fn decode_observation_row(row: &sqlx::sqlite::SqliteRow) -> Result<ScanObservation, VoomError> {
    let provider_object_identity = string_column(row, "provider_object_identity")?;
    validate_provider_object_identity(&provider_object_identity)?;
    let evidence = match row
        .try_get::<Option<String>, _>("evidence_json")
        .map_err(|error| VoomError::database_context("scan_observations.evidence_json", error))?
    {
        Some(json) => Some(ScanObservationEvidence::parse_database(&json)?),
        None => None,
    };
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
        evidence,
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
    if session.observation_count > MAX_SCAN_SESSION_OBSERVATIONS {
        return Err(VoomError::database(format!(
            "scan_sessions {} observation_count {} exceeds maximum {}",
            session.id.0, session.observation_count, MAX_SCAN_SESSION_OBSERVATIONS
        )));
    }
    if !has_coherent_progress_counters(session) {
        return Err(VoomError::database(format!(
            "scan_sessions {} has incoherent progress counters",
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

fn validate_scan_session_integrity(
    session: &ScanSession,
    attributed_location_count: u64,
) -> Result<(), VoomError> {
    if session.status != ScanSessionStatus::Succeeded && attributed_location_count != 0 {
        return Err(VoomError::database(format!(
            "non-succeeded scan session {} has attributed locations",
            session.id
        )));
    }
    if session.status == ScanSessionStatus::Succeeded
        && session.retired_location_count != attributed_location_count
    {
        return Err(VoomError::database(format!(
            "scan session {} retired count {} does not match {attributed_location_count} attributed locations",
            session.id, session.retired_location_count
        )));
    }
    Ok(())
}

fn has_coherent_progress_counters(session: &ScanSession) -> bool {
    if session.batch_count != session.next_sequence {
        return false;
    }
    if session.batch_count == 0 {
        return session.observation_count == 0;
    }
    if session.observation_count < session.batch_count {
        return false;
    }
    session
        .batch_count
        .checked_mul(1_000)
        .is_none_or(|maximum| session.observation_count <= maximum)
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
    evidence_json: Option<String>,
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
            evidence_json: match &observation.evidence {
                Some(evidence) => Some(evidence.to_database_json()?),
                None => None,
            },
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
    let row = batch_with_predecessor_in_tx(tx, input.session_id, input.sequence_i64).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let outcome = validate_batch_link(&row, input.scan_session_id, input.sequence)?;
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
    Ok(Some(outcome))
}

async fn validate_batch_replay_parent_frontier_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
    outcome: &ScanBatchOutcome,
) -> Result<(), VoomError> {
    let replayed_frontier = outcome.sequence.checked_add(1).ok_or_else(|| {
        VoomError::database(format!(
            "scan session {} batch {} frontier overflows",
            session.id, outcome.sequence
        ))
    })?;
    if replayed_frontier > session.next_sequence
        || (replayed_frontier == session.next_sequence
            && (session.batch_count != replayed_frontier
                || session.observation_count != outcome.cumulative_observation_count))
    {
        return Err(VoomError::database(format!(
            "scan session {} batch {} does not match parent frontier",
            session.id, outcome.sequence
        )));
    }
    let frontier_sequence = session.next_sequence.checked_sub(1).ok_or_else(|| {
        VoomError::database(format!(
            "scan session {} batch {} does not match parent frontier",
            session.id, outcome.sequence
        ))
    })?;
    let session_id = i64_from_u64(session.id.0, "scan_observation_batches.scan_session_id")?;
    let frontier_sequence_i64 = i64_from_u64(
        frontier_sequence,
        "scan_observation_batches.frontier_sequence",
    )?;
    let row = batch_with_predecessor_in_tx(tx, session_id, frontier_sequence_i64)
        .await?
        .ok_or_else(|| {
            VoomError::database(format!(
                "scan session {} is missing frontier batch {frontier_sequence}",
                session.id
            ))
        })?;
    let frontier = validate_batch_link(&row, session.id, frontier_sequence)?;
    if frontier.cumulative_observation_count != session.observation_count {
        return Err(VoomError::database(format!(
            "scan session {} frontier batch {frontier_sequence} cumulative count does not match parent progress",
            session.id
        )));
    }
    Ok(())
}

async fn validate_new_batch_predecessor_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
    sequence: u64,
) -> Result<(), VoomError> {
    let Some(previous_sequence) = sequence.checked_sub(1) else {
        if session.observation_count == 0 {
            return Ok(());
        }
        return Err(VoomError::database(format!(
            "scan session {} sequence zero has non-zero persisted observations",
            session.id
        )));
    };
    let previous_sequence_i64 = i64_from_u64(
        previous_sequence,
        "scan_observation_batches.previous_sequence",
    )?;
    let session_id = i64_from_u64(session.id.0, "scan_observation_batches.scan_session_id")?;
    let row = batch_with_predecessor_in_tx(tx, session_id, previous_sequence_i64)
        .await?
        .ok_or_else(|| {
            VoomError::database(format!(
                "scan session {} is missing predecessor batch {previous_sequence}",
                session.id
            ))
        })?;
    let predecessor = validate_batch_link(&row, session.id, previous_sequence)?;
    if predecessor.cumulative_observation_count != session.observation_count {
        return Err(VoomError::database(format!(
            "scan session {} predecessor cumulative count does not match session progress",
            session.id
        )));
    }
    Ok(())
}

async fn batch_with_predecessor_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: i64,
    sequence: i64,
) -> Result<Option<sqlx::sqlite::SqliteRow>, VoomError> {
    sqlx::query(
        "SELECT batch.request_hash, batch.observation_count, \
         batch.cumulative_observation_count, batch.previous_sequence, \
         predecessor.sequence AS predecessor_sequence, \
         predecessor.cumulative_observation_count AS predecessor_cumulative_observation_count \
         FROM scan_observation_batches AS batch \
         LEFT JOIN scan_observation_batches AS predecessor \
           ON predecessor.scan_session_id = batch.scan_session_id \
          AND predecessor.sequence = batch.previous_sequence \
         WHERE batch.scan_session_id = ? AND batch.sequence = ?",
    )
    .bind(session_id)
    .bind(sequence)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("scan batch predecessor lookup", error))
}

fn validate_batch_link(
    row: &sqlx::sqlite::SqliteRow,
    session_id: ScanSessionId,
    sequence: u64,
) -> Result<ScanBatchOutcome, VoomError> {
    let outcome = batch_outcome_from_row(row, session_id, sequence)?;
    let previous = optional_u64_column(row, "previous_sequence")?;
    let predecessor_sequence = optional_u64_column(row, "predecessor_sequence")?;
    let predecessor_cumulative =
        optional_u64_column(row, "predecessor_cumulative_observation_count")?;
    if sequence == 0 {
        if previous.is_some()
            || predecessor_sequence.is_some()
            || predecessor_cumulative.is_some()
            || outcome.cumulative_observation_count != outcome.accepted_observation_count
        {
            return Err(invalid_batch_link(session_id, sequence));
        }
        return Ok(outcome);
    }
    let expected_previous = sequence.checked_sub(1);
    let expected_cumulative = predecessor_cumulative
        .and_then(|count| count.checked_add(outcome.accepted_observation_count));
    if previous != expected_previous
        || predecessor_sequence != expected_previous
        || expected_cumulative != Some(outcome.cumulative_observation_count)
    {
        return Err(invalid_batch_link(session_id, sequence));
    }
    Ok(outcome)
}

fn invalid_batch_link(session_id: ScanSessionId, sequence: u64) -> VoomError {
    VoomError::database(format!(
        "scan session {session_id} batch {sequence} has invalid predecessor relationship"
    ))
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
        "INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
         request_hash, observation_count, accepted_at, cumulative_observation_count) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(input.session_id)
    .bind(input.sequence_i64)
    .bind((input.sequence_i64 > 0).then(|| input.sequence_i64 - 1))
    .bind(&input.request_hash)
    .bind(input.observation_count_i64)
    .bind(&input.accepted_at)
    .bind(cumulative_count)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("scan batch insert", error))?;
    Ok(())
}

async fn persist_new_batch_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &PreparedBatch,
    next_sequence: i64,
    batch_count: i64,
    cumulative_count: i64,
    current_count: i64,
) -> Result<(), VoomError> {
    let updated = update_batch_progress_in_tx(
        tx,
        input,
        next_sequence,
        batch_count,
        cumulative_count,
        current_count,
    )
    .await?;
    if updated.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "scan session {} batch {} raced",
            input.scan_session_id, input.sequence
        )));
    }
    insert_batch_in_tx(tx, input, cumulative_count).await?;
    insert_observations_in_tx(tx, input).await
}

async fn rollback_batch_savepoint(tx: &mut Transaction<'_, Sqlite>) -> Result<(), VoomError> {
    sqlx::query("ROLLBACK TO scan_batch_accept")
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("rollback scan batch savepoint", error))?;
    release_batch_savepoint(tx).await
}

async fn release_batch_savepoint(tx: &mut Transaction<'_, Sqlite>) -> Result<(), VoomError> {
    sqlx::query("RELEASE scan_batch_accept")
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("release scan batch savepoint", error))?;
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
             stability_started_at, stability_confirmed_at, evidence_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(&observation.evidence_json)
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
    current_count: i64,
) -> Result<sqlx::sqlite::SqliteQueryResult, VoomError> {
    sqlx::query(
        "UPDATE scan_sessions SET next_sequence = ?, batch_count = ?, observation_count = ?, \
         progress_deadline_at = ? WHERE id = ? AND status = 'running' AND next_sequence = ? \
         AND observation_count = ?",
    )
    .bind(next_sequence)
    .bind(batch_count)
    .bind(cumulative_count)
    .bind(&input.next_progress_deadline_at)
    .bind(input.session_id)
    .bind(input.sequence_i64)
    .bind(current_count)
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
        || outcome.cumulative_observation_count > MAX_SCAN_SESSION_OBSERVATIONS
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

fn optional_u64_column(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<Option<u64>, VoomError> {
    let value: Option<i64> = row
        .try_get(column)
        .map_err(|error| map_row_err("scan observation batch", error))?;
    value
        .map(|value| u64_from_i64(value, format!("scan_observation_batches.{column}")))
        .transpose()
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
