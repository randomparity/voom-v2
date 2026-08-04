//! Artifact persistence split into semantic repository contracts.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, ErrorCode, FailureClass, FileAssetId, FileLocationId,
    FileVersionId, JobId, LeaseId, MediaSnapshotId, TicketId, VoomError, WorkerId,
};

use super::Repository;
use crate::repo::execution::leases::LeaseState;

/// Access capabilities persisted on an artifact handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactHandleAccessMode {
    LocalPath,
    Read,
    Write,
}

/// Storage location categories accepted by the artifact-location schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactLocationKind {
    LocalPath,
    SharedMount,
    ObjectStore,
    Staging,
    Backup,
}

impl ArtifactLocationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalPath => "local_path",
            Self::SharedMount => "shared_mount",
            Self::ObjectStore => "object_store",
            Self::Staging => "staging",
            Self::Backup => "backup",
        }
    }

    fn parse_database(value: &str) -> Result<Self, VoomError> {
        match value {
            "local_path" => Ok(Self::LocalPath),
            "shared_mount" => Ok(Self::SharedMount),
            "object_store" => Ok(Self::ObjectStore),
            "staging" => Ok(Self::Staging),
            "backup" => Ok(Self::Backup),
            other => Err(VoomError::database(format!(
                "artifact_locations.kind {other:?} not in vocab"
            ))),
        }
    }
}

impl std::fmt::Display for ArtifactLocationKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct NewArtifactHandle {
    pub size_bytes: Option<i64>,
    pub checksum: Option<String>,
    pub privacy_class: String,
    pub durability_class: String,
    pub allowed_access_modes: Vec<ArtifactHandleAccessMode>,
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
    pub allowed_access_modes: Vec<ArtifactHandleAccessMode>,
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
    pub kind: ArtifactLocationKind,
    pub value: String,
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ArtifactLocation {
    pub id: ArtifactLocationId,
    pub artifact_handle_id: ArtifactHandleId,
    pub kind: ArtifactLocationKind,
    pub value: String,
    pub observed_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveArtifactLocation {
    pub id: ArtifactLocationId,
    pub kind: ArtifactLocationKind,
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
    pub failure_class: Option<FailureClass>,
    pub error_code: Option<ErrorCode>,
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
    pub failure_class: Option<FailureClass>,
    pub error_code: Option<ErrorCode>,
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
    pub failure_class: Option<FailureClass>,
    pub error_code: Option<ErrorCode>,
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
    pub failure_class: FailureClass,
    pub error_code: ErrorCode,
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

mod commits;
mod evidence;
mod handles;
mod verification;

pub(super) fn parse_failure_class(value: &str, field: &str) -> Result<FailureClass, VoomError> {
    FailureClass::from_wire_str(value)
        .ok_or_else(|| VoomError::database(format!("{field} {value:?} not in vocab")))
}

pub(super) fn parse_error_code(value: &str, field: &str) -> Result<ErrorCode, VoomError> {
    ErrorCode::from_wire_str(value)
        .ok_or_else(|| VoomError::database(format!("{field} {value:?} not in vocab")))
}

pub(super) fn checked_sqlite_id(value: u64, context: &str) -> Result<i64, VoomError> {
    i64::try_from(value)
        .map_err(|error| VoomError::Internal(format!("{context} exceeds SQLite integer: {error}")))
}

#[cfg(test)]
mod tests;
