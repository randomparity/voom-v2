//! Durable operation ledger for atomic plural audio extraction.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, BundleId, FileLocationId, FileVersionId, LeaseId,
    MediaSnapshotId, VoomError, WorkerId,
};

use super::Repository;
use super::common::{i64_from_u64, iso8601, map_row_err, u32_from_i64, u64_from_i64};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioExtractOperationState {
    Planned,
    Staged,
    Prepared,
    RecoveryRequired,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAudioExtractOperation {
    pub operation_key: String,
    pub operation_id: Option<String>,
    pub source_file_version_id: FileVersionId,
    pub source_bundle_id: BundleId,
    pub source_media_snapshot_id: MediaSnapshotId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAudioExtractOutput {
    pub output_id: Option<String>,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub bundle_role: String,
    pub target_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewStagedAudioExtractOutput {
    pub operation_output_id: u64,
    pub staging_path: String,
    pub expected_size_bytes: u64,
    pub expected_checksum: String,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub result_facts: serde_json::Value,
}

#[derive(Debug)]
pub struct StageAudioExtractOperation<'a> {
    pub operation_id: u64,
    pub claim: &'a NewAudioExtractClaim,
    pub worker_result: &'a serde_json::Value,
    pub outputs: &'a [NewStagedAudioExtractOutput],
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPreparedAudioExtractOutput {
    pub operation_output_id: u64,
    pub staging_path: String,
    pub temp_path: String,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub probe_worker_id: WorkerId,
    pub probe_payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFinalizedAudioExtractOutput {
    pub operation_output_id: u64,
    pub source_file_version_id: FileVersionId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub result_file_asset_id: u64,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub bundle_member_id: u64,
    pub recorded_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioExtractRecoveryFailure {
    pub error_code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyAudioExtractOwner {
    pub commit_record_id: u64,
    pub source_file_version_id: u64,
    pub source_media_snapshot_count: u64,
    pub sole_source_media_snapshot_id: Option<u64>,
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub artifact_location_handle_id: u64,
    pub artifact_location_value: String,
    pub artifact_location_retired_at: Option<String>,
    pub verification_id: u64,
    pub verification_artifact_handle_id: u64,
    pub verification_status: String,
    pub staging_path: String,
    pub temp_path: Option<String>,
    pub source_lineage: serde_json::Value,
    pub expected_size_bytes: u64,
    pub expected_checksum: String,
    pub observed_size_bytes: u64,
    pub observed_checksum: String,
    pub result_file_asset_id: u64,
    pub result_file_version_id: u64,
    pub result_file_location_id: u64,
    pub result_location_file_version_id: u64,
    pub result_location: String,
    pub result_location_retired_at: Option<String>,
    pub result_size_bytes: u64,
    pub result_checksum: String,
    pub bundle_member_id: u64,
    pub bundle_id: u64,
    pub bundle_role: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLegacyAudioExtractAdoption {
    pub operation: NewAudioExtractOperation,
    pub output: NewAudioExtractOutput,
    pub owner: LegacyAudioExtractOwner,
    pub probe_worker_id: WorkerId,
    pub probe_payload: serde_json::Value,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub result_facts: serde_json::Value,
    pub recorded_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioExtractOperation {
    pub id: u64,
    pub operation_key: String,
    pub operation_id: Option<String>,
    pub source_file_version_id: FileVersionId,
    pub source_bundle_id: BundleId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub state: AudioExtractOperationState,
    pub dispatch_generation: u32,
    pub worker_result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioExtractOperationOutput {
    pub id: u64,
    pub operation_id: u64,
    pub ordinal: u32,
    pub output_id: Option<String>,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub bundle_role: String,
    pub target_path: String,
    pub staging_path: Option<String>,
    pub temp_path: Option<String>,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_location_id: Option<ArtifactLocationId>,
    pub verification_id: Option<ArtifactVerificationId>,
    pub commit_record_id: Option<ArtifactCommitRecordId>,
    pub probe_worker_id: Option<WorkerId>,
    pub probe_payload: Option<serde_json::Value>,
    pub result_file_asset_id: Option<u64>,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
    pub result_media_snapshot_id: Option<MediaSnapshotId>,
    pub bundle_member_id: Option<u64>,
    pub lineage_id: Option<u64>,
    pub result_facts: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioExtractOperationRecord {
    pub operation: AudioExtractOperation,
    pub outputs: Vec<AudioExtractOperationOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAudioExtractClaim {
    pub operation_key: String,
    pub expected_generation: u32,
    pub lease_id: LeaseId,
    pub claim_token: String,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAudioExtractDispatchAttempt {
    pub worker_id: WorkerId,
    pub worker_epoch: u32,
    pub idempotency_key: String,
    pub attempt_directory: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioExtractDispatchAttemptStatus {
    Active,
    Terminal,
    Quarantined,
    Quiesced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioExtractDispatchAttempt {
    pub id: u64,
    pub operation_id: u64,
    pub generation: u32,
    pub worker_id: WorkerId,
    pub worker_epoch: u32,
    pub idempotency_key: String,
    pub attempt_directory: String,
    pub paths: Vec<String>,
    pub status: AudioExtractDispatchAttemptStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioExtractQuiescenceAcknowledgement {
    pub operation_key: String,
    pub generation: u32,
    pub attempt_id: u64,
    pub worker_id: WorkerId,
    pub worker_epoch: u32,
    pub idempotency_key: String,
    pub acknowledged_by: String,
}

#[derive(Debug, Clone)]
pub struct SqliteAudioExtractOperationRepo {
    pool: SqlitePool,
}

impl SqliteAudioExtractOperationRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get_exact_by_key(
        &self,
        input: &NewAudioExtractOperation,
        outputs: &[NewAudioExtractOutput],
    ) -> Result<Option<AudioExtractOperationRecord>, VoomError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            VoomError::database_context("audio_extract_operations exact get begin", error)
        })?;
        let record = Self::get_exact_by_key_in_tx(&mut tx, input, outputs).await?;
        tx.commit().await.map_err(|error| {
            VoomError::database_context("audio_extract_operations exact get commit", error)
        })?;
        Ok(record)
    }

    pub async fn get_exact_by_key_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        input: &NewAudioExtractOperation,
        outputs: &[NewAudioExtractOutput],
    ) -> Result<Option<AudioExtractOperationRecord>, VoomError> {
        let Some(record) = load_record_by_key(tx, &input.operation_key).await? else {
            return Ok(None);
        };
        require_exact_replay(&record, input, outputs)?;
        Ok(Some(record))
    }

    pub async fn legacy_committed_owner(
        &self,
        target_path: &str,
        bundle_id: BundleId,
        bundle_role: &str,
    ) -> Result<Option<LegacyAudioExtractOwner>, VoomError> {
        let row = legacy_owner_row(&self.pool, target_path, bundle_id, bundle_role).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        require_committed_legacy_owner(&row)?;
        decode_legacy_owner(&row).map(Some)
    }

    pub async fn insert_legacy_adoption_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        input: &NewLegacyAudioExtractAdoption,
    ) -> Result<AudioExtractOperationRecord, VoomError> {
        validate_new_operation(&input.operation, std::slice::from_ref(&input.output))?;
        if load_record_by_key(tx, &input.operation.operation_key)
            .await?
            .is_some()
        {
            return Err(VoomError::Conflict(format!(
                "audio extraction operation {} appeared during legacy adoption",
                input.operation.operation_key
            )));
        }
        let recorded_at = iso8601(input.recorded_at)?;
        let operation_id = insert_legacy_adopted_operation(tx, input, &recorded_at).await?;
        let output_id = insert_legacy_adopted_output(tx, operation_id, input).await?;
        insert_legacy_adopted_lineage(tx, output_id, input, &recorded_at).await?;
        load_record_by_id(tx, operation_id)
            .await?
            .ok_or_else(|| VoomError::Internal("adopted audio extraction disappeared".to_owned()))
    }

    pub async fn stage_operation_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        input: StageAudioExtractOperation<'_>,
    ) -> Result<(), VoomError> {
        for output in input.outputs {
            bind_staged_output(tx, output).await?;
        }
        let worker_result = serde_json::to_string(input.worker_result).map_err(|error| {
            VoomError::Internal(format!("serialize staged audio extraction result: {error}"))
        })?;
        let result = sqlx::query(
            "UPDATE audio_extract_operations SET state = 'staged', worker_result = ? \
             WHERE id = ? AND state = 'planned' AND worker_result IS NULL \
             AND dispatch_generation = ? AND claim_lease_id = ? AND claim_token = ? \
             AND claim_expires_at > ?",
        )
        .bind(worker_result)
        .bind(i64_from_u64(input.operation_id))
        .bind(i64::from(input.claim.expected_generation))
        .bind(i64_from_u64(input.claim.lease_id.0))
        .bind(&input.claim.claim_token)
        .bind(iso8601(input.observed_at)?)
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("stage audio extraction operation", error))?;
        require_one_operation_update(result.rows_affected(), input.operation_id, "planned")
    }

    pub async fn prepare_operation_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        operation_id: u64,
        claim: &NewAudioExtractClaim,
        outputs: &[NewPreparedAudioExtractOutput],
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        for output in outputs {
            bind_prepared_output(tx, output).await?;
        }
        let result = sqlx::query(
            "UPDATE audio_extract_operations SET state = 'prepared' \
             WHERE id = ? AND state = 'staged' \
             AND dispatch_generation = ? AND claim_lease_id = ? AND claim_token = ? \
             AND claim_expires_at > ?",
        )
        .bind(i64_from_u64(operation_id))
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(iso8601(now)?)
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("prepare audio extract operation", error))?;
        require_one_operation_update(result.rows_affected(), operation_id, "staged")
    }

    pub async fn record_finalized_output_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        output: &NewFinalizedAudioExtractOutput,
    ) -> Result<u64, VoomError> {
        let recorded_at = iso8601(output.recorded_at)?;
        let lineage_id = insert_output_lineage(
            tx,
            &AudioExtractLineageInput {
                operation_output_id: output.operation_output_id,
                source_file_version_id: output.source_file_version_id,
                source_media_snapshot_id: output.source_media_snapshot_id,
                source_snapshot_stream_id: &output.source_snapshot_stream_id,
                source_provider_stream_index: output.source_provider_stream_index,
                result_file_version_id: output.result_file_version_id,
                recorded_at: &recorded_at,
            },
        )
        .await?;
        bind_finalized_output(tx, output).await?;
        Ok(lineage_id)
    }

    pub async fn complete_operation_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        operation_id: u64,
        claim: &NewAudioExtractClaim,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let finished_at = iso8601(now)?;
        let result = sqlx::query(
            "UPDATE audio_extract_operations SET state = 'committed', finished_at = ?, \
             recovery_failure_class = NULL, recovery_error_code = NULL, recovery_message = NULL \
             WHERE id = ? AND state IN ('prepared', 'recovery_required') \
             AND dispatch_generation = ? AND claim_lease_id = ? AND claim_token = ? \
             AND claim_expires_at > ?",
        )
        .bind(&finished_at)
        .bind(i64_from_u64(operation_id))
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(&finished_at)
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("finalize audio extract operation", error))?;
        require_one_operation_update(result.rows_affected(), operation_id, "prepared/recovery")
    }

    pub async fn mark_recovery_required_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        operation_id: u64,
        claim: &NewAudioExtractClaim,
        failure: &AudioExtractRecoveryFailure,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let result = sqlx::query(
            "UPDATE audio_extract_operations SET state = 'recovery_required', \
             recovery_failure_class = 'commit_failure', recovery_error_code = ?, \
             recovery_message = ? WHERE id = ? AND state IN ('prepared', 'recovery_required') \
             AND dispatch_generation = ? AND claim_lease_id = ? AND claim_token = ? \
             AND claim_expires_at > ?",
        )
        .bind(&failure.error_code)
        .bind(&failure.message)
        .bind(i64_from_u64(operation_id))
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(iso8601(now)?)
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            VoomError::database_context("mark audio extraction set recovery required", error)
        })?;
        require_one_operation_update(
            result.rows_affected(),
            operation_id,
            "prepared/recovery_required",
        )
    }

    pub async fn create_planned(
        &self,
        input: NewAudioExtractOperation,
        outputs: &[NewAudioExtractOutput],
        now: OffsetDateTime,
    ) -> Result<AudioExtractOperationRecord, VoomError> {
        validate_new_operation(&input, outputs)?;
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                VoomError::database_context("audio_extract_operations begin immediate", error)
            })?;
        if let Some(existing) = load_record_by_key(&mut tx, &input.operation_key).await? {
            require_exact_replay(&existing, &input, outputs)?;
            tx.commit().await.map_err(|error| {
                VoomError::database_context("audio_extract_operations replay commit", error)
            })?;
            return Ok(existing);
        }
        let record = insert_planned(&mut tx, &input, outputs, now).await?;
        tx.commit().await.map_err(|error| {
            VoomError::database_context("audio_extract_operations create commit", error)
        })?;
        Ok(record)
    }

    pub async fn acquire_claim(
        &self,
        claim: &NewAudioExtractClaim,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        if claim.claim_token.is_empty() || claim.expires_at <= now {
            return Err(VoomError::Config(
                "audio extraction claim requires a non-empty token and future expiry".to_owned(),
            ));
        }
        let now = iso8601(now)?;
        let expires_at = iso8601(claim.expires_at)?;
        let result = sqlx::query(
            "UPDATE audio_extract_operations \
             SET claim_lease_id = ?, claim_token = ?, claim_expires_at = ? \
             WHERE operation_key = ? AND dispatch_generation = ? AND state != 'committed' \
             AND (claim_token IS NULL OR claim_expires_at <= ? \
                  OR (claim_lease_id = ? AND claim_token = ?))",
        )
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(expires_at)
        .bind(&claim.operation_key)
        .bind(i64::from(claim.expected_generation))
        .bind(now)
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            VoomError::database_context("audio_extract_operations acquire claim", error)
        })?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        Err(VoomError::Config(format!(
            "audio extraction operation {} claim is held or generation changed",
            claim.operation_key
        )))
    }

    pub async fn assert_live_claim(
        &self,
        claim: &NewAudioExtractClaim,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM audio_extract_operations \
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
        .map_err(|error| {
            VoomError::database_context("assert live audio extraction claim", error)
        })?;
        if present {
            return Ok(());
        }
        Err(VoomError::Conflict(format!(
            "audio extraction operation {} lost its exact live claim",
            claim.operation_key
        )))
    }

    pub async fn renew_claims_for_lease_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        lease_id: LeaseId,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let lease_id = i64_from_u64(lease_id.0);
        let now = iso8601(now)?;
        let claimed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audio_extract_operations \
             WHERE claim_lease_id = ? AND claim_token IS NOT NULL AND state != 'committed'",
        )
        .bind(lease_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| {
            VoomError::database_context("count audio extraction claims for heartbeat", error)
        })?;
        let claimed = u64::try_from(claimed).map_err(|error| {
            VoomError::database(format!(
                "audio extraction claim count is invalid for lease {lease_id}: {error}"
            ))
        })?;
        let renewed = sqlx::query(
            "UPDATE audio_extract_operations SET claim_expires_at = ? \
             WHERE claim_lease_id = ? AND claim_token IS NOT NULL AND state != 'committed' \
               AND claim_expires_at > ?",
        )
        .bind(iso8601(expires_at)?)
        .bind(lease_id)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            VoomError::database_context("renew audio extraction claims with heartbeat", error)
        })?
        .rows_affected();
        if claimed == renewed {
            return Ok(());
        }
        Err(VoomError::Conflict(format!(
            "workflow lease {lease_id} heartbeat cannot renew an expired audio extraction claim"
        )))
    }

    pub async fn release_claim(&self, claim: &NewAudioExtractClaim) -> Result<(), VoomError> {
        let result = sqlx::query(
            "UPDATE audio_extract_operations \
             SET claim_lease_id = NULL, claim_token = NULL, claim_expires_at = NULL \
             WHERE operation_key = ? AND dispatch_generation = ? \
             AND claim_lease_id = ? AND claim_token = ? AND state != 'committed'",
        )
        .bind(&claim.operation_key)
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            VoomError::database_context("audio_extract_operations release claim", error)
        })?;
        let _ = result.rows_affected();
        Ok(())
    }

    pub async fn record_dispatch_attempt(
        &self,
        claim: &NewAudioExtractClaim,
        attempt: NewAudioExtractDispatchAttempt,
        now: OffsetDateTime,
    ) -> Result<AudioExtractDispatchAttempt, VoomError> {
        validate_dispatch_attempt(&attempt)?;
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                VoomError::database_context("audio extract dispatch begin immediate", error)
            })?;
        let operation_id = require_live_planned_claim(&mut tx, claim, now).await?;
        let attempt_id =
            insert_dispatch_attempt(&mut tx, operation_id, claim, &attempt, now).await?;
        let stored = load_dispatch_attempt(&mut tx, attempt_id).await?;
        tx.commit().await.map_err(|error| {
            VoomError::database_context("audio extract dispatch attempt commit", error)
        })?;
        Ok(stored)
    }

    pub async fn get_dispatch_attempt(
        &self,
        operation_id: u64,
        generation: u32,
    ) -> Result<Option<AudioExtractDispatchAttempt>, VoomError> {
        let attempt_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM audio_extract_dispatch_attempts \
             WHERE operation_id = ? AND generation = ?",
        )
        .bind(i64_from_u64(operation_id))
        .bind(i64::from(generation))
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| {
            VoomError::database_context("audio extract dispatch attempt find", error)
        })?;
        let Some(attempt_id) = attempt_id else {
            return Ok(None);
        };
        let mut tx = self.pool.begin().await.map_err(|error| {
            VoomError::database_context("audio extract dispatch attempt get begin", error)
        })?;
        let attempt = load_dispatch_attempt(&mut tx, attempt_id).await?;
        tx.commit().await.map_err(|error| {
            VoomError::database_context("audio extract dispatch attempt get commit", error)
        })?;
        Ok(Some(attempt))
    }

    pub async fn mark_dispatch_terminal(
        &self,
        claim: &NewAudioExtractClaim,
        attempt_id: u64,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        self.update_dispatch_status(
            claim,
            attempt_id,
            AudioExtractDispatchAttemptStatus::Terminal,
            now,
        )
        .await
    }

    pub async fn quarantine_dispatch(
        &self,
        claim: &NewAudioExtractClaim,
        attempt_id: u64,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        self.update_dispatch_status(
            claim,
            attempt_id,
            AudioExtractDispatchAttemptStatus::Quarantined,
            now,
        )
        .await
    }

    async fn update_dispatch_status(
        &self,
        claim: &NewAudioExtractClaim,
        attempt_id: u64,
        status: AudioExtractDispatchAttemptStatus,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let (status, evidence_kind, evidence_at) = match status {
            AudioExtractDispatchAttemptStatus::Terminal => {
                ("terminal", Some("terminal_response"), Some(iso8601(now)?))
            }
            AudioExtractDispatchAttemptStatus::Quarantined => ("quarantined", None, None),
            AudioExtractDispatchAttemptStatus::Active
            | AudioExtractDispatchAttemptStatus::Quiesced => {
                return Err(VoomError::Internal(
                    "unsupported audio dispatch status transition".to_owned(),
                ));
            }
        };
        let result = sqlx::query(
            "UPDATE audio_extract_dispatch_attempts SET status = ?, evidence_kind = ?, \
             evidence_at = ? WHERE id = ? AND status = 'active' AND operation_id = \
             (SELECT id FROM audio_extract_operations WHERE operation_key = ? \
              AND dispatch_generation = ? AND claim_lease_id = ? AND claim_token = ? \
              AND claim_expires_at > ?)",
        )
        .bind(status)
        .bind(evidence_kind)
        .bind(evidence_at)
        .bind(i64_from_u64(attempt_id))
        .bind(&claim.operation_key)
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(iso8601(now)?)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            VoomError::database_context("audio extract dispatch status update", error)
        })?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        Err(VoomError::Conflict(format!(
            "audio extraction dispatch attempt {attempt_id} lost its claim or is not active"
        )))
    }

    pub async fn advance_terminal_generation(
        &self,
        claim: &NewAudioExtractClaim,
        attempt_id: u64,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let result = sqlx::query(
            "UPDATE audio_extract_operations SET dispatch_generation = dispatch_generation + 1, \
             claim_lease_id = NULL, claim_token = NULL, claim_expires_at = NULL \
             WHERE operation_key = ? AND state = 'planned' AND dispatch_generation = ? \
             AND claim_lease_id = ? AND claim_token = ? AND claim_expires_at > ? \
             AND EXISTS (SELECT 1 FROM audio_extract_dispatch_attempts attempt \
                         WHERE attempt.id = ? \
                         AND attempt.operation_id = audio_extract_operations.id \
                         AND attempt.generation = audio_extract_operations.dispatch_generation \
                         AND attempt.status IN ('terminal', 'quiesced')) \
             AND NOT EXISTS (SELECT 1 FROM audio_extract_operation_outputs output \
                             WHERE output.operation_id = audio_extract_operations.id \
                             AND output.staging_path IS NOT NULL)",
        )
        .bind(&claim.operation_key)
        .bind(i64::from(claim.expected_generation))
        .bind(i64_from_u64(claim.lease_id.0))
        .bind(&claim.claim_token)
        .bind(iso8601(now)?)
        .bind(i64_from_u64(attempt_id))
        .execute(&self.pool)
        .await
        .map_err(|error| {
            VoomError::database_context("audio extract dispatch generation advance", error)
        })?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        Err(VoomError::Conflict(format!(
            "audio extraction dispatch attempt {attempt_id} is not safe to clean and advance"
        )))
    }

    pub async fn acknowledge_quiescence(
        &self,
        acknowledgement: &AudioExtractQuiescenceAcknowledgement,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                VoomError::database_context("audio extract quiescence begin", error)
            })?;
        Self::acknowledge_quiescence_in_tx(&mut tx, acknowledgement, now).await?;
        tx.commit()
            .await
            .map_err(|error| VoomError::database_context("audio extract quiescence commit", error))
    }

    pub async fn acknowledge_quiescence_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        acknowledgement: &AudioExtractQuiescenceAcknowledgement,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        if acknowledgement.acknowledged_by.trim().is_empty() {
            return Err(VoomError::Config(
                "audio extraction quiescence acknowledgement requires an actor".to_owned(),
            ));
        }
        let now = iso8601(now)?;
        let result = sqlx::query(
            "UPDATE audio_extract_dispatch_attempts \
             SET status = 'quiesced', evidence_kind = 'operator_acknowledgement', \
                 evidence_at = ?, acknowledged_by = ? \
             WHERE id = ? AND generation = ? AND worker_id = ? AND worker_epoch = ? \
             AND idempotency_key = ? AND status = 'quarantined' \
             AND operation_id = (SELECT id FROM audio_extract_operations \
                                 WHERE operation_key = ? AND state = 'planned' \
                                 AND dispatch_generation = ? \
                                 AND (claim_token IS NULL OR claim_expires_at <= ?))",
        )
        .bind(&now)
        .bind(&acknowledgement.acknowledged_by)
        .bind(i64_from_u64(acknowledgement.attempt_id))
        .bind(i64::from(acknowledgement.generation))
        .bind(i64_from_u64(acknowledgement.worker_id.0))
        .bind(i64::from(acknowledgement.worker_epoch))
        .bind(&acknowledgement.idempotency_key)
        .bind(&acknowledgement.operation_key)
        .bind(i64::from(acknowledgement.generation))
        .bind(&now)
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            VoomError::database_context("audio extract quiescence acknowledgement", error)
        })?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        Err(VoomError::Conflict(
            "audio extraction quiescence evidence does not exactly match an expired, \
             quarantined planned attempt"
                .to_owned(),
        ))
    }
}

impl Repository for SqliteAudioExtractOperationRepo {}

async fn bind_staged_output(
    tx: &mut Transaction<'_, Sqlite>,
    output: &NewStagedAudioExtractOutput,
) -> Result<(), VoomError> {
    let result_facts = serde_json::to_string(&output.result_facts).map_err(|error| {
        VoomError::Internal(format!("serialize staged audio extraction output: {error}"))
    })?;
    let result = sqlx::query(
        "UPDATE audio_extract_operation_outputs SET staging_path = ?, expected_size_bytes = ?, \
         expected_checksum = ?, artifact_handle_id = ?, artifact_location_id = ?, \
         result_facts = ? \
         WHERE id = ? AND staging_path IS NULL AND artifact_handle_id IS NULL",
    )
    .bind(&output.staging_path)
    .bind(i64_from_u64(output.expected_size_bytes))
    .bind(&output.expected_checksum)
    .bind(i64_from_u64(output.artifact_handle_id.0))
    .bind(i64_from_u64(output.artifact_location_id.0))
    .bind(result_facts)
    .bind(i64_from_u64(output.operation_output_id))
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("bind staged audio extract output", error))?;
    require_one_output_update(result.rows_affected(), output.operation_output_id, "staged")
}

async fn bind_prepared_output(
    tx: &mut Transaction<'_, Sqlite>,
    output: &NewPreparedAudioExtractOutput,
) -> Result<(), VoomError> {
    let probe_payload = serde_json::to_string(&output.probe_payload).map_err(|error| {
        VoomError::Internal(format!("serialize audio extraction probe payload: {error}"))
    })?;
    let result = sqlx::query(
        "UPDATE audio_extract_operation_outputs SET temp_path = ?, verification_id = ?, \
         commit_record_id = ?, probe_worker_id = ?, probe_payload = ? \
         WHERE id = ? AND staging_path = ? AND artifact_handle_id = ? \
           AND artifact_location_id = ? AND verification_id IS NULL AND commit_record_id IS NULL",
    )
    .bind(&output.temp_path)
    .bind(i64_from_u64(output.verification_id.0))
    .bind(i64_from_u64(output.commit_record_id.0))
    .bind(i64_from_u64(output.probe_worker_id.0))
    .bind(probe_payload)
    .bind(i64_from_u64(output.operation_output_id))
    .bind(&output.staging_path)
    .bind(i64_from_u64(output.artifact_handle_id.0))
    .bind(i64_from_u64(output.artifact_location_id.0))
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("bind prepared audio extract output", error))?;
    require_one_output_update(
        result.rows_affected(),
        output.operation_output_id,
        "prepared",
    )
}

async fn bind_finalized_output(
    tx: &mut Transaction<'_, Sqlite>,
    output: &NewFinalizedAudioExtractOutput,
) -> Result<(), VoomError> {
    let result = sqlx::query(
        "UPDATE audio_extract_operation_outputs SET result_file_asset_id = ?, \
         result_file_version_id = ?, result_file_location_id = ?, result_media_snapshot_id = ?, \
         bundle_member_id = ? \
         WHERE id = ? AND result_file_version_id IS NULL",
    )
    .bind(i64_from_u64(output.result_file_asset_id))
    .bind(i64_from_u64(output.result_file_version_id.0))
    .bind(i64_from_u64(output.result_file_location_id.0))
    .bind(i64_from_u64(output.result_media_snapshot_id.0))
    .bind(i64_from_u64(output.bundle_member_id))
    .bind(i64_from_u64(output.operation_output_id))
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("bind finalized audio extract output", error))?;
    require_one_output_update(
        result.rows_affected(),
        output.operation_output_id,
        "finalized",
    )
}

fn require_one_operation_update(
    rows_affected: u64,
    operation_id: u64,
    expected: &str,
) -> Result<(), VoomError> {
    if rows_affected == 1 {
        return Ok(());
    }
    Err(VoomError::Conflict(format!(
        "audio extraction operation {operation_id} is not claimed in {expected}"
    )))
}

fn require_one_output_update(
    rows_affected: u64,
    output_id: u64,
    transition: &str,
) -> Result<(), VoomError> {
    if rows_affected == 1 {
        return Ok(());
    }
    Err(VoomError::Conflict(format!(
        "audio extraction output {output_id} could not be {transition}"
    )))
}

async fn legacy_owner_row(
    pool: &SqlitePool,
    target_path: &str,
    bundle_id: BundleId,
    bundle_role: &str,
) -> Result<Option<SqliteRow>, VoomError> {
    sqlx::query(
        "SELECT commit_record.id AS commit_record_id, commit_record.state, \
         commit_record.source_file_version_id, commit_record.artifact_handle_id, \
         commit_record.verification_id, commit_record.temp_path, handle.source_lineage, \
         (SELECT COUNT(*) FROM media_snapshots source_snapshot \
          WHERE source_snapshot.file_version_id = commit_record.source_file_version_id) \
           AS source_media_snapshot_count, \
         (SELECT MIN(source_snapshot.id) FROM media_snapshots source_snapshot \
          WHERE source_snapshot.file_version_id = commit_record.source_file_version_id) \
           AS sole_source_media_snapshot_id, \
         verification.artifact_handle_id AS verification_artifact_handle_id, \
         verification.artifact_location_id, verification.path, verification.status, \
         verification.expected_size_bytes, verification.expected_checksum, \
         verification.observed_size_bytes, verification.observed_checksum, \
         artifact_location.artifact_handle_id AS artifact_location_handle_id, \
         artifact_location.value AS artifact_location_value, \
         artifact_location.retired_at AS artifact_location_retired_at, \
         result_version.file_asset_id AS result_file_asset_id, \
         commit_record.result_file_version_id, commit_record.result_file_location_id, \
         result_location.file_version_id AS result_location_file_version_id, \
         result_location.value AS result_location, \
         result_location.retired_at AS result_location_retired_at, \
         result_version.size_bytes AS result_size_bytes, \
         result_version.content_hash AS result_checksum, member.id AS bundle_member_id, \
         member.bundle_id, member.role AS bundle_role \
         FROM artifact_commit_records commit_record \
         JOIN artifact_handles handle ON handle.id = commit_record.artifact_handle_id \
         JOIN artifact_verifications verification \
           ON verification.id = commit_record.verification_id \
         JOIN artifact_locations artifact_location \
           ON artifact_location.id = verification.artifact_location_id \
         LEFT JOIN file_versions result_version \
           ON result_version.id = commit_record.result_file_version_id \
         LEFT JOIN file_locations result_location \
           ON result_location.id = commit_record.result_file_location_id \
         LEFT JOIN asset_bundle_members member \
           ON member.file_asset_id = result_version.file_asset_id \
         WHERE commit_record.target_path = ? \
           AND commit_record.state IN ('pending', 'committed', 'recovery_required') \
           AND member.bundle_id = ? AND member.role = ?",
    )
    .bind(target_path)
    .bind(i64_from_u64(bundle_id.0))
    .bind(bundle_role)
    .fetch_optional(pool)
    .await
    .map_err(|error| VoomError::database_context("legacy audio extract owner", error))
}

fn require_committed_legacy_owner(row: &SqliteRow) -> Result<(), VoomError> {
    let state: String = row.try_get("state").map_err(legacy_owner_row_err)?;
    if state == "committed" {
        return Ok(());
    }
    let id: i64 = row
        .try_get("commit_record_id")
        .map_err(legacy_owner_row_err)?;
    Err(VoomError::Conflict(format!(
        "audio extraction target has uncommitted owner {id} in state {state}"
    )))
}

fn decode_legacy_owner(row: &SqliteRow) -> Result<LegacyAudioExtractOwner, VoomError> {
    let lineage: Option<String> = row
        .try_get("source_lineage")
        .map_err(legacy_owner_row_err)?;
    let lineage = lineage.ok_or_else(|| {
        VoomError::Conflict(
            "committed legacy audio extraction is missing source lineage".to_owned(),
        )
    })?;
    Ok(LegacyAudioExtractOwner {
        commit_record_id: legacy_required_u64(row, "commit_record_id")?,
        source_file_version_id: legacy_required_u64(row, "source_file_version_id")?,
        source_media_snapshot_count: legacy_required_u64(row, "source_media_snapshot_count")?,
        sole_source_media_snapshot_id: legacy_maybe_u64(row, "sole_source_media_snapshot_id")?,
        artifact_handle_id: legacy_required_u64(row, "artifact_handle_id")?,
        artifact_location_id: legacy_required_u64(row, "artifact_location_id")?,
        artifact_location_handle_id: legacy_required_u64(row, "artifact_location_handle_id")?,
        artifact_location_value: legacy_string(row, "artifact_location_value")?,
        artifact_location_retired_at: legacy_optional_string(row, "artifact_location_retired_at")?,
        verification_id: legacy_required_u64(row, "verification_id")?,
        verification_artifact_handle_id: legacy_required_u64(
            row,
            "verification_artifact_handle_id",
        )?,
        verification_status: legacy_string(row, "status")?,
        staging_path: legacy_string(row, "path")?,
        temp_path: legacy_optional_string(row, "temp_path")?,
        source_lineage: serde_json::from_str(&lineage).map_err(|error| {
            VoomError::Conflict(format!(
                "legacy audio extraction lineage is malformed: {error}"
            ))
        })?,
        expected_size_bytes: legacy_optional_u64(row, "expected_size_bytes")?,
        expected_checksum: legacy_string(row, "expected_checksum")?,
        observed_size_bytes: legacy_optional_u64(row, "observed_size_bytes")?,
        observed_checksum: legacy_string(row, "observed_checksum")?,
        result_file_asset_id: legacy_optional_u64(row, "result_file_asset_id")?,
        result_file_version_id: legacy_optional_u64(row, "result_file_version_id")?,
        result_file_location_id: legacy_optional_u64(row, "result_file_location_id")?,
        result_location_file_version_id: legacy_optional_u64(
            row,
            "result_location_file_version_id",
        )?,
        result_location: legacy_string(row, "result_location")?,
        result_location_retired_at: legacy_optional_string(row, "result_location_retired_at")?,
        result_size_bytes: legacy_optional_u64(row, "result_size_bytes")?,
        result_checksum: legacy_string(row, "result_checksum")?,
        bundle_member_id: legacy_optional_u64(row, "bundle_member_id")?,
        bundle_id: legacy_optional_u64(row, "bundle_id")?,
        bundle_role: legacy_string(row, "bundle_role")?,
    })
}

fn legacy_required_u64(row: &SqliteRow, column: &str) -> Result<u64, VoomError> {
    let value: i64 = row.try_get(column).map_err(legacy_owner_row_err)?;
    u64::try_from(value).map_err(|error| VoomError::database_context(column, error))
}

fn legacy_optional_u64(row: &SqliteRow, column: &str) -> Result<u64, VoomError> {
    let value = row
        .try_get::<Option<i64>, _>(column)
        .map_err(legacy_owner_row_err)?
        .ok_or_else(|| legacy_missing_column(column))?;
    u64::try_from(value).map_err(|error| VoomError::database_context(column, error))
}

fn legacy_maybe_u64(row: &SqliteRow, column: &str) -> Result<Option<u64>, VoomError> {
    row.try_get::<Option<i64>, _>(column)
        .map_err(legacy_owner_row_err)?
        .map(u64::try_from)
        .transpose()
        .map_err(|error| VoomError::database_context(column, error))
}

fn legacy_string(row: &SqliteRow, column: &str) -> Result<String, VoomError> {
    row.try_get::<Option<String>, _>(column)
        .map_err(legacy_owner_row_err)?
        .ok_or_else(|| legacy_missing_column(column))
}

fn legacy_optional_string(row: &SqliteRow, column: &str) -> Result<Option<String>, VoomError> {
    row.try_get(column).map_err(legacy_owner_row_err)
}

fn legacy_missing_column(column: &str) -> VoomError {
    VoomError::Conflict(format!(
        "committed legacy audio extraction is missing {column}"
    ))
}

fn legacy_owner_row_err(error: sqlx::Error) -> VoomError {
    VoomError::database_context("legacy audio extraction owner decode", error)
}

async fn insert_legacy_adopted_operation(
    tx: &mut Transaction<'_, Sqlite>,
    input: &NewLegacyAudioExtractAdoption,
    recorded_at: &str,
) -> Result<i64, VoomError> {
    sqlx::query(
        "INSERT INTO audio_extract_operations \
         (operation_key, operation_id, source_file_version_id, source_bundle_id, \
          source_media_snapshot_id, state, created_at, finished_at) \
         VALUES (?, ?, ?, ?, ?, 'committed', ?, ?)",
    )
    .bind(&input.operation.operation_key)
    .bind(&input.operation.operation_id)
    .bind(i64_from_u64(input.operation.source_file_version_id.0))
    .bind(i64_from_u64(input.operation.source_bundle_id.0))
    .bind(i64_from_u64(input.operation.source_media_snapshot_id.0))
    .bind(recorded_at)
    .bind(recorded_at)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("insert adopted audio operation", error))
    .map(|result| result.last_insert_rowid())
}

async fn insert_legacy_adopted_output(
    tx: &mut Transaction<'_, Sqlite>,
    operation_id: i64,
    input: &NewLegacyAudioExtractAdoption,
) -> Result<u64, VoomError> {
    let probe_payload = serde_json::to_string(&input.probe_payload)
        .map_err(|error| VoomError::Internal(format!("encode adopted probe: {error}")))?;
    let result_facts = serde_json::to_string(&input.result_facts)
        .map_err(|error| VoomError::Internal(format!("encode adopted result facts: {error}")))?;
    let owner = &input.owner;
    let result = sqlx::query(
        "INSERT INTO audio_extract_operation_outputs \
         (operation_id, ordinal, output_id, source_snapshot_stream_id, \
          source_provider_stream_index, bundle_role, target_path, staging_path, temp_path, \
          expected_size_bytes, expected_checksum, artifact_handle_id, artifact_location_id, \
          verification_id, commit_record_id, probe_worker_id, probe_payload, \
          result_file_asset_id, result_file_version_id, result_file_location_id, \
          result_media_snapshot_id, bundle_member_id, result_facts) \
         VALUES (?, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(operation_id)
    .bind(&input.output.output_id)
    .bind(&input.output.source_snapshot_stream_id)
    .bind(i64::from(input.output.source_provider_stream_index))
    .bind(&input.output.bundle_role)
    .bind(&input.output.target_path)
    .bind(&owner.staging_path)
    .bind(&owner.temp_path)
    .bind(i64_from_u64(owner.expected_size_bytes))
    .bind(&owner.expected_checksum)
    .bind(i64_from_u64(owner.artifact_handle_id))
    .bind(i64_from_u64(owner.artifact_location_id))
    .bind(i64_from_u64(owner.verification_id))
    .bind(i64_from_u64(owner.commit_record_id))
    .bind(i64_from_u64(input.probe_worker_id.0))
    .bind(probe_payload)
    .bind(i64_from_u64(owner.result_file_asset_id))
    .bind(i64_from_u64(owner.result_file_version_id))
    .bind(i64_from_u64(owner.result_file_location_id))
    .bind(i64_from_u64(input.result_media_snapshot_id.0))
    .bind(i64_from_u64(owner.bundle_member_id))
    .bind(result_facts)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("insert adopted audio output", error))?;
    u64::try_from(result.last_insert_rowid())
        .map_err(|error| VoomError::Internal(format!("adopted output id: {error}")))
}

async fn insert_legacy_adopted_lineage(
    tx: &mut Transaction<'_, Sqlite>,
    output_id: u64,
    input: &NewLegacyAudioExtractAdoption,
    recorded_at: &str,
) -> Result<(), VoomError> {
    insert_output_lineage(
        tx,
        &AudioExtractLineageInput {
            operation_output_id: output_id,
            source_file_version_id: input.operation.source_file_version_id,
            source_media_snapshot_id: input.operation.source_media_snapshot_id,
            source_snapshot_stream_id: &input.output.source_snapshot_stream_id,
            source_provider_stream_index: input.output.source_provider_stream_index,
            result_file_version_id: FileVersionId(input.owner.result_file_version_id),
            recorded_at,
        },
    )
    .await?;
    Ok(())
}

struct AudioExtractLineageInput<'a> {
    operation_output_id: u64,
    source_file_version_id: FileVersionId,
    source_media_snapshot_id: MediaSnapshotId,
    source_snapshot_stream_id: &'a str,
    source_provider_stream_index: u32,
    result_file_version_id: FileVersionId,
    recorded_at: &'a str,
}

async fn insert_output_lineage(
    tx: &mut Transaction<'_, Sqlite>,
    input: &AudioExtractLineageInput<'_>,
) -> Result<u64, VoomError> {
    let result = sqlx::query(
        "INSERT INTO audio_extract_output_lineage \
         (operation_output_id, source_file_version_id, source_media_snapshot_id, \
          source_snapshot_stream_id, source_provider_stream_index, \
          result_file_version_id, recorded_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(i64_from_u64(input.operation_output_id))
    .bind(i64_from_u64(input.source_file_version_id.0))
    .bind(i64_from_u64(input.source_media_snapshot_id.0))
    .bind(input.source_snapshot_stream_id)
    .bind(i64::from(input.source_provider_stream_index))
    .bind(i64_from_u64(input.result_file_version_id.0))
    .bind(input.recorded_at)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("insert audio extraction lineage", error))?;
    Ok(u64_from_i64(result.last_insert_rowid()))
}

fn validate_new_operation(
    input: &NewAudioExtractOperation,
    outputs: &[NewAudioExtractOutput],
) -> Result<(), VoomError> {
    if input.operation_key.is_empty() {
        return Err(VoomError::Config(
            "audio extraction operation key must not be empty".to_owned(),
        ));
    }
    if outputs.is_empty() {
        return Err(VoomError::Config(
            "audio extraction operation must contain at least one output".to_owned(),
        ));
    }
    Ok(())
}

fn validate_dispatch_attempt(input: &NewAudioExtractDispatchAttempt) -> Result<(), VoomError> {
    if input.idempotency_key.is_empty()
        || input.attempt_directory.is_empty()
        || input.paths.is_empty()
    {
        return Err(VoomError::Config(
            "audio extraction dispatch requires key, directory, and output paths".to_owned(),
        ));
    }
    Ok(())
}

async fn require_live_planned_claim(
    tx: &mut Transaction<'_, Sqlite>,
    claim: &NewAudioExtractClaim,
    now: OffsetDateTime,
) -> Result<i64, VoomError> {
    let now = iso8601(now)?;
    let operation_id = sqlx::query_scalar(
        "SELECT id FROM audio_extract_operations \
         WHERE operation_key = ? AND state = 'planned' AND dispatch_generation = ? \
         AND claim_lease_id = ? AND claim_token = ? AND claim_expires_at > ?",
    )
    .bind(&claim.operation_key)
    .bind(i64::from(claim.expected_generation))
    .bind(i64_from_u64(claim.lease_id.0))
    .bind(&claim.claim_token)
    .bind(now)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| {
        VoomError::database_context("audio_extract_operations require planned claim", error)
    })?;
    operation_id.ok_or_else(|| {
        VoomError::Config(format!(
            "audio extraction operation {} claim is not live for generation {}",
            claim.operation_key, claim.expected_generation
        ))
    })
}

async fn insert_dispatch_attempt(
    tx: &mut Transaction<'_, Sqlite>,
    operation_id: i64,
    claim: &NewAudioExtractClaim,
    attempt: &NewAudioExtractDispatchAttempt,
    now: OffsetDateTime,
) -> Result<i64, VoomError> {
    let result = sqlx::query(
        "INSERT INTO audio_extract_dispatch_attempts \
         (operation_id, generation, worker_id, worker_epoch, idempotency_key, \
          attempt_directory, status, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'active', ?)",
    )
    .bind(operation_id)
    .bind(i64::from(claim.expected_generation))
    .bind(i64_from_u64(attempt.worker_id.0))
    .bind(i64::from(attempt.worker_epoch))
    .bind(&attempt.idempotency_key)
    .bind(&attempt.attempt_directory)
    .bind(iso8601(now)?)
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        VoomError::database_context("audio_extract_dispatch_attempts insert", error)
    })?;
    let attempt_id = result.last_insert_rowid();
    for (ordinal, path) in attempt.paths.iter().enumerate() {
        insert_dispatch_path(tx, attempt_id, ordinal, path).await?;
    }
    Ok(attempt_id)
}

async fn insert_dispatch_path(
    tx: &mut Transaction<'_, Sqlite>,
    attempt_id: i64,
    ordinal: usize,
    path: &str,
) -> Result<(), VoomError> {
    let ordinal = i64::try_from(ordinal)
        .map_err(|error| VoomError::Internal(format!("dispatch path ordinal overflow: {error}")))?;
    sqlx::query(
        "INSERT INTO audio_extract_dispatch_attempt_paths (attempt_id, ordinal, path) \
         VALUES (?, ?, ?)",
    )
    .bind(attempt_id)
    .bind(ordinal)
    .bind(path)
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        VoomError::database_context("audio_extract_dispatch_attempt_paths insert", error)
    })?;
    Ok(())
}

async fn load_dispatch_attempt(
    tx: &mut Transaction<'_, Sqlite>,
    attempt_id: i64,
) -> Result<AudioExtractDispatchAttempt, VoomError> {
    let row = sqlx::query(
        "SELECT id, operation_id, generation, worker_id, worker_epoch, idempotency_key, \
         attempt_directory, status FROM audio_extract_dispatch_attempts WHERE id = ?",
    )
    .bind(attempt_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("audio dispatch attempt get", error))?;
    let paths = sqlx::query_scalar(
        "SELECT path FROM audio_extract_dispatch_attempt_paths \
         WHERE attempt_id = ? ORDER BY ordinal",
    )
    .bind(attempt_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("audio dispatch attempt paths", error))?;
    let status: String = row.try_get("status").map_err(dispatch_row_err)?;
    Ok(AudioExtractDispatchAttempt {
        id: u64_from_i64(row.try_get("id").map_err(dispatch_row_err)?),
        operation_id: u64_from_i64(row.try_get("operation_id").map_err(dispatch_row_err)?),
        generation: u32_from_i64(row.try_get("generation").map_err(dispatch_row_err)?)?,
        worker_id: WorkerId(u64_from_i64(
            row.try_get("worker_id").map_err(dispatch_row_err)?,
        )),
        worker_epoch: u32_from_i64(row.try_get("worker_epoch").map_err(dispatch_row_err)?)?,
        idempotency_key: row.try_get("idempotency_key").map_err(dispatch_row_err)?,
        attempt_directory: row.try_get("attempt_directory").map_err(dispatch_row_err)?,
        paths,
        status: AudioExtractDispatchAttemptStatus::parse(&status)?,
    })
}

async fn insert_planned(
    tx: &mut Transaction<'_, Sqlite>,
    input: &NewAudioExtractOperation,
    outputs: &[NewAudioExtractOutput],
    now: OffsetDateTime,
) -> Result<AudioExtractOperationRecord, VoomError> {
    let created_at = iso8601(now)?;
    let result = sqlx::query(
        "INSERT INTO audio_extract_operations \
         (operation_key, operation_id, source_file_version_id, source_bundle_id, \
          source_media_snapshot_id, state, created_at) \
         VALUES (?, ?, ?, ?, ?, 'planned', ?)",
    )
    .bind(&input.operation_key)
    .bind(&input.operation_id)
    .bind(i64_from_u64(input.source_file_version_id.0))
    .bind(i64_from_u64(input.source_bundle_id.0))
    .bind(i64_from_u64(input.source_media_snapshot_id.0))
    .bind(created_at)
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        VoomError::database_context("audio_extract_operations insert planned", error)
    })?;
    let operation_db_id = result.last_insert_rowid();
    for (ordinal, output) in outputs.iter().enumerate() {
        insert_output(tx, operation_db_id, ordinal, output).await?;
    }
    load_record_by_id(tx, operation_db_id)
        .await?
        .ok_or_else(|| {
            VoomError::Internal(
                "audio extraction operation vanished after planned insert".to_owned(),
            )
        })
}

async fn insert_output(
    tx: &mut Transaction<'_, Sqlite>,
    operation_id: i64,
    ordinal: usize,
    output: &NewAudioExtractOutput,
) -> Result<(), VoomError> {
    let ordinal = i64::try_from(ordinal)
        .map_err(|error| VoomError::Internal(format!("audio output ordinal overflow: {error}")))?;
    sqlx::query(
        "INSERT INTO audio_extract_operation_outputs \
         (operation_id, ordinal, output_id, source_snapshot_stream_id, \
          source_provider_stream_index, bundle_role, target_path) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(operation_id)
    .bind(ordinal)
    .bind(&output.output_id)
    .bind(&output.source_snapshot_stream_id)
    .bind(i64::from(output.source_provider_stream_index))
    .bind(&output.bundle_role)
    .bind(&output.target_path)
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        VoomError::database_context(
            format!("audio_extract_operation_outputs insert ordinal {ordinal}"),
            error,
        )
    })?;
    Ok(())
}

async fn load_record_by_key(
    tx: &mut Transaction<'_, Sqlite>,
    operation_key: &str,
) -> Result<Option<AudioExtractOperationRecord>, VoomError> {
    let row = sqlx::query(
        "SELECT id, operation_key, operation_id, source_file_version_id, \
         source_bundle_id, source_media_snapshot_id, state, dispatch_generation, worker_result \
         FROM audio_extract_operations WHERE operation_key = ?",
    )
    .bind(operation_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("audio_extract_operations get by key", error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    load_record_for_operation_row(tx, row).await.map(Some)
}

async fn load_record_by_id(
    tx: &mut Transaction<'_, Sqlite>,
    id: i64,
) -> Result<Option<AudioExtractOperationRecord>, VoomError> {
    let row = sqlx::query(
        "SELECT id, operation_key, operation_id, source_file_version_id, \
         source_bundle_id, source_media_snapshot_id, state, dispatch_generation, worker_result \
         FROM audio_extract_operations WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("audio_extract_operations get by id", error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    load_record_for_operation_row(tx, row).await.map(Some)
}

async fn load_record_for_operation_row(
    tx: &mut Transaction<'_, Sqlite>,
    row: SqliteRow,
) -> Result<AudioExtractOperationRecord, VoomError> {
    let operation = decode_operation(&row)?;
    let rows = sqlx::query(
        "SELECT id, operation_id, ordinal, output_id, source_snapshot_stream_id, \
         source_provider_stream_index, bundle_role, target_path, staging_path, temp_path, \
         artifact_handle_id, artifact_location_id, verification_id, commit_record_id, \
         probe_worker_id, probe_payload, \
         result_file_asset_id, result_file_version_id, result_file_location_id, \
         result_media_snapshot_id, bundle_member_id, result_facts, \
         (SELECT lineage.id FROM audio_extract_output_lineage lineage \
          WHERE lineage.operation_output_id = audio_extract_operation_outputs.id) AS lineage_id \
         FROM audio_extract_operation_outputs WHERE operation_id = ? ORDER BY ordinal",
    )
    .bind(i64_from_u64(operation.id))
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("audio_extract_operation_outputs list", error))?;
    let outputs = rows
        .iter()
        .map(decode_output)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AudioExtractOperationRecord { operation, outputs })
}

fn decode_operation(row: &SqliteRow) -> Result<AudioExtractOperation, VoomError> {
    let state: String = row.try_get("state").map_err(operation_row_err)?;
    Ok(AudioExtractOperation {
        id: u64_from_i64(row.try_get("id").map_err(operation_row_err)?),
        operation_key: row.try_get("operation_key").map_err(operation_row_err)?,
        operation_id: row.try_get("operation_id").map_err(operation_row_err)?,
        source_file_version_id: FileVersionId(u64_from_i64(
            row.try_get("source_file_version_id")
                .map_err(operation_row_err)?,
        )),
        source_bundle_id: BundleId(u64_from_i64(
            row.try_get("source_bundle_id").map_err(operation_row_err)?,
        )),
        source_media_snapshot_id: MediaSnapshotId(u64_from_i64(
            row.try_get("source_media_snapshot_id")
                .map_err(operation_row_err)?,
        )),
        state: AudioExtractOperationState::parse(&state)?,
        dispatch_generation: u32_from_i64(
            row.try_get("dispatch_generation")
                .map_err(operation_row_err)?,
        )?,
        worker_result: row
            .try_get::<Option<String>, _>("worker_result")
            .map_err(operation_row_err)?
            .map(|value| {
                serde_json::from_str(&value).map_err(|error| {
                    VoomError::database(format!(
                        "audio_extract_operations.worker_result malformed: {error}"
                    ))
                })
            })
            .transpose()?,
    })
}

fn decode_output(row: &SqliteRow) -> Result<AudioExtractOperationOutput, VoomError> {
    Ok(AudioExtractOperationOutput {
        id: u64_from_i64(row.try_get("id").map_err(output_row_err)?),
        operation_id: u64_from_i64(row.try_get("operation_id").map_err(output_row_err)?),
        ordinal: u32_from_i64(row.try_get("ordinal").map_err(output_row_err)?)?,
        output_id: row.try_get("output_id").map_err(output_row_err)?,
        source_snapshot_stream_id: row
            .try_get("source_snapshot_stream_id")
            .map_err(output_row_err)?,
        source_provider_stream_index: u32_from_i64(
            row.try_get("source_provider_stream_index")
                .map_err(output_row_err)?,
        )?,
        bundle_role: row.try_get("bundle_role").map_err(output_row_err)?,
        target_path: row.try_get("target_path").map_err(output_row_err)?,
        staging_path: row.try_get("staging_path").map_err(output_row_err)?,
        temp_path: row.try_get("temp_path").map_err(output_row_err)?,
        artifact_handle_id: optional_id(row, "artifact_handle_id", ArtifactHandleId)?,
        artifact_location_id: optional_id(row, "artifact_location_id", ArtifactLocationId)?,
        verification_id: optional_id(row, "verification_id", ArtifactVerificationId)?,
        commit_record_id: optional_id(row, "commit_record_id", ArtifactCommitRecordId)?,
        probe_worker_id: optional_id(row, "probe_worker_id", WorkerId)?,
        probe_payload: row
            .try_get::<Option<String>, _>("probe_payload")
            .map_err(output_row_err)?
            .map(|value| {
                serde_json::from_str(&value).map_err(|error| {
                    VoomError::database(format!(
                        "audio_extract_operation_outputs.probe_payload malformed: {error}"
                    ))
                })
            })
            .transpose()?,
        result_file_asset_id: row
            .try_get::<Option<i64>, _>("result_file_asset_id")
            .map_err(output_row_err)?
            .map(u64_from_i64),
        result_file_version_id: optional_id(row, "result_file_version_id", FileVersionId)?,
        result_file_location_id: optional_id(row, "result_file_location_id", FileLocationId)?,
        result_media_snapshot_id: optional_id(row, "result_media_snapshot_id", MediaSnapshotId)?,
        bundle_member_id: row
            .try_get::<Option<i64>, _>("bundle_member_id")
            .map_err(output_row_err)?
            .map(u64_from_i64),
        lineage_id: row
            .try_get::<Option<i64>, _>("lineage_id")
            .map_err(output_row_err)?
            .map(u64_from_i64),
        result_facts: row
            .try_get::<Option<String>, _>("result_facts")
            .map_err(output_row_err)?
            .map(|value| {
                serde_json::from_str(&value).map_err(|error| {
                    VoomError::database(format!(
                        "audio_extract_operation_outputs.result_facts malformed: {error}"
                    ))
                })
            })
            .transpose()?,
    })
}

fn optional_id<T>(
    row: &SqliteRow,
    column: &str,
    constructor: impl FnOnce(u64) -> T,
) -> Result<Option<T>, VoomError> {
    Ok(row
        .try_get::<Option<i64>, _>(column)
        .map_err(output_row_err)?
        .map(u64_from_i64)
        .map(constructor))
}

fn require_exact_replay(
    record: &AudioExtractOperationRecord,
    input: &NewAudioExtractOperation,
    outputs: &[NewAudioExtractOutput],
) -> Result<(), VoomError> {
    let operation_matches = record.operation.operation_id == input.operation_id
        && record.operation.source_file_version_id == input.source_file_version_id
        && record.operation.source_bundle_id == input.source_bundle_id
        && record.operation.source_media_snapshot_id == input.source_media_snapshot_id;
    let outputs_match = record.outputs.len() == outputs.len()
        && record
            .outputs
            .iter()
            .zip(outputs)
            .all(|(stored, requested)| output_matches(stored, requested));
    if operation_matches && outputs_match {
        return Ok(());
    }
    Err(VoomError::Config(format!(
        "audio extraction operation {} does not match persisted descriptor",
        input.operation_key
    )))
}

fn output_matches(stored: &AudioExtractOperationOutput, requested: &NewAudioExtractOutput) -> bool {
    stored.output_id == requested.output_id
        && stored.source_snapshot_stream_id == requested.source_snapshot_stream_id
        && stored.source_provider_stream_index == requested.source_provider_stream_index
        && stored.bundle_role == requested.bundle_role
        && stored.target_path == requested.target_path
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err supplies the owned sqlx error; the shared decoder only borrows it"
)]
fn operation_row_err(error: sqlx::Error) -> VoomError {
    map_row_err("audio_extract_operations", &error)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err supplies the owned sqlx error; the shared decoder only borrows it"
)]
fn output_row_err(error: sqlx::Error) -> VoomError {
    map_row_err("audio_extract_operation_outputs", &error)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err supplies the owned sqlx error; the shared decoder only borrows it"
)]
fn dispatch_row_err(error: sqlx::Error) -> VoomError {
    map_row_err("audio_extract_dispatch_attempts", &error)
}

impl AudioExtractOperationState {
    fn parse(value: &str) -> Result<Self, VoomError> {
        match value {
            "planned" => Ok(Self::Planned),
            "staged" => Ok(Self::Staged),
            "prepared" => Ok(Self::Prepared),
            "recovery_required" => Ok(Self::RecoveryRequired),
            "committed" => Ok(Self::Committed),
            other => Err(VoomError::database(format!(
                "audio_extract_operations.state {other:?} not in vocab"
            ))),
        }
    }
}

impl AudioExtractDispatchAttemptStatus {
    fn parse(value: &str) -> Result<Self, VoomError> {
        match value {
            "active" => Ok(Self::Active),
            "terminal" => Ok(Self::Terminal),
            "quarantined" => Ok(Self::Quarantined),
            "quiesced" => Ok(Self::Quiesced),
            other => Err(VoomError::database(format!(
                "audio_extract_dispatch_attempts.status {other:?} not in vocab"
            ))),
        }
    }
}

#[cfg(test)]
#[path = "audio_extract_operations_test.rs"]
mod tests;
