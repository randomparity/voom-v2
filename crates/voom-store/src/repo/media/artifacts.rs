//! Artifact persistence split into semantic repository contracts.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileAssetId, FileLocationId, FileVersionId, JobId,
    LeaseId, MediaSnapshotId, TicketId, VoomError, WorkerId,
};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
};
use crate::repo::execution::leases::LeaseState;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactHandle {
    pub id: ArtifactHandleId,
    pub file_version_id: Option<FileVersionId>,
    pub privacy_class: String,
    pub durability_class: String,
    pub mutability: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactHandleFacts {
    pub handle: ArtifactHandle,
    pub size_bytes: Option<u64>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactExpectedFacts {
    pub source_file_version_id: Option<FileVersionId>,
    pub size_bytes: u64,
    pub checksum: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveArtifactLocation {
    pub id: ArtifactLocationId,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct PolicyArtifactTarget {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub file_version_id: FileVersionId,
    pub file_location_id: FileLocationId,
    pub media_snapshot_id: MediaSnapshotId,
    pub path: String,
    pub size_bytes: u64,
    pub checksum: String,
}

#[derive(Debug, Clone)]
pub struct PolicyArtifactResolution {
    pub target: PolicyArtifactTarget,
    pub created_handle: Option<ArtifactHandle>,
    pub created_location: Option<ArtifactLocation>,
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
            other => Err(VoomError::database(format!(
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
    pub workflow_ticket_id: Option<TicketId>,
    pub workflow_lease_id: Option<LeaseId>,
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
    pub workflow_ticket_id: Option<TicketId>,
    pub workflow_lease_id: Option<LeaseId>,
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
            other => Err(VoomError::database(format!(
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
pub struct ArtifactCommitEvidence {
    pub id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub source_file_version_id: FileVersionId,
    pub verification_id: ArtifactVerificationId,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
    pub state: ArtifactCommitState,
    pub report: JsonValue,
    pub started_at: OffsetDateTime,
    pub promotion_started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct ArtifactVerificationEvidence {
    pub artifact_handle_id: ArtifactHandleId,
    pub workflow_ticket_id: Option<TicketId>,
    pub workflow_lease_id: Option<LeaseId>,
    pub status: ArtifactVerificationStatus,
    pub report: JsonValue,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ResultLeaseEvidence {
    pub ticket_id: TicketId,
    pub state: LeaseState,
    pub acquired_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub last_heartbeat_at: OffsetDateTime,
    pub released_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct CommittedTicketEvidence {
    pub ticket_id: TicketId,
    pub ticket_job_id: Option<JobId>,
    pub ticket_payload: JsonValue,
    pub result: JsonValue,
    pub commit: Option<ArtifactCommitEvidence>,
    pub verification: Option<ArtifactVerificationEvidence>,
    pub result_lease: Option<ResultLeaseEvidence>,
    pub source_file_asset_id: Option<FileAssetId>,
    pub result_file_asset_id: Option<FileAssetId>,
    pub location_file_version_id: Option<FileVersionId>,
    pub snapshot_file_version_id: Option<FileVersionId>,
}

#[derive(Debug, Clone)]
pub struct VerifiedTicketEvidence {
    pub verification: ArtifactVerification,
    pub file_version_id: Option<FileVersionId>,
    pub location_value: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ArtifactCommitFailure {
    pub failure_class: String,
    pub error_code: String,
    pub message: String,
    pub finished_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct NewSidecarArtifactCommit {
    pub commit_record_id: ArtifactCommitRecordId,
    pub target_path: String,
    pub content_hash: String,
    pub size_bytes: u64,
    pub observed_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SidecarArtifactCommit {
    pub commit_record: ArtifactCommitRecord,
    pub file_asset_id: FileAssetId,
    pub file_version_id: FileVersionId,
    pub file_location_id: FileLocationId,
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

    pub async fn committed_ticket_evidence(
        &self,
        ticket_ids: &[TicketId],
    ) -> Result<Vec<CommittedTicketEvidence>, VoomError> {
        if ticket_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = ticket_ids
            .iter()
            .map(|id| checked_sqlite_id(id.0, "committed evidence ticket id"))
            .collect::<Result<Vec<_>, _>>()?;
        let ids = serialize_json(&ids, "committed evidence ticket ids")?;
        let rows = sqlx::query(COMMITTED_TICKET_EVIDENCE_SQL)
            .bind(ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| VoomError::database_context("committed ticket evidence", error))?;
        rows.iter().map(decode_committed_ticket_evidence).collect()
    }

    pub async fn verified_ticket_evidence(
        &self,
        _ticket_id: TicketId,
        verification_id: ArtifactVerificationId,
        _handle_id: ArtifactHandleId,
        location_id: FileLocationId,
    ) -> Result<Option<VerifiedTicketEvidence>, VoomError> {
        let sql = format!(
            "{SELECT_ARTIFACT_VERIFICATION_COLS}, \
             fl.file_version_id AS selected_file_version_id, \
             fl.value AS selected_location_value, \
             l.ticket_id AS selected_lease_ticket_id \
             FROM artifact_verifications v \
             LEFT JOIN leases l ON l.id = v.workflow_lease_id \
             LEFT JOIN file_locations fl ON fl.id = ? AND fl.retired_at IS NULL \
             WHERE v.id = ?"
        );
        let row = sqlx::query(&sql)
            .bind(checked_sqlite_id(
                location_id.0,
                "verified file location id",
            )?)
            .bind(checked_sqlite_id(
                verification_id.0,
                "verified artifact verification id",
            )?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| VoomError::database_context("verified ticket evidence", error))?;
        row.as_ref()
            .map(decode_verified_ticket_evidence)
            .transpose()
    }
}

impl Repository for SqliteArtifactRepo {}

#[async_trait]
pub trait ArtifactHandleRepo: Repository {
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

#[async_trait]
pub trait ArtifactVerificationRepo: Repository {
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
    async fn verification_for_workflow_lease(
        &self,
        lease_id: LeaseId,
    ) -> Result<Option<ArtifactVerification>, VoomError>;
    async fn verifications_for_workflow_job(
        &self,
        job_id: JobId,
    ) -> Result<Vec<ArtifactVerification>, VoomError>;
}

#[async_trait]
pub trait ArtifactCommitRepo: Repository {
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
    async fn record_verified_sidecar_commit_rows_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewSidecarArtifactCommit,
    ) -> Result<SidecarArtifactCommit, VoomError>;
}

impl SqliteArtifactRepo {
    pub async fn create_handle_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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
        .bind(&input.kind)
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
            "SELECT id, file_version_id, privacy_class, durability_class, mutability, created_at \
             FROM artifact_handles WHERE id = ?",
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
            "SELECT id, file_version_id, privacy_class, durability_class, mutability, \
                    created_at, size_bytes, checksum \
             FROM artifact_handles WHERE id = ?",
        )
        .bind(checked_sqlite_id(handle_id.0, "artifact handle id")?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("artifact_handles facts", error))?
        .ok_or_else(|| VoomError::NotFound(format!("artifact_handles {handle_id} missing")))?;
        let size_bytes: Option<i64> = row
            .try_get("size_bytes")
            .map_err(|error| map_row_err("artifact_handles.size_bytes", &error))?;
        let checksum = row
            .try_get("checksum")
            .map_err(|error| map_row_err("artifact_handles.checksum", &error))?;
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
        kind: &str,
    ) -> Result<Option<LiveArtifactLocation>, VoomError> {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, kind, value FROM artifact_locations \
             WHERE artifact_handle_id = ? AND kind = ? AND retired_at IS NULL \
             ORDER BY id ASC",
        )
        .bind(checked_sqlite_id(handle_id.0, "artifact handle id")?)
        .bind(kind)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("artifact_locations live kind", error))?;
        match rows.as_slice() {
            [] => Ok(None),
            [(id, kind, value)] => Ok(Some(LiveArtifactLocation {
                id: ArtifactLocationId(u64::try_from(*id).map_err(|error| {
                    VoomError::database_context("artifact_locations.id negative", error)
                })?),
                kind: kind.clone(),
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
        kind: &str,
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

    pub async fn resolve_policy_artifact_target_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        file_version_id: FileVersionId,
        file_location_id: Option<FileLocationId>,
        now: OffsetDateTime,
    ) -> Result<PolicyArtifactResolution, VoomError> {
        let version = require_active_policy_file_version(tx, file_version_id).await?;
        let location = select_policy_file_location(tx, file_version_id, file_location_id).await?;
        let media_snapshot_id = latest_policy_media_snapshot(tx, file_version_id).await?;
        let (handle, created_handle) = self
            .resolve_policy_artifact_handle(tx, &version, &location, now)
            .await?;
        let (artifact_location, created_location) = self
            .resolve_policy_artifact_location(tx, handle.id, &location, now)
            .await?;

        Ok(PolicyArtifactResolution {
            target: PolicyArtifactTarget {
                artifact_handle_id: handle.id,
                artifact_location_id: artifact_location.id,
                file_version_id,
                file_location_id: location.id,
                media_snapshot_id,
                path: location.value,
                size_bytes: version.size_bytes,
                checksum: version.content_hash,
            },
            created_handle,
            created_location,
        })
    }

    async fn resolve_policy_artifact_handle(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        version: &PolicyFileVersion,
        location: &PolicyFileLocation,
        now: OffsetDateTime,
    ) -> Result<(ArtifactHandle, Option<ArtifactHandle>), VoomError> {
        let committed = policy_committed_handles(tx, version.id).await?;
        match committed.as_slice() {
            [] => {}
            [handle] => {
                require_policy_handle_content_facts(tx, handle.id, version).await?;
                return Ok((handle.clone(), None));
            }
            _ => {
                return Err(VoomError::Conflict(format!(
                    "file_version {} has {} committed artifact handles",
                    version.id,
                    committed.len()
                )));
            }
        }
        let handle = policy_canonical_handle(tx, version.id).await?;
        if let Some(handle) = handle {
            require_policy_handle_facts(tx, handle.id, version).await?;
            return Ok((handle, None));
        }

        let handle = self
            .create_handle_in_tx(
                tx,
                NewArtifactHandle {
                    size_bytes: Some(i64_from_u64(
                        version.size_bytes,
                        concat!(module_path!(), ": ", stringify!(version.size_bytes)),
                    )?),
                    checksum: Some(version.content_hash.clone()),
                    privacy_class: "internal".to_owned(),
                    durability_class: "active".to_owned(),
                    allowed_access_modes: vec!["local_path".to_owned()],
                    mutability: "immutable".to_owned(),
                    source_lineage: Some(serde_json::json!({
                        "kind": "policy_verification",
                        "file_version_id": version.id.0,
                        "file_location_id": location.id.0,
                    })),
                    file_version_id: Some(version.id),
                    created_at: now,
                },
            )
            .await?;
        Ok((handle.clone(), Some(handle)))
    }

    async fn resolve_policy_artifact_location(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        handle_id: ArtifactHandleId,
        location: &PolicyFileLocation,
        now: OffsetDateTime,
    ) -> Result<(ArtifactLocation, Option<ArtifactLocation>), VoomError> {
        let rows = sqlx::query(
            "SELECT id, artifact_handle_id, kind, value, observed_at, retired_at \
             FROM artifact_locations \
             WHERE artifact_handle_id = ? AND kind = 'local_path' \
               AND value = ? AND retired_at IS NULL \
             ORDER BY id",
        )
        .bind(i64_from_u64(
            handle_id.0,
            concat!(module_path!(), ": ", stringify!(handle_id.0)),
        )?)
        .bind(&location.value)
        .fetch_all(&mut **tx)
        .await
        .map_err(|err| VoomError::database_context("policy artifact location lookup", err))?;
        match rows.as_slice() {
            [] => {
                let created = self
                    .record_location_in_tx(
                        tx,
                        NewArtifactLocation {
                            artifact_handle_id: handle_id,
                            kind: "local_path".to_owned(),
                            value: location.value.clone(),
                            observed_at: now,
                        },
                    )
                    .await?;
                Ok((created.clone(), Some(created)))
            }
            [row] => Ok((row_to_location(row)?, None)),
            _ => Err(VoomError::Conflict(format!(
                "artifact_handle {handle_id} has {} live local_path locations for {:?}",
                rows.len(),
                location.value
            ))),
        }
    }

    pub async fn record_verification_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewArtifactVerification,
    ) -> Result<ArtifactVerification, VoomError> {
        if input.workflow_ticket_id.is_some() != input.workflow_lease_id.is_some() {
            return Err(VoomError::Config(
                "artifact_verifications workflow ticket and lease must be both set or both absent"
                    .to_owned(),
            ));
        }
        validate_verification_location(tx, &input).await?;

        let report = serialize_json(&input.report, "artifact_verifications.report")?;
        let started_at = iso8601(input.started_at)?;
        let finished_at = iso8601(input.finished_at)?;
        let artifact_handle_id = i64_from_u64(
            input.artifact_handle_id.0,
            "artifact_verifications.artifact_handle_id",
        )?;
        let artifact_location_id = i64_from_u64(
            input.artifact_location_id.0,
            "artifact_verifications.artifact_location_id",
        )?;
        let worker_id = i64_from_u64(input.worker_id.0, "artifact_verifications.worker_id")?;
        let workflow_ticket_id = input
            .workflow_ticket_id
            .map(|id| i64_from_u64(id.0, "artifact_verifications.workflow_ticket_id"))
            .transpose()?;
        let workflow_lease_id = input
            .workflow_lease_id
            .map(|id| i64_from_u64(id.0, "artifact_verifications.workflow_lease_id"))
            .transpose()?;
        let expected_size_bytes = i64_from_u64(
            input.expected_size_bytes,
            "artifact_verifications.expected_size_bytes",
        )?;
        let observed_size_bytes = input
            .observed_size_bytes
            .map(|value| i64_from_u64(value, "artifact_verifications.observed_size_bytes"))
            .transpose()?;
        let res = sqlx::query(
            "INSERT INTO artifact_verifications \
             (artifact_handle_id, artifact_location_id, path, worker_id, \
              workflow_ticket_id, workflow_lease_id, status, \
              expected_size_bytes, expected_checksum, observed_size_bytes, observed_checksum, \
              failure_class, error_code, message, report, started_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(artifact_handle_id)
        .bind(artifact_location_id)
        .bind(&input.path)
        .bind(worker_id)
        .bind(workflow_ticket_id)
        .bind(workflow_lease_id)
        .bind(input.status.as_str())
        .bind(expected_size_bytes)
        .bind(&input.expected_checksum)
        .bind(observed_size_bytes)
        .bind(&input.observed_checksum)
        .bind(&input.failure_class)
        .bind(&input.error_code)
        .bind(&input.message)
        .bind(report)
        .bind(&started_at)
        .bind(&finished_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_verifications insert", e))?;

        Ok(ArtifactVerification {
            id: ArtifactVerificationId(u64_from_i64(
                res.last_insert_rowid(),
                "artifact_verifications.id",
            )?),
            artifact_handle_id: input.artifact_handle_id,
            artifact_location_id: input.artifact_location_id,
            path: input.path,
            worker_id: input.worker_id,
            workflow_ticket_id: input.workflow_ticket_id,
            workflow_lease_id: input.workflow_lease_id,
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

    pub async fn latest_successful_verification_for_live_staging_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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
            .bind(i64_from_u64(
                handle_id.0,
                concat!(module_path!(), ": ", stringify!(handle_id.0)),
            )?)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("artifact_verifications latest", e))?;
        row.as_ref().map(row_to_verification).transpose()
    }

    pub async fn list_verifications(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactVerification>, VoomError> {
        let sql = SELECT_ARTIFACT_VERIFICATION_COLS.to_owned()
            + " \
             FROM artifact_verifications v \
             WHERE v.artifact_handle_id = ? ORDER BY v.id ASC";
        let rows = sqlx::query(&sql)
            .bind(i64_from_u64(
                handle_id.0,
                concat!(module_path!(), ": ", stringify!(handle_id.0)),
            )?)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("artifact_verifications list", e))?;
        rows.iter().map(row_to_verification).collect()
    }

    pub async fn verification_for_workflow_lease(
        &self,
        lease_id: LeaseId,
    ) -> Result<Option<ArtifactVerification>, VoomError> {
        let sql = SELECT_ARTIFACT_VERIFICATION_COLS.to_owned()
            + " FROM artifact_verifications v WHERE v.workflow_lease_id = ?";
        let row = sqlx::query(&sql)
            .bind(i64_from_u64(
                lease_id.0,
                concat!(module_path!(), ": ", stringify!(lease_id.0)),
            )?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| {
                VoomError::database_context("artifact_verifications workflow lease lookup", err)
            })?;
        row.as_ref().map(row_to_verification).transpose()
    }

    pub async fn verifications_for_workflow_job(
        &self,
        job_id: JobId,
    ) -> Result<Vec<ArtifactVerification>, VoomError> {
        let sql = SELECT_ARTIFACT_VERIFICATION_COLS.to_owned()
            + " FROM artifact_verifications v \
               WHERE v.workflow_ticket_id IN (SELECT id FROM tickets WHERE job_id = ?) \
                  OR v.id IN ( \
                      SELECT artifact_verification_id \
                      FROM workflow_file_phase_summaries \
                      WHERE job_id = ? AND artifact_verification_id IS NOT NULL \
                  ) \
               ORDER BY v.id";
        let rows = sqlx::query(&sql)
            .bind(i64_from_u64(
                job_id.0,
                concat!(module_path!(), ": ", stringify!(job_id.0)),
            )?)
            .bind(i64_from_u64(
                job_id.0,
                concat!(module_path!(), ": ", stringify!(job_id.0)),
            )?)
            .fetch_all(&self.pool)
            .await
            .map_err(|err| {
                VoomError::database_context("artifact_verifications workflow job lookup", err)
            })?;
        rows.iter().map(row_to_verification).collect()
    }

    pub async fn create_pending_commit_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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
        .bind(i64_from_u64(
            input.artifact_handle_id.0,
            concat!(module_path!(), ": ", stringify!(input.artifact_handle_id.0)),
        )?)
        .bind(i64_from_u64(
            input.source_file_version_id.0,
            concat!(
                module_path!(),
                ": ",
                stringify!(input.source_file_version_id.0)
            ),
        )?)
        .bind(i64_from_u64(
            input.verification_id.0,
            concat!(module_path!(), ": ", stringify!(input.verification_id.0)),
        )?)
        .bind(&input.target_path)
        .bind(&input.temp_path)
        .bind(report)
        .bind(&started_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| map_commit_insert_err(&e, input.artifact_handle_id, &input.target_path))?;

        Ok(ArtifactCommitRecord {
            id: ArtifactCommitRecordId(u64_from_i64(
                res.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res.last_insert_rowid())),
            )?),
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

    pub async fn mark_commit_committed_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        result_file_version_id: FileVersionId,
        result_file_location_id: FileLocationId,
        promotion_started_at: OffsetDateTime,
        finished_at: OffsetDateTime,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        validate_committed_result(tx, id, result_file_version_id, result_file_location_id).await?;
        let promotion_started_at = iso8601(promotion_started_at)?;
        let finished_at = iso8601(finished_at)?;
        // `recovery_required` is allowed alongside `pending` so the recovery
        // entrypoint can finalize a re-driven commit on the existing record;
        // `committed`/`failed` remain terminal and are still rejected.
        let res = sqlx::query(
            "UPDATE artifact_commit_records \
             SET state = 'committed', result_file_version_id = ?, result_file_location_id = ?, \
                 promotion_started_at = ?, finished_at = ?, \
                 failure_class = NULL, error_code = NULL, message = NULL, recovery_reason = NULL \
             WHERE id = ? AND state IN ('pending', 'recovery_required')",
        )
        .bind(i64_from_u64(
            result_file_version_id.0,
            concat!(module_path!(), ": ", stringify!(result_file_version_id.0)),
        )?)
        .bind(i64_from_u64(
            result_file_location_id.0,
            concat!(module_path!(), ": ", stringify!(result_file_location_id.0)),
        )?)
        .bind(&promotion_started_at)
        .bind(&finished_at)
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records commit", e))?;
        changed_commit_record(tx, id, res.rows_affected(), "commit").await
    }

    pub async fn mark_commit_failed_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records fail", e))?;
        changed_commit_record(tx, id, res.rows_affected(), "fail").await
    }

    pub async fn mark_commit_recovery_required_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        failure: ArtifactCommitFailure,
        recovery_reason: String,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        let finished_at = iso8601(failure.finished_at)?;
        let res = sqlx::query(
            "UPDATE artifact_commit_records \
             SET state = 'recovery_required', failure_class = ?, error_code = ?, message = ?, \
                 recovery_reason = ?, finished_at = ? \
             WHERE id = ? AND state IN ('pending', 'recovery_required')",
        )
        .bind(&failure.failure_class)
        .bind(&failure.error_code)
        .bind(&failure.message)
        .bind(&recovery_reason)
        .bind(&finished_at)
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records recovery_required", e))?;
        changed_commit_record(tx, id, res.rows_affected(), "recovery_required").await
    }

    /// Replace a pending commit report in the caller's transaction.
    pub async fn update_pending_commit_report_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        report: &JsonValue,
    ) -> Result<(), VoomError> {
        let report = serialize_json(report, "artifact_commit_records.report")?;
        let result = sqlx::query(
            "UPDATE artifact_commit_records SET report = ? WHERE id = ? AND state = 'pending'",
        )
        .bind(report)
        .bind(checked_sqlite_id(id.0, "artifact commit id")?)
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            VoomError::database_context("artifact_commit_records report update", error)
        })?;
        if result.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "artifact_commit_records report update: id={id} not pending"
            )));
        }
        Ok(())
    }

    pub async fn get_commit_record(
        &self,
        id: ArtifactCommitRecordId,
    ) -> Result<Option<ArtifactCommitRecord>, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
            + " FROM artifact_commit_records c WHERE c.id = ?";
        let row = sqlx::query(&sql)
            .bind(i64_from_u64(
                id.0,
                concat!(module_path!(), ": ", stringify!(id.0)),
            )?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("artifact_commit_records get", e))?;
        row.as_ref().map(row_to_commit_record).transpose()
    }

    pub async fn list_commit_records(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactCommitRecord>, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
            + " \
             FROM artifact_commit_records c \
             WHERE c.artifact_handle_id = ? ORDER BY c.id ASC";
        let rows = sqlx::query(&sql)
            .bind(i64_from_u64(
                handle_id.0,
                concat!(module_path!(), ": ", stringify!(handle_id.0)),
            )?)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("artifact_commit_records list", e))?;
        rows.iter().map(row_to_commit_record).collect()
    }

    /// `true` when any commit record for `source_file_version_id` is currently
    /// in the recovery-required state — a durable "unrecovered prior mutation"
    /// signal the safety gate consults (ADR 0028). Recovery-required is a live
    /// state, so a later recovered/committed record clears it.
    pub async fn has_recovery_required_for_source_version(
        &self,
        source_file_version_id: FileVersionId,
    ) -> Result<bool, VoomError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM artifact_commit_records \
             WHERE source_file_version_id = ? AND state = 'recovery_required'",
        )
        .bind(i64_from_u64(
            source_file_version_id.0,
            concat!(module_path!(), ": ", stringify!(source_file_version_id.0)),
        )?)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            VoomError::database_context("artifact_commit_records recovery_required count", e)
        })?;
        Ok(count > 0)
    }

    pub async fn record_verified_sidecar_commit_rows_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewSidecarArtifactCommit,
    ) -> Result<SidecarArtifactCommit, VoomError> {
        let pending = validate_sidecar_commit_input(tx, &input).await?;

        let created_at = iso8601(input.observed_at)?;
        let finished_at = iso8601(input.finished_at)?;
        let size_i64 = i64::try_from(input.size_bytes).map_err(|_| {
            VoomError::Config(format!(
                "file_versions: size_bytes {} overflows i64",
                input.size_bytes
            ))
        })?;

        let asset_res = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
            .bind(&created_at)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("file_assets sidecar insert", e))?;
        let file_asset_id = FileAssetId(u64_from_i64(
            asset_res.last_insert_rowid(),
            "file_assets.id",
        )?);

        let version_res = sqlx::query(
            "INSERT INTO file_versions \
             (file_asset_id, content_hash, size_bytes, produced_by, \
              produced_from_version_id, created_at) \
             VALUES (?, ?, ?, 'staged_commit', ?, ?)",
        )
        .bind(i64_from_u64(
            file_asset_id.0,
            "file_versions.file_asset_id",
        )?)
        .bind(&input.content_hash)
        .bind(size_i64)
        .bind(i64_from_u64(
            pending.source_file_version_id.0,
            "file_versions.produced_from_version_id",
        )?)
        .bind(&created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("file_versions sidecar insert", e))?;
        let file_version_id = FileVersionId(u64_from_i64(
            version_res.last_insert_rowid(),
            "file_versions.id",
        )?);

        let location_res = sqlx::query(
            "INSERT INTO file_locations \
             (file_version_id, kind, value, proof_kind, proof_value, observed_at) \
             VALUES (?, 'local_path', ?, NULL, NULL, ?)",
        )
        .bind(i64_from_u64(
            file_version_id.0,
            "file_locations.file_version_id",
        )?)
        .bind(&input.target_path)
        .bind(&created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("file_locations sidecar insert", e))?;
        let file_location_id = FileLocationId(u64_from_i64(
            location_res.last_insert_rowid(),
            "file_locations.id",
        )?);

        let res = sqlx::query(
            "UPDATE artifact_commit_records \
             SET state = 'committed', result_file_version_id = ?, result_file_location_id = ?, \
                 promotion_started_at = NULL, finished_at = ?, failure_class = NULL, \
                 error_code = NULL, message = NULL, recovery_reason = NULL \
             WHERE id = ? AND state IN ('pending', 'recovery_required')",
        )
        .bind(i64_from_u64(
            file_version_id.0,
            "artifact_commit_records.result_file_version_id",
        )?)
        .bind(i64_from_u64(
            file_location_id.0,
            "artifact_commit_records.result_file_location_id",
        )?)
        .bind(&finished_at)
        .bind(i64_from_u64(
            input.commit_record_id.0,
            "artifact_commit_records.id",
        )?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records sidecar commit", e))?;
        let commit_record = changed_commit_record(
            tx,
            input.commit_record_id,
            res.rows_affected(),
            "sidecar_commit",
        )
        .await?;

        Ok(SidecarArtifactCommit {
            commit_record,
            file_asset_id,
            file_version_id,
            file_location_id,
        })
    }
}

async fn validate_verification_location(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &NewArtifactVerification,
) -> Result<(), VoomError> {
    let location_id = i64_from_u64(input.artifact_location_id.0, "artifact_locations.id")?;
    let owner: Option<(i64, String)> =
        sqlx::query_as("SELECT artifact_handle_id, value FROM artifact_locations WHERE id = ?")
            .bind(location_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("artifact_locations owner lookup", e))?;
    let (owner_id, location_value) = owner.ok_or_else(|| {
        VoomError::NotFound(format!(
            "artifact_locations {} missing",
            input.artifact_location_id
        ))
    })?;
    let owner_id = u64_from_i64(owner_id, "artifact_locations.artifact_handle_id")?;
    if owner_id != input.artifact_handle_id.0 {
        return Err(VoomError::Conflict(format!(
            "artifact_verifications: location {} belongs to artifact_handle {}",
            input.artifact_location_id,
            ArtifactHandleId(owner_id)
        )));
    }
    if input.path != location_value {
        return Err(VoomError::Conflict(format!(
            "artifact_verifications: path {:?} does not match artifact_location {} value {:?}",
            input.path, input.artifact_location_id, location_value
        )));
    }
    Ok(())
}

async fn validate_sidecar_commit_input(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &NewSidecarArtifactCommit,
) -> Result<ArtifactCommitRecord, VoomError> {
    let pending = get_active_commit_record_in_tx(tx, input.commit_record_id).await?;
    if pending.target_path != input.target_path {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records sidecar commit: \
             target_path {:?} does not match pending target {:?}",
            input.target_path, pending.target_path
        )));
    }
    if input.target_path.is_empty() {
        return Err(VoomError::Config(
            "artifact_commit_records sidecar commit: target_path is empty".to_owned(),
        ));
    }
    validate_commit_verification(
        tx,
        &NewArtifactCommitRecord {
            artifact_handle_id: pending.artifact_handle_id,
            source_file_version_id: pending.source_file_version_id,
            verification_id: pending.verification_id,
            target_path: pending.target_path.clone(),
            temp_path: pending.temp_path.clone(),
            report: pending.report.clone(),
            started_at: pending.started_at,
        },
    )
    .await?;
    Ok(pending)
}

#[async_trait]
impl ArtifactHandleRepo for SqliteArtifactRepo {
    async fn create_handle_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactHandle,
    ) -> Result<ArtifactHandle, VoomError> {
        SqliteArtifactRepo::create_handle_in_tx(self, tx, input).await
    }

    async fn create_handle(&self, input: NewArtifactHandle) -> Result<ArtifactHandle, VoomError> {
        SqliteArtifactRepo::create_handle(self, input).await
    }

    async fn record_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactLocation,
    ) -> Result<ArtifactLocation, VoomError> {
        SqliteArtifactRepo::record_location_in_tx(self, tx, input).await
    }

    async fn record_location(
        &self,
        input: NewArtifactLocation,
    ) -> Result<ArtifactLocation, VoomError> {
        SqliteArtifactRepo::record_location(self, input).await
    }

    async fn retire_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        location_id: ArtifactLocationId,
        now: OffsetDateTime,
    ) -> Result<ArtifactHandleId, VoomError> {
        SqliteArtifactRepo::retire_location_in_tx(self, tx, location_id, now).await
    }

    async fn retire_location(
        &self,
        location_id: ArtifactLocationId,
        now: OffsetDateTime,
    ) -> Result<ArtifactHandleId, VoomError> {
        SqliteArtifactRepo::retire_location(self, location_id, now).await
    }

    async fn record_lineage_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactLineage,
    ) -> Result<ArtifactLineage, VoomError> {
        SqliteArtifactRepo::record_lineage_in_tx(self, tx, input).await
    }

    async fn record_lineage(
        &self,
        input: NewArtifactLineage,
    ) -> Result<ArtifactLineage, VoomError> {
        SqliteArtifactRepo::record_lineage(self, input).await
    }

    async fn get_handle(&self, id: ArtifactHandleId) -> Result<Option<ArtifactHandle>, VoomError> {
        SqliteArtifactRepo::get_handle(self, id).await
    }

    async fn list_locations_for_handle(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactLocation>, VoomError> {
        SqliteArtifactRepo::list_locations_for_handle(self, handle_id).await
    }
}

#[async_trait]
impl ArtifactVerificationRepo for SqliteArtifactRepo {
    async fn record_verification_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactVerification,
    ) -> Result<ArtifactVerification, VoomError> {
        SqliteArtifactRepo::record_verification_in_tx(self, tx, input).await
    }

    async fn latest_successful_verification_for_live_staging_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        handle_id: ArtifactHandleId,
    ) -> Result<Option<ArtifactVerification>, VoomError> {
        SqliteArtifactRepo::latest_successful_verification_for_live_staging_in_tx(
            self, tx, handle_id,
        )
        .await
    }

    async fn list_verifications(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactVerification>, VoomError> {
        SqliteArtifactRepo::list_verifications(self, handle_id).await
    }

    async fn verification_for_workflow_lease(
        &self,
        lease_id: LeaseId,
    ) -> Result<Option<ArtifactVerification>, VoomError> {
        SqliteArtifactRepo::verification_for_workflow_lease(self, lease_id).await
    }

    async fn verifications_for_workflow_job(
        &self,
        job_id: JobId,
    ) -> Result<Vec<ArtifactVerification>, VoomError> {
        SqliteArtifactRepo::verifications_for_workflow_job(self, job_id).await
    }
}

#[async_trait]
impl ArtifactCommitRepo for SqliteArtifactRepo {
    async fn create_pending_commit_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewArtifactCommitRecord,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        SqliteArtifactRepo::create_pending_commit_in_tx(self, tx, input).await
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
        SqliteArtifactRepo::mark_commit_committed_in_tx(
            self,
            tx,
            id,
            result_file_version_id,
            result_file_location_id,
            promotion_started_at,
            finished_at,
        )
        .await
    }

    async fn mark_commit_failed_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        failure: ArtifactCommitFailure,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        SqliteArtifactRepo::mark_commit_failed_in_tx(self, tx, id, failure).await
    }

    async fn mark_commit_recovery_required_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        failure: ArtifactCommitFailure,
        recovery_reason: String,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        SqliteArtifactRepo::mark_commit_recovery_required_in_tx(
            self,
            tx,
            id,
            failure,
            recovery_reason,
        )
        .await
    }

    async fn get_commit_record(
        &self,
        id: ArtifactCommitRecordId,
    ) -> Result<Option<ArtifactCommitRecord>, VoomError> {
        SqliteArtifactRepo::get_commit_record(self, id).await
    }

    async fn list_commit_records(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactCommitRecord>, VoomError> {
        SqliteArtifactRepo::list_commit_records(self, handle_id).await
    }

    async fn record_verified_sidecar_commit_rows_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewSidecarArtifactCommit,
    ) -> Result<SidecarArtifactCommit, VoomError> {
        SqliteArtifactRepo::record_verified_sidecar_commit_rows_in_tx(self, tx, input).await
    }
}

const SELECT_ARTIFACT_VERIFICATION_COLS: &str = "SELECT v.id, v.artifact_handle_id, \
    v.artifact_location_id, v.path, v.worker_id, v.workflow_ticket_id, v.workflow_lease_id, \
    v.status, v.expected_size_bytes, \
    v.expected_checksum, v.observed_size_bytes, v.observed_checksum, v.failure_class, \
    v.error_code, v.message, v.report, v.started_at, v.finished_at";

const SELECT_ARTIFACT_COMMIT_RECORD_COLS: &str = "SELECT c.id, c.artifact_handle_id, \
    c.source_file_version_id, c.verification_id, c.target_path, c.result_file_version_id, \
    c.result_file_location_id, c.state, c.failure_class, c.error_code, c.message, \
    c.recovery_reason, c.temp_path, c.report, c.started_at, c.promotion_started_at, \
    c.finished_at";

const COMMITTED_TICKET_EVIDENCE_SQL: &str = "WITH ticket_results AS ( \
         SELECT t.id, t.job_id, t.payload, t.result AS raw_result, \
                CASE WHEN json_valid(t.result) THEN t.result ELSE '{}' END AS safe_result \
         FROM tickets t WHERE t.id IN (SELECT value FROM json_each(?)) \
           AND t.state = 'succeeded' AND t.result IS NOT NULL \
     ), expanded_results AS ( \
         SELECT t.id, t.job_id, t.payload, t.raw_result, \
                COALESCE(CAST(output.key AS INTEGER), 0) AS result_ordinal, \
                CASE WHEN json_type(t.safe_result, '$.outputs') = 'array' \
                           AND json_array_length(t.safe_result, '$.outputs') > 0 \
                     THEN json_patch(t.safe_result, output.value) ELSE t.safe_result END AS result \
         FROM ticket_results t LEFT JOIN json_each(t.safe_result, '$.outputs') AS output \
           ON json_type(t.safe_result, '$.outputs') = 'array' \
          AND json_array_length(t.safe_result, '$.outputs') > 0 \
     ) SELECT t.id AS ticket_id, t.job_id AS ticket_job_id, \
              t.payload AS ticket_payload, t.raw_result, t.result, \
              c.id AS commit_id, c.artifact_handle_id AS commit_artifact_handle_id, \
              c.source_file_version_id AS commit_source_file_version_id, \
              c.verification_id AS commit_verification_id, \
              c.result_file_version_id AS commit_result_file_version_id, \
              c.result_file_location_id AS commit_result_file_location_id, \
              c.state AS commit_state, c.report AS commit_report, \
              c.started_at AS commit_started_at, \
              c.promotion_started_at AS commit_promotion_started_at, \
              c.finished_at AS commit_finished_at, \
              v.artifact_handle_id AS verification_artifact_handle_id, \
              v.workflow_ticket_id AS verification_ticket_id, \
              v.workflow_lease_id AS verification_lease_id, \
              v.status AS verification_status, v.report AS verification_report, \
              v.started_at AS verification_started_at, \
              v.finished_at AS verification_finished_at, \
              rl.ticket_id AS result_lease_ticket_id, rl.state AS result_lease_state, \
              rl.acquired_at AS result_lease_acquired_at, \
              rl.expires_at AS result_lease_expires_at, \
              rl.last_heartbeat_at AS result_lease_last_heartbeat_at, \
              rl.released_at AS result_lease_released_at, \
              sfv.file_asset_id AS source_file_asset_id, \
              fv.file_asset_id AS result_file_asset_id, \
              fl.file_version_id AS location_file_version_id, \
              ms.file_version_id AS snapshot_file_version_id \
       FROM expanded_results t \
       LEFT JOIN artifact_commit_records c \
         ON c.id = json_extract(t.result, '$.commit_record_id') \
       LEFT JOIN artifact_verifications v ON v.id = c.verification_id \
       LEFT JOIN leases rl ON rl.id = json_extract(t.result, '$.lease_id') \
       LEFT JOIN file_versions sfv \
         ON sfv.id = json_extract(t.result, '$.source_file_version_id') \
       LEFT JOIN file_versions fv \
         ON fv.id = json_extract(t.result, '$.result_file_version_id') \
       LEFT JOIN file_locations fl \
         ON fl.id = json_extract(t.result, '$.result_file_location_id') \
       LEFT JOIN media_snapshots ms \
         ON ms.id = json_extract(t.result, '$.result_media_snapshot_id') \
       ORDER BY t.id, t.result_ordinal";

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

struct CommitVerificationFacts {
    verification_id: i64,
    verification_handle_id: i64,
    status: String,
    verification_path: String,
    location_kind: String,
    location_value: String,
    retired_at: Option<String>,
    location_handle_id: i64,
    handle_file_version_id: Option<i64>,
    source_retired_at: Option<String>,
    latest_successful_id: Option<i64>,
}

impl From<CommitVerificationRow> for CommitVerificationFacts {
    fn from(row: CommitVerificationRow) -> Self {
        let (
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
        ) = row;
        Self {
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
        }
    }
}

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
    .bind(i64_from_u64(
        input.verification_id.0,
        concat!(module_path!(), ": ", stringify!(input.verification_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("artifact_verifications commit lookup", e))?;
    let Some(row) = row else {
        return Err(VoomError::NotFound(format!(
            "artifact_verifications {} missing",
            input.verification_id
        )));
    };
    let facts = CommitVerificationFacts::from(row);
    if !is_successful_live_staging_verification(&facts, input)? {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records: verification {} is not a successful live staging \
             verification for artifact_handle {}",
            input.verification_id, input.artifact_handle_id
        )));
    }
    let source_file_version_id = i64_from_u64(
        input.source_file_version_id.0,
        "artifact_handles.file_version_id",
    )?;
    if facts.handle_file_version_id != Some(source_file_version_id) {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records: source_file_version_id {} does not match \
             artifact_handle {} file_version_id",
            input.source_file_version_id, input.artifact_handle_id
        )));
    }
    let verification_id = i64_from_u64(input.verification_id.0, "artifact_verifications.id")?;
    if facts.verification_id != verification_id || facts.source_retired_at.is_some() {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records: source_file_version_id {} is not live",
            input.source_file_version_id
        )));
    }
    Ok(())
}

fn is_successful_live_staging_verification(
    facts: &CommitVerificationFacts,
    input: &NewArtifactCommitRecord,
) -> Result<bool, VoomError> {
    Ok(u64_from_i64(
        facts.verification_handle_id,
        "artifact_verifications.artifact_handle_id",
    )? == input.artifact_handle_id.0
        && u64_from_i64(
            facts.location_handle_id,
            "artifact_locations.artifact_handle_id",
        )? == input.artifact_handle_id.0
        && facts.status == ArtifactVerificationStatus::Succeeded.as_str()
        && facts.verification_path == facts.location_value
        && facts.location_kind == "staging"
        && facts.retired_at.is_none()
        && facts.latest_successful_id
            == Some(i64_from_u64(
                input.verification_id.0,
                "artifact_verifications.id",
            )?))
}

async fn validate_committed_result(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: ArtifactCommitRecordId,
    result_file_version_id: FileVersionId,
    result_file_location_id: FileLocationId,
) -> Result<(), VoomError> {
    // Accept `recovery_required` as well as `pending`: the recovery entrypoint
    // finalizes a re-driven commit on the existing (recovery_required) record.
    let pending_row: Option<(i64, String)> = sqlx::query_as(
        "SELECT source_file_version_id, target_path FROM artifact_commit_records \
         WHERE id = ? AND state IN ('pending', 'recovery_required')",
    )
    .bind(i64_from_u64(
        commit_id.0,
        concat!(module_path!(), ": ", stringify!(commit_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("artifact_commit_records pending lookup", e))?;
    let (source_version_id, target_path) = pending_row.ok_or_else(|| {
        VoomError::Conflict(format!(
            "artifact_commit_records commit: id={commit_id} not pending or recovery_required"
        ))
    })?;

    let version_row: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT produced_by, produced_from_version_id, retired_at FROM file_versions WHERE id = ?",
    )
    .bind(i64_from_u64(
        result_file_version_id.0,
        concat!(module_path!(), ": ", stringify!(result_file_version_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("file_versions commit-result lookup", e))?;
    let Some((produced_by, produced_from_version_id, result_retired_at)) = version_row else {
        return Err(VoomError::NotFound(format!(
            "file_versions {result_file_version_id} missing"
        )));
    };
    let source_version_id = u64_from_i64(
        source_version_id,
        "artifact_commit_records.source_file_version_id",
    )?;
    let produced_from_version_id = produced_from_version_id
        .map(|value| u64_from_i64(value, "file_versions.produced_from_version_id"))
        .transpose()?;
    if produced_by != "staged_commit"
        || produced_from_version_id != Some(source_version_id)
        || result_retired_at.is_some()
    {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records commit: result version {result_file_version_id} \
             is not a staged_commit child of source version {}",
            FileVersionId(source_version_id)
        )));
    }

    let location_row: Option<(i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT file_version_id, kind, value, retired_at FROM file_locations WHERE id = ?",
    )
    .bind(i64_from_u64(
        result_file_location_id.0,
        concat!(module_path!(), ": ", stringify!(result_file_location_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("file_locations commit-result lookup", e))?;
    let (location_version_id, location_kind, location_value, retired_at) = location_row
        .ok_or_else(|| {
            VoomError::NotFound(format!("file_locations {result_file_location_id} missing"))
        })?;
    if u64_from_i64(
        location_version_id,
        concat!(module_path!(), ": ", stringify!(location_version_id)),
    )? != result_file_version_id.0
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

async fn get_active_commit_record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactCommitRecordId,
) -> Result<ArtifactCommitRecord, VoomError> {
    let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
        + " FROM artifact_commit_records c \
           WHERE c.id = ? AND c.state IN ('pending', 'recovery_required')";
    let row = sqlx::query(&sql)
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records pending get", e))?;
    row.as_ref()
        .map(row_to_commit_record)
        .transpose()?
        .ok_or_else(|| {
            VoomError::Conflict(format!(
                "artifact_commit_records sidecar commit: id={id} is not active"
            ))
        })
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
        VoomError::database(format!("artifact_commit_records insert: {err}"))
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

fn checked_sqlite_id(value: u64, context: &str) -> Result<i64, VoomError> {
    i64::try_from(value)
        .map_err(|error| VoomError::Internal(format!("{context} exceeds SQLite integer: {error}")))
}

#[derive(Debug)]
struct PolicyFileVersion {
    id: FileVersionId,
    content_hash: String,
    size_bytes: u64,
}

#[derive(Debug)]
struct PolicyFileLocation {
    id: FileLocationId,
    value: String,
}

async fn require_active_policy_file_version(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileVersionId,
) -> Result<PolicyFileVersion, VoomError> {
    let row: Option<(i64, String, i64, i64)> = sqlx::query_as(
        "SELECT v.id, v.content_hash, v.size_bytes, \
                (SELECT MAX(current.id) FROM file_versions current \
                 WHERE current.file_asset_id = v.file_asset_id \
                   AND current.retired_at IS NULL) \
         FROM file_versions v \
         WHERE v.id = ? AND v.retired_at IS NULL",
    )
    .bind(i64_from_u64(
        id.0,
        concat!(module_path!(), ": ", stringify!(id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("policy file version lookup", err))?;
    let Some((row_id, content_hash, size_bytes, active_id)) = row else {
        return Err(VoomError::NotFound(format!(
            "active file_version {id} missing"
        )));
    };
    let row_id = u64_from_i64(row_id, "file_versions.id")?;
    let active_id = u64_from_i64(active_id, "file_versions.active_id")?;
    if row_id != active_id {
        return Err(VoomError::Conflict(format!(
            "file_version {id} was superseded by {}",
            FileVersionId(active_id)
        )));
    }
    Ok(PolicyFileVersion {
        id,
        content_hash,
        size_bytes: u64_from_i64(
            size_bytes,
            concat!(module_path!(), ": ", stringify!(size_bytes)),
        )?,
    })
}

async fn select_policy_file_location(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    version_id: FileVersionId,
    selected_id: Option<FileLocationId>,
) -> Result<PolicyFileLocation, VoomError> {
    let mut rows: Vec<(i64, i64, String, String)> = if let Some(location_id) = selected_id {
        sqlx::query_as(
            "SELECT id, file_version_id, kind, value \
             FROM file_locations \
             WHERE id = ? AND retired_at IS NULL",
        )
        .bind(i64_from_u64(
            location_id.0,
            concat!(module_path!(), ": ", stringify!(location_id.0)),
        )?)
        .fetch_all(&mut **tx)
        .await
        .map_err(|err| VoomError::database_context("policy file location lookup", err))?
    } else {
        sqlx::query_as(
            "SELECT id, file_version_id, kind, value \
             FROM file_locations \
             WHERE file_version_id = ? AND kind = 'local_path' \
               AND retired_at IS NULL ORDER BY id",
        )
        .bind(i64_from_u64(
            version_id.0,
            concat!(module_path!(), ": ", stringify!(version_id.0)),
        )?)
        .fetch_all(&mut **tx)
        .await
        .map_err(|err| VoomError::database_context("policy local path lookup", err))?
    };
    let [row] = rows.as_mut_slice() else {
        return Err(VoomError::Config(format!(
            "file_version {version_id} must have exactly one selected live local_path; found {}",
            rows.len()
        )));
    };
    let location_id = FileLocationId(u64_from_i64(
        row.0,
        concat!(module_path!(), ": ", stringify!(row.0)),
    )?);
    let location_version_id = u64_from_i64(row.1, "file_locations.file_version_id")?;
    if location_version_id != version_id.0 {
        return Err(VoomError::Conflict(format!(
            "file_location {location_id} belongs to file_version {}, not {version_id}",
            FileVersionId(location_version_id)
        )));
    }
    if row.2 != "local_path" {
        return Err(VoomError::Config(format!(
            "file_location {location_id} must be kind local_path"
        )));
    }
    Ok(PolicyFileLocation {
        id: location_id,
        value: std::mem::take(&mut row.3),
    })
}

async fn latest_policy_media_snapshot(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    version_id: FileVersionId,
) -> Result<MediaSnapshotId, VoomError> {
    let id: Option<i64> =
        sqlx::query_scalar("SELECT MAX(id) FROM media_snapshots WHERE file_version_id = ?")
            .bind(i64_from_u64(
                version_id.0,
                concat!(module_path!(), ": ", stringify!(version_id.0)),
            )?)
            .fetch_one(&mut **tx)
            .await
            .map_err(|err| VoomError::database_context("policy media snapshot lookup", err))?;
    id.map(|value| {
        u64_from_i64(value, concat!(module_path!(), ": ", stringify!(value))).map(MediaSnapshotId)
    })
    .transpose()?
    .ok_or_else(|| {
        VoomError::Config(format!(
            "file_version {version_id} has no media snapshot for verification"
        ))
    })
}

async fn policy_committed_handles(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    version_id: FileVersionId,
) -> Result<Vec<ArtifactHandle>, VoomError> {
    let rows = sqlx::query(
        "SELECT h.id, h.file_version_id, h.privacy_class, h.durability_class, \
                h.mutability, h.created_at \
         FROM artifact_commit_records c \
         JOIN artifact_handles h ON h.id = c.artifact_handle_id \
         WHERE c.state = 'committed' AND c.result_file_version_id = ? \
         ORDER BY h.id",
    )
    .bind(i64_from_u64(
        version_id.0,
        concat!(module_path!(), ": ", stringify!(version_id.0)),
    )?)
    .fetch_all(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("policy committed artifact lookup", err))?;
    rows.iter().map(row_to_handle).collect()
}

async fn policy_canonical_handle(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    version_id: FileVersionId,
) -> Result<Option<ArtifactHandle>, VoomError> {
    let rows = sqlx::query(
        "SELECT id, file_version_id, privacy_class, durability_class, mutability, created_at \
         FROM artifact_handles \
         WHERE file_version_id = ? AND durability_class = 'active' \
           AND json_extract(source_lineage, '$.kind') = 'policy_verification' \
         ORDER BY id",
    )
    .bind(i64_from_u64(
        version_id.0,
        concat!(module_path!(), ": ", stringify!(version_id.0)),
    )?)
    .fetch_all(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("policy canonical artifact lookup", err))?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => row_to_handle(row).map(Some),
        _ => Err(VoomError::Conflict(format!(
            "file_version {version_id} has {} canonical verification handles",
            rows.len()
        ))),
    }
}

async fn require_policy_handle_facts(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    handle_id: ArtifactHandleId,
    version: &PolicyFileVersion,
) -> Result<(), VoomError> {
    let file_version_id = require_policy_handle_content_facts(tx, handle_id, version).await?;
    if file_version_id != Some(version.id.0) {
        return Err(policy_handle_facts_conflict(handle_id, version.id));
    }
    Ok(())
}

async fn require_policy_handle_content_facts(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    handle_id: ArtifactHandleId,
    version: &PolicyFileVersion,
) -> Result<Option<u64>, VoomError> {
    let row: Option<(Option<i64>, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT size_bytes, checksum, file_version_id \
         FROM artifact_handles WHERE id = ?",
    )
    .bind(i64_from_u64(
        handle_id.0,
        concat!(module_path!(), ": ", stringify!(handle_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("policy artifact facts lookup", err))?;
    let Some((size_bytes, checksum, file_version_id)) = row else {
        return Err(VoomError::NotFound(format!(
            "artifact_handle {handle_id} missing"
        )));
    };
    if size_bytes
        != Some(i64_from_u64(
            version.size_bytes,
            concat!(module_path!(), ": ", stringify!(version.size_bytes)),
        )?)
        || checksum.as_deref() != Some(version.content_hash.as_str())
    {
        return Err(policy_handle_facts_conflict(handle_id, version.id));
    }
    file_version_id
        .map(|value| u64_from_i64(value, "artifact_handles.file_version_id"))
        .transpose()
}

fn policy_handle_facts_conflict(
    handle_id: ArtifactHandleId,
    version_id: FileVersionId,
) -> VoomError {
    VoomError::Conflict(format!(
        "artifact_handle {handle_id} facts do not match file_version {version_id}"
    ))
}

fn decode_committed_ticket_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<CommittedTicketEvidence, VoomError> {
    let ticket_id = TicketId(evidence_u64(row, "ticket_id")?);
    let ticket_payload = evidence_json(row, "ticket_payload", ticket_id)?;
    let raw_result: String = evidence_value(row, "raw_result")?;
    serde_json::from_str::<JsonValue>(&raw_result).map_err(|error| {
        VoomError::database_context(format!("committed ticket {ticket_id} ticket result"), error)
    })?;
    let result = evidence_json(row, "result", ticket_id)?;
    Ok(CommittedTicketEvidence {
        ticket_id,
        ticket_job_id: evidence_optional_id(row, "ticket_job_id", JobId)?,
        ticket_payload,
        result,
        commit: decode_artifact_commit_evidence(row)?,
        verification: decode_artifact_verification_evidence(row)?,
        result_lease: decode_result_lease_evidence(row)?,
        source_file_asset_id: evidence_optional_id(row, "source_file_asset_id", FileAssetId)?,
        result_file_asset_id: evidence_optional_id(row, "result_file_asset_id", FileAssetId)?,
        location_file_version_id: evidence_optional_id(
            row,
            "location_file_version_id",
            FileVersionId,
        )?,
        snapshot_file_version_id: evidence_optional_id(
            row,
            "snapshot_file_version_id",
            FileVersionId,
        )?,
    })
}

fn decode_artifact_commit_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<Option<ArtifactCommitEvidence>, VoomError> {
    let Some(id) = evidence_optional_id(row, "commit_id", ArtifactCommitRecordId)? else {
        return Ok(None);
    };
    let state: String = evidence_value(row, "commit_state")?;
    Ok(Some(ArtifactCommitEvidence {
        id,
        artifact_handle_id: ArtifactHandleId(evidence_u64(row, "commit_artifact_handle_id")?),
        source_file_version_id: FileVersionId(evidence_u64(row, "commit_source_file_version_id")?),
        verification_id: ArtifactVerificationId(evidence_u64(row, "commit_verification_id")?),
        result_file_version_id: evidence_optional_id(
            row,
            "commit_result_file_version_id",
            FileVersionId,
        )?,
        result_file_location_id: evidence_optional_id(
            row,
            "commit_result_file_location_id",
            FileLocationId,
        )?,
        state: ArtifactCommitState::parse(&state)?,
        report: evidence_json_without_ticket(row, "commit_report")?,
        started_at: evidence_timestamp(row, "commit_started_at")?,
        promotion_started_at: evidence_optional_timestamp(row, "commit_promotion_started_at")?,
        finished_at: evidence_optional_timestamp(row, "commit_finished_at")?,
    }))
}

fn decode_artifact_verification_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<Option<ArtifactVerificationEvidence>, VoomError> {
    let Some(artifact_handle_id) =
        evidence_optional_id(row, "verification_artifact_handle_id", ArtifactHandleId)?
    else {
        return Ok(None);
    };
    let status: String = evidence_value(row, "verification_status")?;
    Ok(Some(ArtifactVerificationEvidence {
        artifact_handle_id,
        workflow_ticket_id: evidence_optional_id(row, "verification_ticket_id", TicketId)?,
        workflow_lease_id: evidence_optional_id(row, "verification_lease_id", LeaseId)?,
        status: ArtifactVerificationStatus::parse(&status)?,
        report: evidence_json_without_ticket(row, "verification_report")?,
        started_at: evidence_timestamp(row, "verification_started_at")?,
        finished_at: evidence_timestamp(row, "verification_finished_at")?,
    }))
}

fn decode_result_lease_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<Option<ResultLeaseEvidence>, VoomError> {
    let Some(ticket_id) = evidence_optional_id(row, "result_lease_ticket_id", TicketId)? else {
        return Ok(None);
    };
    let state: String = evidence_value(row, "result_lease_state")?;
    Ok(Some(ResultLeaseEvidence {
        ticket_id,
        state: LeaseState::parse(&state)?,
        acquired_at: evidence_timestamp(row, "result_lease_acquired_at")?,
        expires_at: evidence_timestamp(row, "result_lease_expires_at")?,
        last_heartbeat_at: evidence_timestamp(row, "result_lease_last_heartbeat_at")?,
        released_at: evidence_optional_timestamp(row, "result_lease_released_at")?,
    }))
}

fn decode_verified_ticket_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<VerifiedTicketEvidence, VoomError> {
    validate_verification_integer_fields(row)?;
    let verification = row_to_verification(row)?;
    validate_verification_lease_ticket(row, &verification)?;
    Ok(VerifiedTicketEvidence {
        verification,
        file_version_id: evidence_optional_id(row, "selected_file_version_id", FileVersionId)?,
        location_value: evidence_value(row, "selected_location_value")?,
    })
}

fn validate_verification_lease_ticket(
    row: &sqlx::sqlite::SqliteRow,
    verification: &ArtifactVerification,
) -> Result<(), VoomError> {
    let lease_ticket_id = evidence_optional_id(row, "selected_lease_ticket_id", TicketId)?;
    match (
        verification.workflow_ticket_id,
        verification.workflow_lease_id,
        lease_ticket_id,
    ) {
        (None, None, None) => Ok(()),
        (Some(ticket_id), Some(_), Some(lease_ticket_id)) if ticket_id == lease_ticket_id => Ok(()),
        _ => Err(VoomError::database(format!(
            "artifact verification {} workflow lease ticket mismatch",
            verification.id
        ))),
    }
}

fn validate_verification_integer_fields(row: &sqlx::sqlite::SqliteRow) -> Result<(), VoomError> {
    for field in [
        "id",
        "artifact_handle_id",
        "artifact_location_id",
        "worker_id",
        "expected_size_bytes",
    ] {
        evidence_u64(row, field)?;
    }
    for field in [
        "workflow_ticket_id",
        "workflow_lease_id",
        "observed_size_bytes",
    ] {
        let value: Option<i64> = evidence_value(row, field)?;
        if let Some(value) = value {
            u64::try_from(value).map_err(|error| {
                VoomError::database_context(format!("workflow evidence {field} is negative"), error)
            })?;
        }
    }
    Ok(())
}

fn evidence_value<T>(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<T, VoomError>
where
    for<'decode> T: sqlx::Decode<'decode, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get(field)
        .map_err(|error| VoomError::database_context(format!("workflow evidence {field}"), error))
}

fn evidence_u64(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<u64, VoomError> {
    let value: i64 = evidence_value(row, field)?;
    u64::try_from(value).map_err(|error| {
        VoomError::database_context(format!("workflow evidence {field} is negative"), error)
    })
}

fn evidence_optional_id<T>(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
    wrap: fn(u64) -> T,
) -> Result<Option<T>, VoomError> {
    let value: Option<i64> = evidence_value(row, field)?;
    value
        .map(|value| {
            u64::try_from(value).map(wrap).map_err(|error| {
                VoomError::database_context(format!("workflow evidence {field} is negative"), error)
            })
        })
        .transpose()
}

fn evidence_json(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
    ticket_id: TicketId,
) -> Result<JsonValue, VoomError> {
    let value: String = evidence_value(row, field)?;
    serde_json::from_str(&value).map_err(|error| {
        VoomError::database_context(
            format!("committed ticket {ticket_id} {field} decode"),
            error,
        )
    })
}

fn evidence_json_without_ticket(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
) -> Result<JsonValue, VoomError> {
    let value: String = evidence_value(row, field)?;
    serde_json::from_str(&value)
        .map_err(|error| VoomError::database_context(format!("workflow evidence {field}"), error))
}

fn evidence_timestamp(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
) -> Result<OffsetDateTime, VoomError> {
    let value: String = evidence_value(row, field)?;
    parse_iso8601(&value)
}

fn evidence_optional_timestamp(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
) -> Result<Option<OffsetDateTime>, VoomError> {
    let value: Option<String> = evidence_value(row, field)?;
    value.as_deref().map(parse_iso8601).transpose()
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
    let workflow_ticket_id: Option<i64> = row
        .try_get("workflow_ticket_id")
        .map_err(|e| map_row_err("artifact_verifications", &e))?;
    let workflow_lease_id: Option<i64> = row
        .try_get("workflow_lease_id")
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
        id: ArtifactVerificationId(u64_from_i64(id, "artifact_verifications.id")?),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(
            artifact_handle_id,
            "artifact_verifications.artifact_handle_id",
        )?),
        artifact_location_id: ArtifactLocationId(u64_from_i64(
            artifact_location_id,
            "artifact_verifications.artifact_location_id",
        )?),
        path,
        worker_id: WorkerId(u64_from_i64(worker_id, "artifact_verifications.worker_id")?),
        workflow_ticket_id: workflow_ticket_id
            .map(|id| u64_from_i64(id, "artifact_verifications.workflow_ticket_id").map(TicketId))
            .transpose()?,
        workflow_lease_id: workflow_lease_id
            .map(|id| u64_from_i64(id, "artifact_verifications.workflow_lease_id").map(LeaseId))
            .transpose()?,
        status: ArtifactVerificationStatus::parse(&status)?,
        expected_size_bytes: u64_from_i64(
            expected_size_bytes,
            "artifact_verifications.expected_size_bytes",
        )?,
        expected_checksum,
        observed_size_bytes: observed_size_bytes
            .map(|value| u64_from_i64(value, "artifact_verifications.observed_size_bytes"))
            .transpose()?,
        observed_checksum,
        failure_class,
        error_code,
        message,
        report: serde_json::from_str(&report)
            .map_err(|e| VoomError::database_context("artifact_verifications report", e))?,
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
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records get", e))?;
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
        id: ArtifactCommitRecordId(u64_from_i64(
            id,
            concat!(module_path!(), ": ", stringify!(id)),
        )?),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(
            artifact_handle_id,
            concat!(module_path!(), ": ", stringify!(artifact_handle_id)),
        )?),
        source_file_version_id: FileVersionId(u64_from_i64(
            source_file_version_id,
            concat!(module_path!(), ": ", stringify!(source_file_version_id)),
        )?),
        verification_id: ArtifactVerificationId(u64_from_i64(
            verification_id,
            concat!(module_path!(), ": ", stringify!(verification_id)),
        )?),
        target_path,
        result_file_version_id: result_file_version_id
            .map(|v| {
                u64_from_i64(v, concat!(module_path!(), ": ", stringify!(v))).map(FileVersionId)
            })
            .transpose()?,
        result_file_location_id: result_file_location_id
            .map(|v| {
                u64_from_i64(v, concat!(module_path!(), ": ", stringify!(v))).map(FileLocationId)
            })
            .transpose()?,
        state: ArtifactCommitState::parse(&state)?,
        failure_class,
        error_code,
        message,
        recovery_reason,
        temp_path,
        report: serde_json::from_str(&report)
            .map_err(|e| VoomError::database_context("artifact_commit_records report", e))?,
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
        id: ArtifactLocationId(u64_from_i64(
            id,
            concat!(module_path!(), ": ", stringify!(id)),
        )?),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(
            handle_id,
            concat!(module_path!(), ": ", stringify!(handle_id)),
        )?),
        kind,
        value,
        observed_at: parse_iso8601(&observed)?,
        retired_at: retired.map(|s| parse_iso8601(&s)).transpose()?,
    })
}

#[cfg(test)]
#[path = "artifacts_test.rs"]
mod tests;
