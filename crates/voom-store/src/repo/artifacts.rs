//! `ArtifactRepo` — owns `artifact_handles` + `artifact_locations` + `artifact_lineage`.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, VoomError, WorkerId,
};

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
    pub file_version_id: Option<FileVersionId>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ArtifactHandle {
    pub id: ArtifactHandleId,
    pub file_version_id: Option<FileVersionId>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactVerificationStatus {
    Succeeded,
    Failed,
}

impl ArtifactVerificationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            other => Err(VoomError::Database(format!(
                "artifact_verifications.status {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewArtifactVerification {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub path: String,
    pub worker_id: WorkerId,
    pub status: ArtifactVerificationStatus,
    pub expected_size_bytes: u64,
    pub expected_checksum: String,
    pub observed_size_bytes: Option<u64>,
    pub observed_checksum: Option<String>,
    pub failure_class: Option<String>,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub report: JsonValue,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ArtifactVerification {
    pub id: ArtifactVerificationId,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub path: String,
    pub worker_id: WorkerId,
    pub status: ArtifactVerificationStatus,
    pub expected_size_bytes: u64,
    pub expected_checksum: String,
    pub observed_size_bytes: Option<u64>,
    pub observed_checksum: Option<String>,
    pub failure_class: Option<String>,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub report: JsonValue,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactCommitState {
    Pending,
    Committed,
    Failed,
    RecoveryRequired,
}

impl ArtifactCommitState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Committed => "committed",
            Self::Failed => "failed",
            Self::RecoveryRequired => "recovery_required",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "pending" => Ok(Self::Pending),
            "committed" => Ok(Self::Committed),
            "failed" => Ok(Self::Failed),
            "recovery_required" => Ok(Self::RecoveryRequired),
            other => Err(VoomError::Database(format!(
                "artifact_commit_records.state {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewArtifactCommitRecord {
    pub artifact_handle_id: ArtifactHandleId,
    pub source_file_version_id: FileVersionId,
    pub verification_id: ArtifactVerificationId,
    pub target_path: String,
    pub temp_path: Option<String>,
    pub report: JsonValue,
    pub started_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ArtifactCommitRecord {
    pub id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub source_file_version_id: FileVersionId,
    pub verification_id: ArtifactVerificationId,
    pub target_path: String,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
    pub state: ArtifactCommitState,
    pub failure_class: Option<String>,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub recovery_reason: Option<String>,
    pub temp_path: Option<String>,
    pub report: JsonValue,
    pub started_at: OffsetDateTime,
    pub promotion_started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct ArtifactCommitFailure {
    pub failure_class: String,
    pub error_code: String,
    pub message: String,
    pub finished_at: OffsetDateTime,
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

    async fn record_verification_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactVerification,
    ) -> Result<ArtifactVerification, VoomError>;
    async fn latest_successful_verification_for_live_staging_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        handle_id: ArtifactHandleId,
    ) -> Result<Option<ArtifactVerification>, VoomError>;
    async fn list_verifications(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactVerification>, VoomError>;

    async fn create_pending_commit_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactCommitRecord,
    ) -> Result<ArtifactCommitRecord, VoomError>;
    async fn mark_commit_committed_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        result_file_version_id: FileVersionId,
        result_file_location_id: FileLocationId,
        promotion_started_at: OffsetDateTime,
        finished_at: OffsetDateTime,
    ) -> Result<ArtifactCommitRecord, VoomError>;
    async fn mark_commit_failed_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        failure: ArtifactCommitFailure,
    ) -> Result<ArtifactCommitRecord, VoomError>;
    async fn mark_commit_recovery_required_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        failure: ArtifactCommitFailure,
        recovery_reason: String,
    ) -> Result<ArtifactCommitRecord, VoomError>;
    async fn get_commit_record(
        &self,
        id: ArtifactCommitRecordId,
    ) -> Result<Option<ArtifactCommitRecord>, VoomError>;
    async fn list_commit_records(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactCommitRecord>, VoomError>;
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
        .bind(input.file_version_id.map(|id| i64_from_u64(id.0)))
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_handles insert: {e}")))?;
        Ok(ArtifactHandle {
            id: ArtifactHandleId(u64_from_i64(res.last_insert_rowid())),
            file_version_id: input.file_version_id,
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
            "SELECT id, file_version_id, privacy_class, durability_class, mutability, created_at \
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

    async fn record_verification_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactVerification,
    ) -> Result<ArtifactVerification, VoomError> {
        let owner: Option<(i64, String)> =
            sqlx::query_as("SELECT artifact_handle_id, value FROM artifact_locations WHERE id = ?")
                .bind(i64_from_u64(input.artifact_location_id.0))
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| {
                    VoomError::Database(format!("artifact_locations owner lookup: {e}"))
                })?;
        let (owner_id, location_value) = owner.ok_or_else(|| {
            VoomError::NotFound(format!(
                "artifact_locations {} missing",
                input.artifact_location_id
            ))
        })?;
        if u64_from_i64(owner_id) != input.artifact_handle_id.0 {
            return Err(VoomError::Conflict(format!(
                "artifact_verifications: location {} belongs to artifact_handle {}",
                input.artifact_location_id,
                ArtifactHandleId(u64_from_i64(owner_id))
            )));
        }
        if input.path != location_value {
            return Err(VoomError::Conflict(format!(
                "artifact_verifications: path {:?} does not match artifact_location {} value {:?}",
                input.path, input.artifact_location_id, location_value
            )));
        }

        let report = serialize_json(&input.report, "artifact_verifications.report")?;
        let started_at = iso8601(input.started_at)?;
        let finished_at = iso8601(input.finished_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_verifications \
             (artifact_handle_id, artifact_location_id, path, worker_id, status, \
              expected_size_bytes, expected_checksum, observed_size_bytes, observed_checksum, \
              failure_class, error_code, message, report, started_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.artifact_handle_id.0))
        .bind(i64_from_u64(input.artifact_location_id.0))
        .bind(&input.path)
        .bind(i64_from_u64(input.worker_id.0))
        .bind(input.status.as_str())
        .bind(i64_from_u64(input.expected_size_bytes))
        .bind(&input.expected_checksum)
        .bind(input.observed_size_bytes.map(i64_from_u64))
        .bind(&input.observed_checksum)
        .bind(&input.failure_class)
        .bind(&input.error_code)
        .bind(&input.message)
        .bind(report)
        .bind(&started_at)
        .bind(&finished_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_verifications insert: {e}")))?;

        Ok(ArtifactVerification {
            id: ArtifactVerificationId(u64_from_i64(res.last_insert_rowid())),
            artifact_handle_id: input.artifact_handle_id,
            artifact_location_id: input.artifact_location_id,
            path: input.path,
            worker_id: input.worker_id,
            status: input.status,
            expected_size_bytes: input.expected_size_bytes,
            expected_checksum: input.expected_checksum,
            observed_size_bytes: input.observed_size_bytes,
            observed_checksum: input.observed_checksum,
            failure_class: input.failure_class,
            error_code: input.error_code,
            message: input.message,
            report: input.report,
            started_at: input.started_at,
            finished_at: input.finished_at,
        })
    }

    async fn latest_successful_verification_for_live_staging_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        handle_id: ArtifactHandleId,
    ) -> Result<Option<ArtifactVerification>, VoomError> {
        let sql = SELECT_ARTIFACT_VERIFICATION_COLS.to_owned()
            + " \
             FROM artifact_verifications v \
             JOIN artifact_locations l ON l.id = v.artifact_location_id \
             WHERE v.artifact_handle_id = ? AND v.status = 'succeeded' \
               AND l.artifact_handle_id = v.artifact_handle_id \
               AND l.kind = 'staging' AND l.retired_at IS NULL \
             ORDER BY v.id DESC LIMIT 1";
        let row = sqlx::query(&sql)
            .bind(i64_from_u64(handle_id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("artifact_verifications latest: {e}")))?;
        row.as_ref().map(row_to_verification).transpose()
    }

    async fn list_verifications(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactVerification>, VoomError> {
        let sql = SELECT_ARTIFACT_VERIFICATION_COLS.to_owned()
            + " \
             FROM artifact_verifications v \
             WHERE v.artifact_handle_id = ? ORDER BY v.id ASC";
        let rows = sqlx::query(&sql)
            .bind(i64_from_u64(handle_id.0))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("artifact_verifications list: {e}")))?;
        rows.iter().map(row_to_verification).collect()
    }

    async fn create_pending_commit_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactCommitRecord,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        validate_commit_verification(tx, &input).await?;
        let report = serialize_json(&input.report, "artifact_commit_records.report")?;
        let started_at = iso8601(input.started_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_commit_records \
             (artifact_handle_id, source_file_version_id, verification_id, target_path, \
              state, temp_path, report, started_at) \
             VALUES (?, ?, ?, ?, 'pending', ?, ?, ?)",
        )
        .bind(i64_from_u64(input.artifact_handle_id.0))
        .bind(i64_from_u64(input.source_file_version_id.0))
        .bind(i64_from_u64(input.verification_id.0))
        .bind(&input.target_path)
        .bind(&input.temp_path)
        .bind(report)
        .bind(&started_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| map_commit_insert_err(&e, input.artifact_handle_id, &input.target_path))?;

        Ok(ArtifactCommitRecord {
            id: ArtifactCommitRecordId(u64_from_i64(res.last_insert_rowid())),
            artifact_handle_id: input.artifact_handle_id,
            source_file_version_id: input.source_file_version_id,
            verification_id: input.verification_id,
            target_path: input.target_path,
            result_file_version_id: None,
            result_file_location_id: None,
            state: ArtifactCommitState::Pending,
            failure_class: None,
            error_code: None,
            message: None,
            recovery_reason: None,
            temp_path: input.temp_path,
            report: input.report,
            started_at: input.started_at,
            promotion_started_at: None,
            finished_at: None,
        })
    }

    async fn mark_commit_committed_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        result_file_version_id: FileVersionId,
        result_file_location_id: FileLocationId,
        promotion_started_at: OffsetDateTime,
        finished_at: OffsetDateTime,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        validate_committed_result(tx, id, result_file_version_id, result_file_location_id).await?;
        let promotion_started_at = iso8601(promotion_started_at)?;
        let finished_at = iso8601(finished_at)?;
        let res = sqlx::query(
            "UPDATE artifact_commit_records \
             SET state = 'committed', result_file_version_id = ?, result_file_location_id = ?, \
                 promotion_started_at = ?, finished_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(i64_from_u64(result_file_version_id.0))
        .bind(i64_from_u64(result_file_location_id.0))
        .bind(&promotion_started_at)
        .bind(&finished_at)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_commit_records commit: {e}")))?;
        changed_commit_record(tx, id, res.rows_affected(), "commit").await
    }

    async fn mark_commit_failed_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        failure: ArtifactCommitFailure,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        let finished_at = iso8601(failure.finished_at)?;
        let res = sqlx::query(
            "UPDATE artifact_commit_records \
             SET state = 'failed', failure_class = ?, error_code = ?, message = ?, finished_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(&failure.failure_class)
        .bind(&failure.error_code)
        .bind(&failure.message)
        .bind(&finished_at)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_commit_records fail: {e}")))?;
        changed_commit_record(tx, id, res.rows_affected(), "fail").await
    }

    async fn mark_commit_recovery_required_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        failure: ArtifactCommitFailure,
        recovery_reason: String,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        let finished_at = iso8601(failure.finished_at)?;
        let res = sqlx::query(
            "UPDATE artifact_commit_records \
             SET state = 'recovery_required', failure_class = ?, error_code = ?, message = ?, \
                 recovery_reason = ?, finished_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(&failure.failure_class)
        .bind(&failure.error_code)
        .bind(&failure.message)
        .bind(&recovery_reason)
        .bind(&finished_at)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            VoomError::Database(format!("artifact_commit_records recovery_required: {e}"))
        })?;
        changed_commit_record(tx, id, res.rows_affected(), "recovery_required").await
    }

    async fn get_commit_record(
        &self,
        id: ArtifactCommitRecordId,
    ) -> Result<Option<ArtifactCommitRecord>, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
            + " FROM artifact_commit_records c WHERE c.id = ?";
        let row = sqlx::query(&sql)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("artifact_commit_records get: {e}")))?;
        row.as_ref().map(row_to_commit_record).transpose()
    }

    async fn list_commit_records(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactCommitRecord>, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
            + " \
             FROM artifact_commit_records c \
             WHERE c.artifact_handle_id = ? ORDER BY c.id ASC";
        let rows = sqlx::query(&sql)
            .bind(i64_from_u64(handle_id.0))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("artifact_commit_records list: {e}")))?;
        rows.iter().map(row_to_commit_record).collect()
    }
}

const SELECT_ARTIFACT_VERIFICATION_COLS: &str = "SELECT v.id, v.artifact_handle_id, \
    v.artifact_location_id, v.path, v.worker_id, v.status, v.expected_size_bytes, \
    v.expected_checksum, v.observed_size_bytes, v.observed_checksum, v.failure_class, \
    v.error_code, v.message, v.report, v.started_at, v.finished_at";

const SELECT_ARTIFACT_COMMIT_RECORD_COLS: &str = "SELECT c.id, c.artifact_handle_id, \
    c.source_file_version_id, c.verification_id, c.target_path, c.result_file_version_id, \
    c.result_file_location_id, c.state, c.failure_class, c.error_code, c.message, \
    c.recovery_reason, c.temp_path, c.report, c.started_at, c.promotion_started_at, \
    c.finished_at";

type CommitVerificationRow = (
    i64,
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    Option<i64>,
    Option<String>,
    Option<i64>,
);

async fn validate_commit_verification(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &NewArtifactCommitRecord,
) -> Result<(), VoomError> {
    let row: Option<CommitVerificationRow> = sqlx::query_as(
        "SELECT v.id, v.artifact_handle_id, v.status, v.path, l.kind, l.value, l.retired_at, \
                l.artifact_handle_id, h.file_version_id, fv.retired_at, \
                (SELECT MAX(v2.id) \
                   FROM artifact_verifications v2 \
                  WHERE v2.artifact_handle_id = v.artifact_handle_id \
                    AND v2.artifact_location_id = v.artifact_location_id \
                    AND v2.status = 'succeeded') AS latest_successful_id \
         FROM artifact_verifications v \
         JOIN artifact_locations l ON l.id = v.artifact_location_id \
         JOIN artifact_handles h ON h.id = v.artifact_handle_id \
         LEFT JOIN file_versions fv ON fv.id = h.file_version_id \
         WHERE v.id = ?",
    )
    .bind(i64_from_u64(input.verification_id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("artifact_verifications commit lookup: {e}")))?;
    let Some((
        verification_id,
        verification_handle_id,
        status,
        verification_path,
        location_kind,
        location_value,
        retired_at,
        location_handle_id,
        handle_file_version_id,
        source_retired_at,
        latest_successful_id,
    )) = row
    else {
        return Err(VoomError::NotFound(format!(
            "artifact_verifications {} missing",
            input.verification_id
        )));
    };
    if u64_from_i64(verification_handle_id) != input.artifact_handle_id.0
        || u64_from_i64(location_handle_id) != input.artifact_handle_id.0
        || status != ArtifactVerificationStatus::Succeeded.as_str()
        || verification_path != location_value
        || location_kind != "staging"
        || retired_at.is_some()
        || latest_successful_id != Some(i64_from_u64(input.verification_id.0))
    {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records: verification {} is not a successful live staging \
             verification for artifact_handle {}",
            input.verification_id, input.artifact_handle_id
        )));
    }
    if handle_file_version_id != Some(i64_from_u64(input.source_file_version_id.0)) {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records: source_file_version_id {} does not match \
             artifact_handle {} file_version_id",
            input.source_file_version_id, input.artifact_handle_id
        )));
    }
    if verification_id != i64_from_u64(input.verification_id.0) || source_retired_at.is_some() {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records: source_file_version_id {} is not live",
            input.source_file_version_id
        )));
    }
    Ok(())
}

async fn validate_committed_result(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: ArtifactCommitRecordId,
    result_file_version_id: FileVersionId,
    result_file_location_id: FileLocationId,
) -> Result<(), VoomError> {
    let pending_row: Option<(i64, String)> = sqlx::query_as(
        "SELECT source_file_version_id, target_path FROM artifact_commit_records \
         WHERE id = ? AND state = 'pending'",
    )
    .bind(i64_from_u64(commit_id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("artifact_commit_records pending lookup: {e}")))?;
    let (source_version_id, target_path) = pending_row.ok_or_else(|| {
        VoomError::Conflict(format!(
            "artifact_commit_records commit: id={commit_id} not pending"
        ))
    })?;

    let version_row: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT produced_by, produced_from_version_id, retired_at FROM file_versions WHERE id = ?",
    )
    .bind(i64_from_u64(result_file_version_id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("file_versions commit-result lookup: {e}")))?;
    let Some((produced_by, produced_from_version_id, result_retired_at)) = version_row else {
        return Err(VoomError::NotFound(format!(
            "file_versions {result_file_version_id} missing"
        )));
    };
    if produced_by != "staged_commit"
        || produced_from_version_id != Some(source_version_id)
        || result_retired_at.is_some()
    {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records commit: result version {result_file_version_id} \
             is not a staged_commit child of source version {}",
            FileVersionId(u64_from_i64(source_version_id))
        )));
    }

    let location_row: Option<(i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT file_version_id, kind, value, retired_at FROM file_locations WHERE id = ?",
    )
    .bind(i64_from_u64(result_file_location_id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("file_locations commit-result lookup: {e}")))?;
    let (location_version_id, location_kind, location_value, retired_at) = location_row
        .ok_or_else(|| {
            VoomError::NotFound(format!("file_locations {result_file_location_id} missing"))
        })?;
    if u64_from_i64(location_version_id) != result_file_version_id.0
        || location_kind != "local_path"
        || location_value != target_path
        || retired_at.is_some()
    {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records commit: result location {result_file_location_id} \
             does not match committed target {target_path:?} for file_version {result_file_version_id}"
        )));
    }
    Ok(())
}

fn map_commit_insert_err(
    err: &sqlx::Error,
    artifact_handle_id: ArtifactHandleId,
    target_path: &str,
) -> VoomError {
    if is_unique_violation(err) {
        VoomError::Conflict(format!(
            "artifact_commit_records: artifact_handle {artifact_handle_id} or target_path \
             {target_path:?} already has an active owner"
        ))
    } else {
        VoomError::Database(format!("artifact_commit_records insert: {err}"))
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

fn row_to_handle(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactHandle, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("artifacts", &e))?;
    let file_version_id: Option<i64> = row
        .try_get("file_version_id")
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
        file_version_id: file_version_id.map(|v| FileVersionId(u64_from_i64(v))),
        privacy_class,
        durability_class,
        mutability,
        created_at: parse_iso8601(&created)?,
    })
}

fn row_to_verification(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactVerification, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let artifact_handle_id: i64 = row
        .try_get("artifact_handle_id")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let artifact_location_id: i64 = row
        .try_get("artifact_location_id")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let path: String = row
        .try_get("path")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let worker_id: i64 = row
        .try_get("worker_id")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let status: String = row
        .try_get("status")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let expected_size_bytes: i64 = row
        .try_get("expected_size_bytes")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let expected_checksum: String = row
        .try_get("expected_checksum")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let observed_size_bytes: Option<i64> = row
        .try_get("observed_size_bytes")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let observed_checksum: Option<String> = row
        .try_get("observed_checksum")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let failure_class: Option<String> = row
        .try_get("failure_class")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let error_code: Option<String> = row
        .try_get("error_code")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let message: Option<String> = row
        .try_get("message")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let report: String = row
        .try_get("report")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let started_at: String = row
        .try_get("started_at")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let finished_at: String = row
        .try_get("finished_at")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;

    Ok(ArtifactVerification {
        id: ArtifactVerificationId(u64_from_i64(id)),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(artifact_handle_id)),
        artifact_location_id: ArtifactLocationId(u64_from_i64(artifact_location_id)),
        path,
        worker_id: WorkerId(u64_from_i64(worker_id)),
        status: ArtifactVerificationStatus::parse(&status)?,
        expected_size_bytes: u64_from_i64(expected_size_bytes),
        expected_checksum,
        observed_size_bytes: observed_size_bytes.map(u64_from_i64),
        observed_checksum,
        failure_class,
        error_code,
        message,
        report: serde_json::from_str(&report)
            .map_err(|e| VoomError::Database(format!("artifact_verifications report: {e}")))?,
        started_at: parse_iso8601(&started_at)?,
        finished_at: parse_iso8601(&finished_at)?,
    })
}

async fn changed_commit_record(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactCommitRecordId,
    rows_affected: u64,
    operation: &str,
) -> Result<ArtifactCommitRecord, VoomError> {
    if rows_affected != 1 {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records {operation}: id={id} not pending"
        )));
    }
    get_commit_record_in_tx(tx, id).await?.ok_or_else(|| {
        VoomError::Internal(format!(
            "artifact_commit_records post-{operation} get vanished: {id}"
        ))
    })
}

async fn get_commit_record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactCommitRecordId,
) -> Result<Option<ArtifactCommitRecord>, VoomError> {
    let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
        + " FROM artifact_commit_records c WHERE c.id = ?";
    let row = sqlx::query(&sql)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_commit_records get: {e}")))?;
    row.as_ref().map(row_to_commit_record).transpose()
}

fn row_to_commit_record(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactCommitRecord, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let artifact_handle_id: i64 = row
        .try_get("artifact_handle_id")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let source_file_version_id: i64 = row
        .try_get("source_file_version_id")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let verification_id: i64 = row
        .try_get("verification_id")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let target_path: String = row
        .try_get("target_path")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let result_file_version_id: Option<i64> = row
        .try_get("result_file_version_id")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let result_file_location_id: Option<i64> = row
        .try_get("result_file_location_id")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let state: String = row
        .try_get("state")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let failure_class: Option<String> = row
        .try_get("failure_class")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let error_code: Option<String> = row
        .try_get("error_code")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let message: Option<String> = row
        .try_get("message")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let recovery_reason: Option<String> = row
        .try_get("recovery_reason")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let temp_path: Option<String> = row
        .try_get("temp_path")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let report: String = row
        .try_get("report")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let started_at: String = row
        .try_get("started_at")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let promotion_started_at: Option<String> = row
        .try_get("promotion_started_at")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;
    let finished_at: Option<String> = row
        .try_get("finished_at")
        .map_err(|e| map_row_err("artifact_commit_records", &e))?;

    Ok(ArtifactCommitRecord {
        id: ArtifactCommitRecordId(u64_from_i64(id)),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(artifact_handle_id)),
        source_file_version_id: FileVersionId(u64_from_i64(source_file_version_id)),
        verification_id: ArtifactVerificationId(u64_from_i64(verification_id)),
        target_path,
        result_file_version_id: result_file_version_id.map(|v| FileVersionId(u64_from_i64(v))),
        result_file_location_id: result_file_location_id.map(|v| FileLocationId(u64_from_i64(v))),
        state: ArtifactCommitState::parse(&state)?,
        failure_class,
        error_code,
        message,
        recovery_reason,
        temp_path,
        report: serde_json::from_str(&report)
            .map_err(|e| VoomError::Database(format!("artifact_commit_records report: {e}")))?,
        started_at: parse_iso8601(&started_at)?,
        promotion_started_at: promotion_started_at
            .map(|s| parse_iso8601(&s))
            .transpose()?,
        finished_at: finished_at.map(|s| parse_iso8601(&s)).transpose()?,
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
