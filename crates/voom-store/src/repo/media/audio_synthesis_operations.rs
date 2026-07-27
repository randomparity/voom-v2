//! Durable operation ledger for recoverable audio synthesis.

use std::collections::HashSet;

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{FileVersionId, LeaseId, MediaSnapshotId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, iso8601, map_row_err, u32_from_i64, u64_from_i64};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSynthesisOperationState {
    Planned,
    Staged,
    Prepared,
    RecoveryRequired,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAudioSynthesisOperation {
    pub operation_key: String,
    pub planned_operation_id: String,
    pub source_file_version_id: FileVersionId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub target_codec: String,
    pub target_channels: u32,
    pub container: String,
    pub target_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAudioSynthesisCompanion {
    pub companion_id: String,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub result_snapshot_stream_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSynthesisOperation {
    pub id: u64,
    pub operation_key: String,
    pub planned_operation_id: String,
    pub source_file_version_id: FileVersionId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub target_codec: String,
    pub target_channels: u32,
    pub container: String,
    pub target_path: String,
    pub state: AudioSynthesisOperationState,
    pub dispatch_generation: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSynthesisCompanion {
    pub id: u64,
    pub operation_id: u64,
    pub ordinal: u32,
    pub companion_id: String,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub result_snapshot_stream_id: String,
    pub result_provider_stream_index: Option<u32>,
    pub codec: Option<String>,
    pub channels: Option<u32>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub disposition_default: Option<bool>,
    pub disposition_forced: Option<bool>,
    pub disposition_commentary: Option<bool>,
    pub lineage_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSynthesisOperationRecord {
    pub operation: AudioSynthesisOperation,
    pub companions: Vec<AudioSynthesisCompanion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAudioSynthesisClaim {
    pub operation_key: String,
    pub expected_generation: u32,
    pub lease_id: LeaseId,
    pub claim_token: String,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAudioSynthesisDispatchAttempt {
    pub worker_id: u64,
    pub worker_epoch: u32,
    pub idempotency_key: String,
    pub attempt_directory: String,
    pub staging_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSynthesisDispatchAttempt {
    pub id: u64,
    pub operation_id: u64,
    pub generation: u32,
    pub worker_id: u64,
    pub worker_epoch: u32,
    pub idempotency_key: String,
    pub attempt_directory: String,
    pub staging_path: String,
    pub status: String,
}

#[derive(Debug)]
pub struct SqliteAudioSynthesisOperationRepo {
    pool: SqlitePool,
}

impl SqliteAudioSynthesisOperationRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create_planned(
        &self,
        input: NewAudioSynthesisOperation,
        companions: &[NewAudioSynthesisCompanion],
        now: OffsetDateTime,
    ) -> Result<AudioSynthesisOperationRecord, VoomError> {
        validate_new_operation(&input, companions)?;
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                VoomError::database_context("audio synthesis create begin immediate", error)
            })?;
        if let Some(existing) = load_record_by_key(&mut tx, &input.operation_key).await? {
            require_exact_replay(&existing, &input, companions)?;
            tx.commit().await.map_err(|error| {
                VoomError::database_context("audio synthesis replay commit", error)
            })?;
            return Ok(existing);
        }
        let operation_id = insert_operation(&mut tx, &input, now).await?;
        insert_companions(&mut tx, operation_id, companions).await?;
        let record = load_record_by_id(&mut tx, operation_id)
            .await?
            .ok_or_else(|| {
                VoomError::Internal("audio synthesis operation disappeared".to_owned())
            })?;
        tx.commit()
            .await
            .map_err(|error| VoomError::database_context("audio synthesis create commit", error))?;
        Ok(record)
    }

    pub async fn get_by_key(
        &self,
        operation_key: &str,
    ) -> Result<Option<AudioSynthesisOperationRecord>, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| VoomError::database_context("audio synthesis get begin", error))?;
        let record = load_record_by_key(&mut tx, operation_key).await?;
        tx.commit()
            .await
            .map_err(|error| VoomError::database_context("audio synthesis get commit", error))?;
        Ok(record)
    }

    pub async fn acquire_claim(
        &self,
        claim: &NewAudioSynthesisClaim,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        validate_claim(claim, now)?;
        let result = sqlx::query(
            "UPDATE audio_synthesis_operations \
             SET claim_lease_id = ?, claim_token = ?, claim_expires_at = ? \
             WHERE operation_key = ? AND dispatch_generation = ? AND state != 'committed' \
             AND (claim_token IS NULL OR claim_expires_at <= ? \
                  OR (claim_lease_id = ? AND claim_token = ?))",
        )
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(iso8601(claim.expires_at)?)
        .bind(&claim.operation_key)
        .bind(i64::from(claim.expected_generation))
        .bind(iso8601(now)?)
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("acquire audio synthesis claim", error))?;
        require_one_update(
            result.rows_affected(),
            &format!("audio synthesis operation {} claim", claim.operation_key),
        )
    }

    pub async fn assert_live_claim(
        &self,
        claim: &NewAudioSynthesisClaim,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM audio_synthesis_operations \
             WHERE operation_key = ? AND dispatch_generation = ? AND state != 'committed' \
             AND claim_lease_id = ? AND claim_token = ? AND claim_expires_at > ?)",
        )
        .bind(&claim.operation_key)
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(iso8601(now)?)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("assert audio synthesis claim", error))?;
        if present {
            return Ok(());
        }
        Err(VoomError::Conflict(format!(
            "audio synthesis operation {} lost its exact live claim",
            claim.operation_key
        )))
    }

    pub async fn record_dispatch_attempt(
        &self,
        claim: &NewAudioSynthesisClaim,
        attempt: &NewAudioSynthesisDispatchAttempt,
        now: OffsetDateTime,
    ) -> Result<AudioSynthesisDispatchAttempt, VoomError> {
        validate_dispatch_attempt(attempt)?;
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                VoomError::database_context("audio synthesis dispatch begin", error)
            })?;
        let operation_id = require_live_planned_claim(&mut tx, claim, now).await?;
        let result = sqlx::query(
            "INSERT INTO audio_synthesis_dispatch_attempts \
             (operation_id, generation, worker_id, worker_epoch, idempotency_key, \
              attempt_directory, staging_path, status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'active', ?)",
        )
        .bind(i64_from_u64(operation_id))
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(attempt.worker_id))
        .bind(i64::from(attempt.worker_epoch))
        .bind(&attempt.idempotency_key)
        .bind(&attempt.attempt_directory)
        .bind(&attempt.staging_path)
        .bind(iso8601(now)?)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            VoomError::database_context("insert audio synthesis dispatch attempt", error)
        })?;
        let stored = load_dispatch_attempt(&mut tx, result.last_insert_rowid()).await?;
        tx.commit().await.map_err(|error| {
            VoomError::database_context("audio synthesis dispatch commit", error)
        })?;
        Ok(stored)
    }

    pub async fn mark_dispatch_terminal(
        &self,
        claim: &NewAudioSynthesisClaim,
        attempt_id: u64,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let now = iso8601(now)?;
        let result = sqlx::query(
            "UPDATE audio_synthesis_dispatch_attempts \
             SET status = 'terminal', evidence_kind = 'terminal_response', evidence_at = ? \
             WHERE id = ? AND status = 'active' AND operation_id = \
             (SELECT id FROM audio_synthesis_operations WHERE operation_key = ? \
              AND dispatch_generation = ? AND claim_lease_id = ? AND claim_token = ? \
              AND claim_expires_at > ?)",
        )
        .bind(&now)
        .bind(i64_from_u64(attempt_id))
        .bind(&claim.operation_key)
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("complete audio synthesis dispatch", error))?;
        require_one_update(
            result.rows_affected(),
            &format!("audio synthesis dispatch attempt {attempt_id}"),
        )
    }

    pub async fn quarantine_and_advance_generation(
        &self,
        claim: &NewAudioSynthesisClaim,
        attempt_id: u64,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                VoomError::database_context("audio synthesis generation begin", error)
            })?;
        let now = iso8601(now)?;
        let quarantined = sqlx::query(
            "UPDATE audio_synthesis_dispatch_attempts SET status = 'quarantined' \
             WHERE id = ? AND status = 'active' AND generation = ? \
             AND operation_id = (SELECT id FROM audio_synthesis_operations \
                 WHERE operation_key = ? AND state = 'planned' AND dispatch_generation = ? \
                 AND claim_lease_id = ? AND claim_token = ? AND claim_expires_at > ?)",
        )
        .bind(i64_from_u64(attempt_id))
        .bind(i64::from(claim.expected_generation))
        .bind(&claim.operation_key)
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            VoomError::database_context("quarantine audio synthesis dispatch", error)
        })?;
        require_one_update(
            quarantined.rows_affected(),
            &format!("audio synthesis dispatch attempt {attempt_id}"),
        )?;
        let advanced = sqlx::query(
            "UPDATE audio_synthesis_operations \
             SET dispatch_generation = dispatch_generation + 1, claim_lease_id = NULL, \
                 claim_token = NULL, claim_expires_at = NULL \
             WHERE operation_key = ? AND state = 'planned' AND dispatch_generation = ? \
             AND claim_lease_id = ? AND claim_token = ? AND claim_expires_at > ?",
        )
        .bind(&claim.operation_key)
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            VoomError::database_context("advance audio synthesis generation", error)
        })?;
        require_one_update(
            advanced.rows_affected(),
            &format!(
                "audio synthesis operation {} generation",
                claim.operation_key
            ),
        )?;
        tx.commit().await.map_err(|error| {
            VoomError::database_context("audio synthesis generation commit", error)
        })
    }
}

impl Repository for SqliteAudioSynthesisOperationRepo {}

async fn insert_operation(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &NewAudioSynthesisOperation,
    now: OffsetDateTime,
) -> Result<u64, VoomError> {
    let result = sqlx::query(
        "INSERT INTO audio_synthesis_operations \
         (operation_key, planned_operation_id, source_file_version_id, \
          source_media_snapshot_id, target_codec, target_channels, container, target_path, \
          state, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'planned', ?)",
    )
    .bind(&input.operation_key)
    .bind(&input.planned_operation_id)
    .bind(i64_from_u64(input.source_file_version_id.0))
    .bind(i64_from_u64(input.source_media_snapshot_id.0))
    .bind(&input.target_codec)
    .bind(i64::from(input.target_channels))
    .bind(&input.container)
    .bind(&input.target_path)
    .bind(iso8601(now)?)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("insert audio synthesis operation", error))?;
    Ok(u64_from_i64(result.last_insert_rowid()))
}

async fn insert_companions(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation_id: u64,
    companions: &[NewAudioSynthesisCompanion],
) -> Result<(), VoomError> {
    for (ordinal, companion) in companions.iter().enumerate() {
        sqlx::query(
            "INSERT INTO audio_synthesis_companions \
             (operation_id, ordinal, companion_id, source_snapshot_stream_id, \
              source_provider_stream_index, result_snapshot_stream_id) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(operation_id))
        .bind(i64_from_u64(u64::try_from(ordinal).map_err(|error| {
            VoomError::Config(format!(
                "audio synthesis companion ordinal overflow: {error}"
            ))
        })?))
        .bind(&companion.companion_id)
        .bind(&companion.source_snapshot_stream_id)
        .bind(i64::from(companion.source_provider_stream_index))
        .bind(&companion.result_snapshot_stream_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("insert audio synthesis companion", error))?;
    }
    Ok(())
}

async fn load_record_by_key(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation_key: &str,
) -> Result<Option<AudioSynthesisOperationRecord>, VoomError> {
    let row = sqlx::query("SELECT * FROM audio_synthesis_operations WHERE operation_key = ?")
        .bind(operation_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("load audio synthesis operation", error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    load_record_from_row(tx, &row).await.map(Some)
}

async fn load_record_by_id(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation_id: u64,
) -> Result<Option<AudioSynthesisOperationRecord>, VoomError> {
    let row = sqlx::query("SELECT * FROM audio_synthesis_operations WHERE id = ?")
        .bind(i64_from_u64(operation_id))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("load audio synthesis operation", error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    load_record_from_row(tx, &row).await.map(Some)
}

async fn load_record_from_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    row: &SqliteRow,
) -> Result<AudioSynthesisOperationRecord, VoomError> {
    let operation = decode_operation(row)?;
    let rows = sqlx::query(
        "SELECT companion.*, lineage.id AS lineage_id \
         FROM audio_synthesis_companions companion \
         LEFT JOIN audio_synthesis_stream_lineage lineage ON lineage.companion_id = companion.id \
         WHERE companion.operation_id = ? ORDER BY companion.ordinal",
    )
    .bind(i64_from_u64(operation.id))
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("load audio synthesis companions", error))?;
    let companions = rows
        .iter()
        .map(decode_companion)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AudioSynthesisOperationRecord {
        operation,
        companions,
    })
}

fn decode_operation(row: &SqliteRow) -> Result<AudioSynthesisOperation, VoomError> {
    let id = u64_from_i64(row.try_get("id").map_err(synthesis_row_err)?);
    Ok(AudioSynthesisOperation {
        id,
        operation_key: row.try_get("operation_key").map_err(synthesis_row_err)?,
        planned_operation_id: row
            .try_get("planned_operation_id")
            .map_err(synthesis_row_err)?,
        source_file_version_id: FileVersionId(u64_from_i64(
            row.try_get("source_file_version_id")
                .map_err(synthesis_row_err)?,
        )),
        source_media_snapshot_id: MediaSnapshotId(u64_from_i64(
            row.try_get("source_media_snapshot_id")
                .map_err(synthesis_row_err)?,
        )),
        target_codec: row.try_get("target_codec").map_err(synthesis_row_err)?,
        target_channels: u32_from_i64(row.try_get("target_channels").map_err(synthesis_row_err)?)?,
        container: row.try_get("container").map_err(synthesis_row_err)?,
        target_path: row.try_get("target_path").map_err(synthesis_row_err)?,
        state: AudioSynthesisOperationState::parse(
            &row.try_get::<String, _>("state")
                .map_err(synthesis_row_err)?,
        )?,
        dispatch_generation: u32_from_i64(
            row.try_get("dispatch_generation")
                .map_err(synthesis_row_err)?,
        )?,
    })
}

fn decode_companion(row: &SqliteRow) -> Result<AudioSynthesisCompanion, VoomError> {
    Ok(AudioSynthesisCompanion {
        id: u64_from_i64(row.try_get("id").map_err(synthesis_row_err)?),
        operation_id: u64_from_i64(row.try_get("operation_id").map_err(synthesis_row_err)?),
        ordinal: u32_from_i64(row.try_get("ordinal").map_err(synthesis_row_err)?)?,
        companion_id: row.try_get("companion_id").map_err(synthesis_row_err)?,
        source_snapshot_stream_id: row
            .try_get("source_snapshot_stream_id")
            .map_err(synthesis_row_err)?,
        source_provider_stream_index: u32_from_i64(
            row.try_get("source_provider_stream_index")
                .map_err(synthesis_row_err)?,
        )?,
        result_snapshot_stream_id: row
            .try_get("result_snapshot_stream_id")
            .map_err(synthesis_row_err)?,
        result_provider_stream_index: optional_u32(row, "result_provider_stream_index")?,
        codec: row.try_get("codec").map_err(synthesis_row_err)?,
        channels: optional_u32(row, "channels")?,
        language: row.try_get("language").map_err(synthesis_row_err)?,
        title: row.try_get("title").map_err(synthesis_row_err)?,
        disposition_default: optional_bool(row, "disposition_default")?,
        disposition_forced: optional_bool(row, "disposition_forced")?,
        disposition_commentary: optional_bool(row, "disposition_commentary")?,
        lineage_id: optional_u64(row, "lineage_id")?,
    })
}

fn optional_u32(row: &SqliteRow, field: &str) -> Result<Option<u32>, VoomError> {
    row.try_get::<Option<i64>, _>(field)
        .map_err(synthesis_row_err)?
        .map(u32_from_i64)
        .transpose()
}

fn optional_u64(row: &SqliteRow, field: &str) -> Result<Option<u64>, VoomError> {
    Ok(row
        .try_get::<Option<i64>, _>(field)
        .map_err(synthesis_row_err)?
        .map(u64_from_i64))
}

fn optional_bool(row: &SqliteRow, field: &str) -> Result<Option<bool>, VoomError> {
    let value = row
        .try_get::<Option<i64>, _>(field)
        .map_err(synthesis_row_err)?;
    match value {
        None => Ok(None),
        Some(0) => Ok(Some(false)),
        Some(1) => Ok(Some(true)),
        Some(value) => Err(VoomError::database(format!(
            "{field} contains invalid boolean {value}"
        ))),
    }
}

fn validate_new_operation(
    input: &NewAudioSynthesisOperation,
    companions: &[NewAudioSynthesisCompanion],
) -> Result<(), VoomError> {
    let required = [
        input.operation_key.as_str(),
        input.planned_operation_id.as_str(),
        input.target_codec.as_str(),
        input.container.as_str(),
        input.target_path.as_str(),
    ];
    if required.iter().any(|value| value.trim().is_empty()) || input.target_channels == 0 {
        return Err(VoomError::Config(
            "audio synthesis operation has empty identity or target fields".to_owned(),
        ));
    }
    if companions.is_empty() {
        return Err(VoomError::Config(
            "audio synthesis operation requires companions".to_owned(),
        ));
    }
    validate_companions(companions)
}

fn validate_companions(companions: &[NewAudioSynthesisCompanion]) -> Result<(), VoomError> {
    let mut source_ids = HashSet::with_capacity(companions.len());
    let mut result_ids = HashSet::with_capacity(companions.len());
    let mut source_indexes = HashSet::with_capacity(companions.len());
    let mut previous_index = None;
    for companion in companions {
        let valid = !companion.companion_id.trim().is_empty()
            && !companion.source_snapshot_stream_id.trim().is_empty()
            && companion.companion_id == companion.result_snapshot_stream_id
            && source_ids.insert(&companion.source_snapshot_stream_id)
            && result_ids.insert(&companion.result_snapshot_stream_id)
            && source_indexes.insert(companion.source_provider_stream_index)
            && previous_index.is_none_or(|index| index < companion.source_provider_stream_index);
        if !valid {
            return Err(VoomError::Config(
                "audio synthesis companion descriptors are invalid or unordered".to_owned(),
            ));
        }
        previous_index = Some(companion.source_provider_stream_index);
    }
    Ok(())
}

fn validate_claim(claim: &NewAudioSynthesisClaim, now: OffsetDateTime) -> Result<(), VoomError> {
    if claim.operation_key.trim().is_empty()
        || claim.claim_token.trim().is_empty()
        || claim.expires_at <= now
    {
        return Err(VoomError::Config(
            "audio synthesis claim requires identity, token, and future expiry".to_owned(),
        ));
    }
    Ok(())
}

fn validate_dispatch_attempt(attempt: &NewAudioSynthesisDispatchAttempt) -> Result<(), VoomError> {
    if attempt.idempotency_key.trim().is_empty()
        || attempt.attempt_directory.trim().is_empty()
        || attempt.staging_path.trim().is_empty()
    {
        return Err(VoomError::Config(
            "audio synthesis dispatch requires key and exact paths".to_owned(),
        ));
    }
    Ok(())
}

fn require_exact_replay(
    stored: &AudioSynthesisOperationRecord,
    input: &NewAudioSynthesisOperation,
    companions: &[NewAudioSynthesisCompanion],
) -> Result<(), VoomError> {
    let operation = &stored.operation;
    let exact_operation = operation.operation_key == input.operation_key
        && operation.planned_operation_id == input.planned_operation_id
        && operation.source_file_version_id == input.source_file_version_id
        && operation.source_media_snapshot_id == input.source_media_snapshot_id
        && operation.target_codec == input.target_codec
        && operation.target_channels == input.target_channels
        && operation.container == input.container
        && operation.target_path == input.target_path;
    let exact_companions = stored.companions.len() == companions.len()
        && stored
            .companions
            .iter()
            .zip(companions)
            .all(|(stored, requested)| {
                stored.companion_id == requested.companion_id
                    && stored.source_snapshot_stream_id == requested.source_snapshot_stream_id
                    && stored.source_provider_stream_index == requested.source_provider_stream_index
                    && stored.result_snapshot_stream_id == requested.result_snapshot_stream_id
            });
    if exact_operation && exact_companions {
        return Ok(());
    }
    Err(VoomError::Conflict(format!(
        "audio synthesis operation {} does not match persisted descriptors",
        input.operation_key
    )))
}

async fn require_live_planned_claim(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    claim: &NewAudioSynthesisClaim,
    now: OffsetDateTime,
) -> Result<u64, VoomError> {
    let operation_id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM audio_synthesis_operations \
         WHERE operation_key = ? AND state = 'planned' AND dispatch_generation = ? \
         AND claim_lease_id = ? AND claim_token = ? AND claim_expires_at > ?",
    )
    .bind(&claim.operation_key)
    .bind(i64::from(claim.expected_generation))
    .bind(i64_from_u64(claim.lease_id.0))
    .bind(&claim.claim_token)
    .bind(iso8601(now)?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("load claimed audio synthesis", error))?;
    let operation_id = operation_id.ok_or_else(|| {
        VoomError::Conflict(format!(
            "audio synthesis operation {} lost its planned claim",
            claim.operation_key
        ))
    })?;
    Ok(u64_from_i64(operation_id))
}

async fn load_dispatch_attempt(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    attempt_id: i64,
) -> Result<AudioSynthesisDispatchAttempt, VoomError> {
    let row = sqlx::query("SELECT * FROM audio_synthesis_dispatch_attempts WHERE id = ?")
        .bind(attempt_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("load audio synthesis dispatch", error))?;
    Ok(AudioSynthesisDispatchAttempt {
        id: u64_from_i64(row.try_get("id").map_err(synthesis_row_err)?),
        operation_id: u64_from_i64(row.try_get("operation_id").map_err(synthesis_row_err)?),
        generation: u32_from_i64(row.try_get("generation").map_err(synthesis_row_err)?)?,
        worker_id: u64_from_i64(row.try_get("worker_id").map_err(synthesis_row_err)?),
        worker_epoch: u32_from_i64(row.try_get("worker_epoch").map_err(synthesis_row_err)?)?,
        idempotency_key: row.try_get("idempotency_key").map_err(synthesis_row_err)?,
        attempt_directory: row
            .try_get("attempt_directory")
            .map_err(synthesis_row_err)?,
        staging_path: row.try_get("staging_path").map_err(synthesis_row_err)?,
        status: row.try_get("status").map_err(synthesis_row_err)?,
    })
}

fn require_one_update(rows_affected: u64, target: &str) -> Result<(), VoomError> {
    if rows_affected == 1 {
        return Ok(());
    }
    Err(VoomError::Conflict(format!(
        "{target} was stale or already changed"
    )))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err supplies the owned sqlx error; the shared decoder only borrows it"
)]
fn synthesis_row_err(error: sqlx::Error) -> VoomError {
    map_row_err("audio synthesis", &error)
}

impl AudioSynthesisOperationState {
    fn parse(value: &str) -> Result<Self, VoomError> {
        match value {
            "planned" => Ok(Self::Planned),
            "staged" => Ok(Self::Staged),
            "prepared" => Ok(Self::Prepared),
            "recovery_required" => Ok(Self::RecoveryRequired),
            "committed" => Ok(Self::Committed),
            other => Err(VoomError::database(format!(
                "audio synthesis operation has unknown state `{other}`"
            ))),
        }
    }
}

#[cfg(test)]
#[path = "audio_synthesis_operations_test.rs"]
mod tests;
