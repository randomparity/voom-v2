//! Typed payload structs paired with `EventKind` via the `Event` sum type.
//! Sprint 1 M1 subset.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use voom_core::{FailureClass, IssueId};

use crate::kind::EventKind;

// --- system -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaInitializedPayload {
    pub migrations_applied: u32,
    #[serde(with = "time::serde::iso8601")]
    pub schema_init_at: OffsetDateTime,
}

// --- jobs -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOpenedPayload {
    pub job_id: u64,
    pub kind: String,
    pub priority: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSucceededPayload {
    pub job_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFailedPayload {
    pub job_id: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCancelledPayload {
    pub job_id: u64,
    pub reason: String,
}

// --- tickets ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketCreatedPayload {
    pub ticket_id: u64,
    pub job_id: Option<u64>,
    pub kind: String,
    pub priority: i64,
    pub max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketReadyPayload {
    pub ticket_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketLeasedPayload {
    pub ticket_id: u64,
    pub lease_id: u64,
    pub worker_id: u64,
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketSucceededPayload {
    pub ticket_id: u64,
    pub lease_id: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketFailedRetriablePayload {
    pub ticket_id: u64,
    pub attempt: u32,
    pub max_attempts: u32,
    pub reason: String,
    /// Failure category that drove the retriability decision. Audit
    /// reads this back through `EventKind::TicketFailedRetriable` to
    /// reconstruct the decision without re-deriving it from `reason`.
    pub class: FailureClass,
    #[serde(with = "time::serde::iso8601")]
    pub next_eligible_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketFailedTerminalPayload {
    pub ticket_id: u64,
    pub attempt: u32,
    pub max_attempts: u32,
    pub reason: String,
    /// Failure category. M3's auto-open path (§10.2 / S3) reads it back
    /// to populate `issues.severity` / `issues.priority`.
    pub class: FailureClass,
    /// `terminal_failure` issue auto-opened by the §10.2 / S3 path.
    /// `None` in M1 (the `issues` table doesn't exist yet) — `Some(id)`
    /// in M3 once `IssueRepo` lands. Always serialized (`null` in M1)
    /// so the wire shape stays stable across the M3 migration.
    pub issue_id: Option<IssueId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketRequeuedAfterLeaseExpiryPayload {
    pub ticket_id: u64,
    pub lease_id: u64,
}

/// Emitted alongside `lease.force_released` when the operator asked
/// for `also_requeue = true` and the ticket still had attempts
/// remaining. Carries the operator `actor` / `reason` for audit
/// continuity even though `lease.force_released` also records them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketRequeuedAfterForceReleasePayload {
    pub ticket_id: u64,
    pub lease_id: u64,
    pub actor: String,
    pub reason: String,
}

// --- leases (worker-execution) ---------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaseAcquiredPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
    pub worker_id: u64,
    pub ttl_seconds: i64,
    #[serde(with = "time::serde::iso8601")]
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseReleasedPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
    pub release_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseExpiredPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseForceReleasedPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
    pub actor: String,
    pub reason: String,
    pub also_requeue: bool,
}

// --- workers ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRegisteredPayload {
    pub worker_id: u64,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCapabilityRecordedPayload {
    pub worker_id: u64,
    pub capability_id: u64,
    pub operation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerGrantRecordedPayload {
    pub worker_id: u64,
    pub grant_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRetiredPayload {
    pub worker_id: u64,
}

// --- artifacts -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactHandleCreatedPayload {
    pub artifact_handle_id: u64,
    pub privacy_class: String,
    pub durability_class: String,
    pub mutability: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactLocationRecordedPayload {
    pub artifact_location_id: u64,
    pub artifact_handle_id: u64,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactLocationRetiredPayload {
    pub artifact_location_id: u64,
    pub artifact_handle_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactLineageRecordedPayload {
    pub artifact_lineage_id: u64,
    pub parent_artifact_id: u64,
    pub child_artifact_id: u64,
    pub operation: String,
}

// --- media identity --------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaWorkCreatedPayload {
    pub media_work_id: u64,
    pub kind: String,
    pub display_title: String,
    pub provisional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaVariantCreatedPayload {
    pub media_variant_id: u64,
    pub media_work_id: u64,
    pub label: String,
    pub provisional: bool,
}

// --- asset bundles ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBundleCreatedPayload {
    pub bundle_id: u64,
    pub media_variant_id: u64,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBundleMemberAddedPayload {
    pub bundle_id: u64,
    pub file_asset_id: u64,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBundleMemberRemovedPayload {
    pub bundle_id: u64,
    pub file_asset_id: u64,
    pub role: String,
}

// --- file identity ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAssetCreatedPayload {
    pub file_asset_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersionCreatedPayload {
    pub file_version_id: u64,
    pub file_asset_id: u64,
    pub content_hash: String,
    pub size_bytes: u64,
    pub produced_by: String,
    pub produced_from_version_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLocationRecordedPayload {
    pub file_location_id: u64,
    pub file_version_id: u64,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLocationAliasedPayload {
    pub file_location_id: u64,
    pub file_version_id: u64,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileLocationRetiredByMovePayload {
    pub file_location_id: u64,
    pub file_version_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub retired_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileLocationRecordedByMovePayload {
    pub retired_file_location_id: u64,
    pub new_file_location_id: u64,
    pub file_version_id: u64,
    pub kind: String,
    pub value: String,
    #[serde(with = "time::serde::iso8601")]
    pub observed_at: OffsetDateTime,
}

// --- identity evidence -----------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityEvidenceRecordedPayload {
    pub evidence_id: u64,
    pub target_type: String,
    pub target_id: u64,
    pub assertion_type: String,
    pub provider: String,
    pub provider_version: String,
    pub confidence: f64,
    #[serde(with = "time::serde::iso8601")]
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityEvidenceAcceptedPayload {
    pub evidence_id: u64,
    pub target_type: String,
    pub target_id: u64,
    pub accepted_user_id: Option<String>,
    #[serde(with = "time::serde::iso8601")]
    pub accepted_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityEvidenceSupersededPayload {
    pub superseded_evidence_id: u64,
    pub superseded_by_evidence_id: u64,
    pub target_type: String,
    pub target_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub superseded_at: OffsetDateTime,
}

// --- media snapshots -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaSnapshotRecordedPayload {
    pub media_snapshot_id: u64,
    pub file_version_id: u64,
    pub probed_by_worker_id: Option<u64>,
    #[serde(with = "time::serde::iso8601")]
    pub probed_at: OffsetDateTime,
}

// --- sum type --------------------------------------------------------------

/// Sum type pairing each `EventKind` with its typed payload.
/// The compiler prevents writers from emitting a payload that doesn't
/// match the kind.
///
/// The `tag` column uses the dotted wire format produced by
/// `EventKind::as_str()`. Every variant carries an explicit
/// `#[serde(rename = "...")]` matching `as_str()` exactly so the
/// JSON round-trip cannot drift from what the `events.kind` column
/// stores. Do NOT use `rename_all` here — it would produce `snake_case`
/// strings (e.g. `"schema_initialized"`) that don't match the wire
/// format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum Event {
    #[serde(rename = "schema.initialized")]
    SchemaInitialized(SchemaInitializedPayload),
    #[serde(rename = "job.opened")]
    JobOpened(JobOpenedPayload),
    #[serde(rename = "job.succeeded")]
    JobSucceeded(JobSucceededPayload),
    #[serde(rename = "job.failed")]
    JobFailed(JobFailedPayload),
    #[serde(rename = "job.cancelled")]
    JobCancelled(JobCancelledPayload),
    #[serde(rename = "ticket.created")]
    TicketCreated(TicketCreatedPayload),
    #[serde(rename = "ticket.ready")]
    TicketReady(TicketReadyPayload),
    #[serde(rename = "ticket.leased")]
    TicketLeased(TicketLeasedPayload),
    #[serde(rename = "ticket.succeeded")]
    TicketSucceeded(TicketSucceededPayload),
    #[serde(rename = "ticket.failed_retriable")]
    TicketFailedRetriable(TicketFailedRetriablePayload),
    #[serde(rename = "ticket.failed_terminal")]
    TicketFailedTerminal(TicketFailedTerminalPayload),
    #[serde(rename = "ticket.requeued_after_lease_expiry")]
    TicketRequeuedAfterLeaseExpiry(TicketRequeuedAfterLeaseExpiryPayload),
    #[serde(rename = "ticket.requeued_after_force_release")]
    TicketRequeuedAfterForceRelease(TicketRequeuedAfterForceReleasePayload),
    #[serde(rename = "lease.acquired")]
    LeaseAcquired(LeaseAcquiredPayload),
    #[serde(rename = "lease.released")]
    LeaseReleased(LeaseReleasedPayload),
    #[serde(rename = "lease.expired")]
    LeaseExpired(LeaseExpiredPayload),
    #[serde(rename = "lease.force_released")]
    LeaseForceReleased(LeaseForceReleasedPayload),
    #[serde(rename = "worker.registered")]
    WorkerRegistered(WorkerRegisteredPayload),
    #[serde(rename = "worker.capability_recorded")]
    WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload),
    #[serde(rename = "worker.grant_recorded")]
    WorkerGrantRecorded(WorkerGrantRecordedPayload),
    #[serde(rename = "worker.retired")]
    WorkerRetired(WorkerRetiredPayload),
    #[serde(rename = "artifact_handle.created")]
    ArtifactHandleCreated(ArtifactHandleCreatedPayload),
    #[serde(rename = "artifact_location.recorded")]
    ArtifactLocationRecorded(ArtifactLocationRecordedPayload),
    #[serde(rename = "artifact_location.retired")]
    ArtifactLocationRetired(ArtifactLocationRetiredPayload),
    #[serde(rename = "artifact_lineage.recorded")]
    ArtifactLineageRecorded(ArtifactLineageRecordedPayload),
    #[serde(rename = "media_work.created")]
    MediaWorkCreated(MediaWorkCreatedPayload),
    #[serde(rename = "media_variant.created")]
    MediaVariantCreated(MediaVariantCreatedPayload),
    #[serde(rename = "asset_bundle.created")]
    AssetBundleCreated(AssetBundleCreatedPayload),
    #[serde(rename = "asset_bundle.member_added")]
    AssetBundleMemberAdded(AssetBundleMemberAddedPayload),
    #[serde(rename = "asset_bundle.member_removed")]
    AssetBundleMemberRemoved(AssetBundleMemberRemovedPayload),
    #[serde(rename = "file_asset.created")]
    FileAssetCreated(FileAssetCreatedPayload),
    #[serde(rename = "file_version.created")]
    FileVersionCreated(FileVersionCreatedPayload),
    #[serde(rename = "file_location.recorded")]
    FileLocationRecorded(FileLocationRecordedPayload),
    #[serde(rename = "file_location.aliased")]
    FileLocationAliased(FileLocationAliasedPayload),
    #[serde(rename = "file_location.retired_by_move")]
    FileLocationRetiredByMove(FileLocationRetiredByMovePayload),
    #[serde(rename = "file_location.recorded_by_move")]
    FileLocationRecordedByMove(FileLocationRecordedByMovePayload),
    #[serde(rename = "identity_evidence.recorded")]
    IdentityEvidenceRecorded(IdentityEvidenceRecordedPayload),
    #[serde(rename = "identity_evidence.accepted")]
    IdentityEvidenceAccepted(IdentityEvidenceAcceptedPayload),
    #[serde(rename = "identity_evidence.superseded")]
    IdentityEvidenceSuperseded(IdentityEvidenceSupersededPayload),
    #[serde(rename = "media_snapshot.recorded")]
    MediaSnapshotRecorded(MediaSnapshotRecordedPayload),
}

impl Event {
    /// The `EventKind` that pairs with this payload. Derived by exhaustive
    /// match so a new variant is a compile error until both `EventKind` and
    /// the `as_str()` table grow to match.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::SchemaInitialized(_) => EventKind::SchemaInitialized,
            Self::JobOpened(_) => EventKind::JobOpened,
            Self::JobSucceeded(_) => EventKind::JobSucceeded,
            Self::JobFailed(_) => EventKind::JobFailed,
            Self::JobCancelled(_) => EventKind::JobCancelled,
            Self::TicketCreated(_) => EventKind::TicketCreated,
            Self::TicketReady(_) => EventKind::TicketReady,
            Self::TicketLeased(_) => EventKind::TicketLeased,
            Self::TicketSucceeded(_) => EventKind::TicketSucceeded,
            Self::TicketFailedRetriable(_) => EventKind::TicketFailedRetriable,
            Self::TicketFailedTerminal(_) => EventKind::TicketFailedTerminal,
            Self::TicketRequeuedAfterLeaseExpiry(_) => EventKind::TicketRequeuedAfterLeaseExpiry,
            Self::TicketRequeuedAfterForceRelease(_) => EventKind::TicketRequeuedAfterForceRelease,
            Self::LeaseAcquired(_) => EventKind::LeaseAcquired,
            Self::LeaseReleased(_) => EventKind::LeaseReleased,
            Self::LeaseExpired(_) => EventKind::LeaseExpired,
            Self::LeaseForceReleased(_) => EventKind::LeaseForceReleased,
            Self::WorkerRegistered(_) => EventKind::WorkerRegistered,
            Self::WorkerCapabilityRecorded(_) => EventKind::WorkerCapabilityRecorded,
            Self::WorkerGrantRecorded(_) => EventKind::WorkerGrantRecorded,
            Self::WorkerRetired(_) => EventKind::WorkerRetired,
            Self::ArtifactHandleCreated(_) => EventKind::ArtifactHandleCreated,
            Self::ArtifactLocationRecorded(_) => EventKind::ArtifactLocationRecorded,
            Self::ArtifactLocationRetired(_) => EventKind::ArtifactLocationRetired,
            Self::ArtifactLineageRecorded(_) => EventKind::ArtifactLineageRecorded,
            Self::MediaWorkCreated(_) => EventKind::MediaWorkCreated,
            Self::MediaVariantCreated(_) => EventKind::MediaVariantCreated,
            Self::AssetBundleCreated(_) => EventKind::AssetBundleCreated,
            Self::AssetBundleMemberAdded(_) => EventKind::AssetBundleMemberAdded,
            Self::AssetBundleMemberRemoved(_) => EventKind::AssetBundleMemberRemoved,
            Self::FileAssetCreated(_) => EventKind::FileAssetCreated,
            Self::FileVersionCreated(_) => EventKind::FileVersionCreated,
            Self::FileLocationRecorded(_) => EventKind::FileLocationRecorded,
            Self::FileLocationAliased(_) => EventKind::FileLocationAliased,
            Self::FileLocationRetiredByMove(_) => EventKind::FileLocationRetiredByMove,
            Self::FileLocationRecordedByMove(_) => EventKind::FileLocationRecordedByMove,
            Self::IdentityEvidenceRecorded(_) => EventKind::IdentityEvidenceRecorded,
            Self::IdentityEvidenceAccepted(_) => EventKind::IdentityEvidenceAccepted,
            Self::IdentityEvidenceSuperseded(_) => EventKind::IdentityEvidenceSuperseded,
            Self::MediaSnapshotRecorded(_) => EventKind::MediaSnapshotRecorded,
        }
    }
}

#[cfg(test)]
#[path = "payload_test.rs"]
mod tests;
