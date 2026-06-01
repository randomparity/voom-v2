//! Typed payload structs paired with `EventKind` via the `Event` sum type.
//! Sprint 1 M1 subset.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use voom_core::{
    CommitId, EvidenceId, FailureClass, IssueId, NodeKind, NodeStatus, PolicyVersionId,
    TicketOperation, UseLeaseId, WorkerKind,
};

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
    pub kind: TicketOperation,
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
    /// in M3 once `SqliteIssueRepo` lands. Always serialized (`null` in M1)
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

// --- nodes ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRegisteredPayload {
    pub node_id: u64,
    pub name: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub heartbeat_ttl_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeHeartbeatRecordedPayload {
    pub node_id: u64,
    pub status: NodeStatus,
    #[serde(with = "time::serde::iso8601")]
    pub last_seen_at: OffsetDateTime,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeMarkedStalePayload {
    pub node_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub marked_stale_at: OffsetDateTime,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRetiredPayload {
    pub node_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub retired_at: OffsetDateTime,
    pub epoch: u64,
}

// --- workers ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRegisteredPayload {
    pub worker_id: u64,
    pub name: String,
    pub kind: WorkerKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerLinkedToNodePayload {
    pub worker_id: u64,
    pub node_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCapabilityRecordedPayload {
    pub worker_id: u64,
    pub capability_id: u64,
    pub operation: TicketOperation,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactStagedPayload {
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub source_file_version_id: u64,
    pub source_file_location_id: Option<u64>,
    pub staging_path: String,
    pub size_bytes: u64,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVerificationStartedPayload {
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub worker_id: u64,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVerificationSucceededPayload {
    pub verification_id: u64,
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub worker_id: u64,
    pub observed_size_bytes: u64,
    pub observed_checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVerificationFailedPayload {
    pub verification_id: u64,
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub worker_id: u64,
    pub error_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCommitStartedPayload {
    pub commit_record_id: u64,
    pub artifact_handle_id: u64,
    pub source_file_version_id: u64,
    pub verification_id: u64,
    pub target_path: String,
    pub temp_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCommitCompletedPayload {
    pub commit_record_id: u64,
    pub artifact_handle_id: u64,
    pub result_file_version_id: u64,
    pub result_file_location_id: u64,
    pub target_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCommitFailedPreMutationPayload {
    pub artifact_handle_id: u64,
    pub commit_record_id: Option<u64>,
    pub target_path: String,
    pub error_code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCommitRecoveryRequiredPayload {
    pub commit_record_id: u64,
    pub artifact_handle_id: u64,
    pub target_path: String,
    pub temp_path: String,
    pub recovery_reason: String,
    pub error_code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactTranscodeStartedPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub staging_path: String,
    #[serde(default)]
    pub profile_name: String,
    #[serde(default)]
    pub encoder: String,
    #[serde(default)]
    pub target_codec: String,
    #[serde(default)]
    pub output_container: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactTranscodeProgressPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub staging_path: String,
    #[serde(default)]
    pub profile_name: String,
    #[serde(default)]
    pub encoder: String,
    #[serde(default)]
    pub target_codec: String,
    #[serde(default)]
    pub output_container: String,
    pub percent_bps: Option<u16>,
    pub message: Option<String>,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactTranscodeSucceededPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub staging_path: String,
    #[serde(default)]
    pub profile_name: String,
    #[serde(default)]
    pub encoder: String,
    #[serde(default)]
    pub target_codec: String,
    pub output_container: String,
    pub output_video_codec: String,
    #[serde(default)]
    pub copied_video: bool,
    #[serde(default)]
    pub output_width: u32,
    #[serde(default)]
    pub output_height: u32,
    #[serde(default)]
    pub output_pixel_format: String,
    pub provider: String,
    pub provider_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactTranscodeFailedPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: Option<u64>,
    pub staging_path: Option<String>,
    #[serde(default)]
    pub profile_name: String,
    #[serde(default)]
    pub encoder: String,
    #[serde(default)]
    pub target_codec: String,
    #[serde(default)]
    pub output_container: String,
    pub failure_class: FailureClass,
    pub error_code: String,
    pub message: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRemuxStreamPayload {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRemuxStartedPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactRemuxStreamPayload>,
    pub default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub clear_default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub track_order: Vec<String>,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRemuxProgressPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactRemuxStreamPayload>,
    pub default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub clear_default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub percent_bps: Option<u16>,
    pub message: Option<String>,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRemuxSucceededPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactRemuxStreamPayload>,
    pub default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub clear_default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub kept_snapshot_stream_ids: Vec<String>,
    pub default_snapshot_stream_ids: Vec<String>,
    pub output_container: String,
    pub provider: String,
    pub provider_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRemuxFailedPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: Option<u64>,
    pub artifact_handle_id: Option<u64>,
    pub artifact_location_id: Option<u64>,
    pub staging_path: Option<String>,
    pub selected_streams: Vec<ArtifactRemuxStreamPayload>,
    pub default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub clear_default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub failure_class: FailureClass,
    pub error_code: String,
    pub message: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioStreamPayload {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioDispositionPayload {
    pub default: Option<bool>,
    pub forced: Option<bool>,
    pub commentary: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioOutputStreamPayload {
    pub snapshot_stream_id: String,
    pub output_provider_stream_index: u32,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: Option<bool>,
    pub disposition: Option<ArtifactAudioDispositionPayload>,
    pub channels: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioTranscodeStartedPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub source_media_snapshot_id: u64,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactAudioStreamPayload>,
    pub target_codec: String,
    pub output_container: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioTranscodeProgressPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub source_media_snapshot_id: u64,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactAudioStreamPayload>,
    pub percent_bps: Option<u16>,
    pub message: Option<String>,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioTranscodeSucceededPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub source_media_snapshot_id: u64,
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactAudioStreamPayload>,
    pub selected_snapshot_stream_ids: Vec<String>,
    pub selected_output_streams: Vec<ArtifactAudioOutputStreamPayload>,
    pub output_container: String,
    pub output_audio_codecs: Vec<String>,
    pub provider: String,
    pub provider_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioTranscodeFailedPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: Option<u64>,
    pub source_media_snapshot_id: Option<u64>,
    pub artifact_handle_id: Option<u64>,
    pub artifact_location_id: Option<u64>,
    pub staging_path: Option<String>,
    pub selected_streams: Vec<ArtifactAudioStreamPayload>,
    pub selected_output_streams: Vec<ArtifactAudioOutputStreamPayload>,
    pub failure_class: FailureClass,
    pub error_code: String,
    pub message: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractStartedPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub source_media_snapshot_id: u64,
    pub source_bundle_id: u64,
    pub staging_path: String,
    pub selected_stream: ArtifactAudioStreamPayload,
    pub role: String,
    pub target_codec: String,
    pub output_container: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractProgressPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub source_media_snapshot_id: u64,
    pub source_bundle_id: u64,
    pub staging_path: String,
    pub selected_stream: ArtifactAudioStreamPayload,
    pub percent_bps: Option<u16>,
    pub message: Option<String>,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractSucceededPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: u64,
    pub source_media_snapshot_id: u64,
    pub source_bundle_id: u64,
    pub artifact_handle_id: u64,
    pub artifact_location_id: u64,
    pub staging_path: String,
    pub selected_stream: ArtifactAudioStreamPayload,
    pub selected_snapshot_stream_id: String,
    pub role: String,
    pub output_container: String,
    pub output_audio_codec: String,
    pub provider: String,
    pub provider_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractFailedPayload {
    pub job_id: u64,
    pub ticket_id: u64,
    pub lease_id: Option<u64>,
    pub source_file_version_id: u64,
    pub source_file_location_id: Option<u64>,
    pub source_media_snapshot_id: Option<u64>,
    pub source_bundle_id: u64,
    pub artifact_handle_id: Option<u64>,
    pub artifact_location_id: Option<u64>,
    pub staging_path: Option<String>,
    pub selected_stream: Option<ArtifactAudioStreamPayload>,
    pub role: Option<String>,
    pub failure_class: FailureClass,
    pub error_code: String,
    pub message: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

// --- issues ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueLifecyclePayload {
    pub issue_id: IssueId,
    pub kind: String,
    pub status: String,
    pub dedupe_key: Option<String>,
    pub policy_version_id: Option<PolicyVersionId>,
    pub report_id: Option<String>,
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

// --- M3 — asset use leases (Phase 1) -----------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseLeaseAcquiredPayload {
    pub lease_id: u64,
    /// One of: `"playback" | "scan" | "copy" | "manual_lock" | "external_lock" | "worker_operation"`.
    pub kind: String,
    /// One of: `"asset" | "bundle" | "version" | "location"`.
    pub scope_type: String,
    pub scope_id: u64,
    /// One of: `"user" | "control_plane" | "worker" | "external_system"`.
    pub issuer_kind: String,
    pub issuer_ref: String,
    /// One of: `"blocking" | "advisory"`.
    pub blocking_mode: String,
    pub ttl_bound: bool,
    #[serde(with = "time::serde::iso8601")]
    pub acquired_at: OffsetDateTime,
    #[serde(default, with = "time::serde::iso8601::option")]
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseLeaseReleasedPayload {
    pub lease_id: u64,
    /// One of: `"released" | "superseded"` (the issuer-driven release reasons).
    /// `expired`, `force_released`, and `issuer_lost` are emitted by their
    /// dedicated event variants.
    pub release_reason: String,
    #[serde(with = "time::serde::iso8601")]
    pub released_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseLeaseExpiredPayload {
    pub lease_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub released_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseLeaseForceReleasedPayload {
    pub lease_id: u64,
    pub actor: String,
    pub reason: String,
    #[serde(with = "time::serde::iso8601")]
    pub released_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseLeaseRecoveredStaleIssuerPayload {
    pub lease_id: u64,
    pub actor: String,
    pub reason: String,
    #[serde(with = "time::serde::iso8601")]
    pub released_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseLeaseReanchoredByMovePayload {
    pub lease_id: u64,
    pub retired_location_id: u64,
    pub new_location_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub reanchored_at: OffsetDateTime,
}

// --- M3 Phase 2 — commit safety gate (Phase A subset) -----------------------
//
// Sprint 1 §9.3 destructive-commit gate. Phase A emits one of four events
// per `prepare_destructive_commit` call: `commit.intent_recorded` on the
// success path (a `commit_intents` row landed in `state = 'pending'`),
// and one of three abort kinds for the matching Phase A `Blocked*` exits.
// Phases B / C land in later commits with their own dedicated event kinds.

/// `commit.intent_recorded` — Phase A success. The row is in
/// `state = 'pending'` and the gate's closure walk has been persisted
/// alongside the target. Carries the granularity-bucketed member counts
/// so an audit reader can size the closure without re-deserializing the
/// JSON column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitIntentRecordedPayload {
    pub commit_id: CommitId,
    /// Wire-format tag identifying the `CommitTarget` variant (one of
    /// `"delete_file_location"`, `"replace_file_location"`,
    /// `"move_file_location"`). Carried separately from the durable
    /// `target` JSON column so audit readers can filter without parsing.
    pub target_kind: String,
    pub closure_asset_count: u32,
    pub closure_bundle_count: u32,
    pub closure_version_count: u32,
    pub closure_location_count: u32,
    pub accepted_evidence_count: u32,
    #[serde(with = "time::serde::iso8601")]
    pub started_at: OffsetDateTime,
}

/// `commit.aborted_by_use_lease` — Phase A or Phase B trip-wire: a
/// blocking use-lease overlapped the closure. The phase tag distinguishes
/// the two emission points (Phase A is the two-tx pattern; Phase B
/// commits in-tx — both pin the same payload shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByUseLeasePayload {
    pub commit_id: CommitId,
    pub lease_id: UseLeaseId,
    /// One of `"asset" | "bundle" | "version" | "location"` —
    /// mirrors `LeaseScope::type_str`.
    pub lease_scope_type: String,
    pub lease_scope_id: u64,
    /// Which gate phase fired this abort. `"prepare"` for Phase A;
    /// `"authorize"` for Phase B (Phase B emission lands in commit 6).
    pub phase: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.aborted_by_stale_evidence` — Phase A or Phase B trip-wire: at
/// least one accepted-evidence pin no longer matches current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByStaleEvidencePayload {
    pub commit_id: CommitId,
    pub evidence_id: EvidenceId,
    /// One of `"pinned_file_version_retired" | "pinned_hash_differs" |
    /// "pinned_location_retired"` — mirrors the `EvidenceDrift` enum
    /// variants (`snake_case`).
    pub drift_kind: String,
    pub phase: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.aborted_by_closure_incomplete` — the closure walker could not
/// enumerate every required member (alias-resolver `Unreachable` in
/// Sprint 1). `message` carries the resolver's diagnostic so an operator
/// can act on the failed mount / object store / FS endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByClosureIncompletePayload {
    pub commit_id: CommitId,
    pub phase: String,
    pub message: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.aborted_by_pending_commit` — Phase A trip-wire (round-7).
/// Another in-flight `commit_intents` row (`state IN ('pending',
/// 'authorized')`) already covers a scope in the new commit's
/// `closure_initial`. Carries the offending scope (`scope_type`,
/// `scope_id`) so an operator can route the wait / takeover decision
/// without a race-prone re-query. `pending_commit_id` identifies the
/// existing in-flight row that won the lock; `commit_id` is the newly
/// landed `aborted` row that recorded the abort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByPendingCommitPayload {
    pub commit_id: CommitId,
    /// ID of the in-flight commit that already covers `scope_*`.
    pub pending_commit_id: CommitId,
    /// One of `"asset" | "bundle" | "version" | "location"` — mirrors
    /// `LeaseScope::type_str`.
    pub scope_type: String,
    pub scope_id: u64,
    /// `"prepare"` — only Phase A emits this event (Phase B / C cannot
    /// reach the overlap branch; they operate on a single committed
    /// intent row).
    pub phase: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.authorized` — Phase B success. The intent transitioned from
/// `pending` to `authorized`; the gate's recomputed `closure_authorized`
/// + per-member epoch snapshot are durably persisted on the row.
///
/// Carries the granularity-bucketed member counts so an audit reader
/// can size the authorized closure without re-deserializing the JSON
/// column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAuthorizedPayload {
    pub commit_id: CommitId,
    pub closure_asset_count: u32,
    pub closure_bundle_count: u32,
    pub closure_version_count: u32,
    pub closure_location_count: u32,
    /// Number of `[kind, row_id, epoch]` triples written to the
    /// `commit_intents.target_row_epochs` JSON column.
    pub target_row_epoch_count: u32,
    #[serde(with = "time::serde::iso8601")]
    pub authorized_at: OffsetDateTime,
}

/// `commit.aborted_by_closure_grew` — Phase B trip-wire: the closure
/// walker found a non-empty `ClosureMemberDelta` between Phase A
/// (`closure_initial`) and Phase B (`closure_authorized`). Carries the
/// per-granularity add/remove counts so an audit reader can characterize
/// the drift without re-deserializing the closure JSON columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByClosureGrewPayload {
    pub commit_id: CommitId,
    pub added_asset_count: u32,
    pub added_bundle_count: u32,
    pub added_version_count: u32,
    pub added_location_count: u32,
    pub removed_asset_count: u32,
    pub removed_bundle_count: u32,
    pub removed_version_count: u32,
    pub removed_location_count: u32,
    pub phase: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

// --- M3 Phase 2 — commit safety gate (Phase C) ------------------------------
//
// Sprint 1 §9.3.2 Phase C. The four payload shapes below correspond to
// `finalize_destructive_commit`'s exit branches:
//   - `commit.completed` — silent dispatch fired, durable mutation landed.
//   - `commit.aborted_pre_mutation` — `MutationOutcome::NotPerformed`
//     (`prior_state='authorized'`) or `abort_destructive_commit`
//     (`prior_state='pending'`).
//   - `commit.aborted_post_mutation` — Phase C defensive trip-wire
//     (`closure_grew` | `fresh_lease` | `closure_grew_and_fresh_lease` |
//     `stale_target_epoch`).
//   - `commit.recovery_required` — emitted alongside the
//     `aborted_post_mutation` payload to flag the durable row for the
//     Sprint 5+ recovery worker. Mirrors the trip-wire fields so the
//     recovery worker can decode the reason from a single row.

/// One drifted target row from the Phase C `stale_target_epoch`
/// trip-wire. Wire-format mirror of the in-memory `TargetEpochDrift`
/// struct that lives in `voom-store` so the on-disk JSON shape is the
/// single source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetEpochDriftWire {
    /// One of `"file_asset" | "file_version" | "file_location" |
    /// "bundle"` — mirrors `TargetMemberKind`'s `snake_case` serde tag.
    pub kind: String,
    pub id: u64,
    pub expected: u64,
    pub observed: u64,
}

/// `commit.completed` — Phase C success. The durable identity mutation
/// has been applied to the matching `IdentityRepo` in the same tx the
/// `commit_intents` row transitioned to `completed`. Carries the
/// granularity-bucketed member counts of `closure_final` so an audit
/// reader can size the silent-path closure without re-deserializing
/// the JSON column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitCompletedPayload {
    pub commit_id: CommitId,
    /// Wire-format tag identifying the `CommitTarget` variant the gate
    /// dispatched (one of `"delete_file_location"`,
    /// `"replace_file_location"`, `"move_file_location"`).
    pub target_kind: String,
    pub closure_asset_count: u32,
    pub closure_bundle_count: u32,
    pub closure_version_count: u32,
    pub closure_location_count: u32,
    #[serde(with = "time::serde::iso8601")]
    pub finalized_at: OffsetDateTime,
}

/// `commit.aborted_pre_mutation` — emitted when a `commit_intents` row
/// is durably transitioned to `aborted` BEFORE any filesystem mutation
/// applied. Two emission sites: `abort_destructive_commit`
/// (`prior_state='pending'`) and `finalize_destructive_commit` called
/// with `MutationOutcome::NotPerformed` (`prior_state='authorized'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedPreMutationPayload {
    pub commit_id: CommitId,
    /// One of `"pending" | "authorized"` — the durable state the row
    /// was in immediately before this transition. Distinguishes
    /// "operator aborted before authorize" from "operator obtained a
    /// permit and chose not to mutate".
    pub prior_state: String,
    /// `AbortReason` `snake_case` tag — one of `"operator_cancel" |
    /// "mutation_failed" | "other"` in Sprint 1 (the other variants
    /// are reserved for gate-driven aborts that route through their
    /// dedicated event kinds).
    pub reason: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.aborted_post_mutation` — Phase C defensive trip-wire.
/// Sprint spec §9.3.2 unified schema: carries the closure delta
/// (vs. `closure_authorized`), the fresh-lease IDs, and (when the
/// `stale_target_epoch` trip-wire fires) the drifted target-row
/// triples. Empty arrays for dimensions that did not fire. The
/// `reason` tag names the dominant trip-wire so audit/recovery tools
/// can route without re-deriving from the array shapes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedPostMutationPayload {
    pub commit_id: CommitId,
    /// One of `"closure_grew" | "fresh_lease" |
    /// "closure_grew_and_fresh_lease" | "stale_target_epoch"`. Single
    /// source of truth for the trip-wire signal; the durable row's
    /// `recovery_reason` column carries the same value.
    pub reason: String,
    pub added_asset_count: u32,
    pub added_bundle_count: u32,
    pub added_version_count: u32,
    pub added_location_count: u32,
    pub removed_asset_count: u32,
    pub removed_bundle_count: u32,
    pub removed_version_count: u32,
    pub removed_location_count: u32,
    /// `UseLeaseId.0` values for every fresh blocking lease the Phase C
    /// recheck saw against `closure_final`. Possibly empty.
    pub fresh_lease_ids: Vec<u64>,
    /// Drifted `(kind, id, expected, observed)` triples from the
    /// `stale_target_epoch` recheck. Possibly empty (only populated
    /// when `reason='stale_target_epoch'`).
    pub target_epoch_drift: Vec<TargetEpochDriftWire>,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.forced_override` — emitted by `prepare_destructive_commit`
/// when the caller threads a non-`None` `ForcePathToken` through
/// `DestructiveCommit.override_token`. Recorded once at prepare time,
/// atomically with the `commit.intent_recorded` insert /
/// `commit_intents.override_token` column write. Authorize does not
/// re-emit — the audit signal is single-shot per commit.
///
/// `bypass` is the canonical `snake_case` rendering of every
/// `BypassKind` bit in the token's set (sorted ascending; the on-disk
/// `BTreeSet` ordering carries over directly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitForcedOverridePayload {
    pub commit_id: CommitId,
    pub actor: String,
    pub reason: String,
    /// `snake_case` tags for every `BypassKind` bit set on the token —
    /// `"closure_incomplete"` in Sprint 1; the array shape leaves room
    /// for future bypass kinds without a payload schema change.
    pub bypass: Vec<String>,
    #[serde(with = "time::serde::iso8601")]
    pub recorded_at: OffsetDateTime,
}

/// `commit.recovery_required` — emitted alongside
/// `commit.aborted_post_mutation` to flag the durable row for the
/// Sprint 5+ recovery worker. Mirrors the trip-wire payload's
/// `reason` / drift fields so the recovery worker can decode the
/// signal from a single row without joining back to the
/// post-mutation event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRecoveryRequiredPayload {
    pub commit_id: CommitId,
    /// Mirror of `commit_intents.recovery_reason`. Same vocabulary as
    /// `CommitAbortedPostMutationPayload.reason`.
    pub recovery_reason: String,
    pub added_asset_count: u32,
    pub added_bundle_count: u32,
    pub added_version_count: u32,
    pub added_location_count: u32,
    pub removed_asset_count: u32,
    pub removed_bundle_count: u32,
    pub removed_version_count: u32,
    pub removed_location_count: u32,
    pub fresh_lease_ids: Vec<u64>,
    pub target_epoch_drift: Vec<TargetEpochDriftWire>,
    #[serde(with = "time::serde::iso8601")]
    pub recorded_at: OffsetDateTime,
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
    #[serde(rename = "node.registered")]
    NodeRegistered(NodeRegisteredPayload),
    #[serde(rename = "node.heartbeat_recorded")]
    NodeHeartbeatRecorded(NodeHeartbeatRecordedPayload),
    #[serde(rename = "node.marked_stale")]
    NodeMarkedStale(NodeMarkedStalePayload),
    #[serde(rename = "node.retired")]
    NodeRetired(NodeRetiredPayload),
    #[serde(rename = "worker.registered")]
    WorkerRegistered(WorkerRegisteredPayload),
    #[serde(rename = "worker.linked_to_node")]
    WorkerLinkedToNode(WorkerLinkedToNodePayload),
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
    #[serde(rename = "artifact.staged")]
    ArtifactStaged(ArtifactStagedPayload),
    #[serde(rename = "artifact.verification_started")]
    ArtifactVerificationStarted(ArtifactVerificationStartedPayload),
    #[serde(rename = "artifact.verification_succeeded")]
    ArtifactVerificationSucceeded(ArtifactVerificationSucceededPayload),
    #[serde(rename = "artifact.verification_failed")]
    ArtifactVerificationFailed(ArtifactVerificationFailedPayload),
    #[serde(rename = "artifact.commit_started")]
    ArtifactCommitStarted(ArtifactCommitStartedPayload),
    #[serde(rename = "artifact.commit_completed")]
    ArtifactCommitCompleted(ArtifactCommitCompletedPayload),
    #[serde(rename = "artifact.commit_failed_pre_mutation")]
    ArtifactCommitFailedPreMutation(ArtifactCommitFailedPreMutationPayload),
    #[serde(rename = "artifact.commit_recovery_required")]
    ArtifactCommitRecoveryRequired(ArtifactCommitRecoveryRequiredPayload),
    #[serde(rename = "artifact.transcode_started")]
    ArtifactTranscodeStarted(ArtifactTranscodeStartedPayload),
    #[serde(rename = "artifact.transcode_progress")]
    ArtifactTranscodeProgress(ArtifactTranscodeProgressPayload),
    #[serde(rename = "artifact.transcode_succeeded")]
    ArtifactTranscodeSucceeded(ArtifactTranscodeSucceededPayload),
    #[serde(rename = "artifact.transcode_failed")]
    ArtifactTranscodeFailed(ArtifactTranscodeFailedPayload),
    #[serde(rename = "artifact.remux_started")]
    ArtifactRemuxStarted(ArtifactRemuxStartedPayload),
    #[serde(rename = "artifact.remux_progress")]
    ArtifactRemuxProgress(ArtifactRemuxProgressPayload),
    #[serde(rename = "artifact.remux_succeeded")]
    ArtifactRemuxSucceeded(ArtifactRemuxSucceededPayload),
    #[serde(rename = "artifact.remux_failed")]
    ArtifactRemuxFailed(ArtifactRemuxFailedPayload),
    #[serde(rename = "artifact.audio_transcode_started")]
    ArtifactAudioTranscodeStarted(ArtifactAudioTranscodeStartedPayload),
    #[serde(rename = "artifact.audio_transcode_progress")]
    ArtifactAudioTranscodeProgress(ArtifactAudioTranscodeProgressPayload),
    #[serde(rename = "artifact.audio_transcode_succeeded")]
    ArtifactAudioTranscodeSucceeded(ArtifactAudioTranscodeSucceededPayload),
    #[serde(rename = "artifact.audio_transcode_failed")]
    ArtifactAudioTranscodeFailed(ArtifactAudioTranscodeFailedPayload),
    #[serde(rename = "artifact.audio_extract_started")]
    ArtifactAudioExtractStarted(ArtifactAudioExtractStartedPayload),
    #[serde(rename = "artifact.audio_extract_progress")]
    ArtifactAudioExtractProgress(ArtifactAudioExtractProgressPayload),
    #[serde(rename = "artifact.audio_extract_succeeded")]
    ArtifactAudioExtractSucceeded(ArtifactAudioExtractSucceededPayload),
    #[serde(rename = "artifact.audio_extract_failed")]
    ArtifactAudioExtractFailed(ArtifactAudioExtractFailedPayload),
    #[serde(rename = "issue.opened")]
    IssueOpened(IssueLifecyclePayload),
    #[serde(rename = "issue.updated")]
    IssueUpdated(IssueLifecyclePayload),
    #[serde(rename = "issue.resolved")]
    IssueResolved(IssueLifecyclePayload),
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
    #[serde(rename = "use_lease.acquired")]
    UseLeaseAcquired(UseLeaseAcquiredPayload),
    #[serde(rename = "use_lease.released")]
    UseLeaseReleased(UseLeaseReleasedPayload),
    #[serde(rename = "use_lease.expired")]
    UseLeaseExpired(UseLeaseExpiredPayload),
    #[serde(rename = "use_lease.force_released")]
    UseLeaseForceReleased(UseLeaseForceReleasedPayload),
    #[serde(rename = "use_lease.recovered_stale_issuer")]
    UseLeaseRecoveredStaleIssuer(UseLeaseRecoveredStaleIssuerPayload),
    #[serde(rename = "use_lease.reanchored_by_move")]
    UseLeaseReanchoredByMove(UseLeaseReanchoredByMovePayload),
    #[serde(rename = "commit.intent_recorded")]
    CommitIntentRecorded(CommitIntentRecordedPayload),
    #[serde(rename = "commit.aborted_by_use_lease")]
    CommitAbortedByUseLease(CommitAbortedByUseLeasePayload),
    #[serde(rename = "commit.aborted_by_stale_evidence")]
    CommitAbortedByStaleEvidence(CommitAbortedByStaleEvidencePayload),
    #[serde(rename = "commit.aborted_by_closure_incomplete")]
    CommitAbortedByClosureIncomplete(CommitAbortedByClosureIncompletePayload),
    #[serde(rename = "commit.aborted_by_pending_commit")]
    CommitAbortedByPendingCommit(CommitAbortedByPendingCommitPayload),
    #[serde(rename = "commit.authorized")]
    CommitAuthorized(CommitAuthorizedPayload),
    #[serde(rename = "commit.aborted_by_closure_grew")]
    CommitAbortedByClosureGrew(CommitAbortedByClosureGrewPayload),
    #[serde(rename = "commit.completed")]
    CommitCompleted(CommitCompletedPayload),
    #[serde(rename = "commit.aborted_pre_mutation")]
    CommitAbortedPreMutation(CommitAbortedPreMutationPayload),
    #[serde(rename = "commit.aborted_post_mutation")]
    CommitAbortedPostMutation(CommitAbortedPostMutationPayload),
    #[serde(rename = "commit.recovery_required")]
    CommitRecoveryRequired(CommitRecoveryRequiredPayload),
    #[serde(rename = "commit.forced_override")]
    CommitForcedOverride(CommitForcedOverridePayload),
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
            Self::NodeRegistered(_) => EventKind::NodeRegistered,
            Self::NodeHeartbeatRecorded(_) => EventKind::NodeHeartbeatRecorded,
            Self::NodeMarkedStale(_) => EventKind::NodeMarkedStale,
            Self::NodeRetired(_) => EventKind::NodeRetired,
            Self::WorkerRegistered(_) => EventKind::WorkerRegistered,
            Self::WorkerLinkedToNode(_) => EventKind::WorkerLinkedToNode,
            Self::WorkerCapabilityRecorded(_) => EventKind::WorkerCapabilityRecorded,
            Self::WorkerGrantRecorded(_) => EventKind::WorkerGrantRecorded,
            Self::WorkerRetired(_) => EventKind::WorkerRetired,
            Self::ArtifactHandleCreated(_) => EventKind::ArtifactHandleCreated,
            Self::ArtifactLocationRecorded(_) => EventKind::ArtifactLocationRecorded,
            Self::ArtifactLocationRetired(_) => EventKind::ArtifactLocationRetired,
            Self::ArtifactLineageRecorded(_) => EventKind::ArtifactLineageRecorded,
            Self::ArtifactStaged(_) => EventKind::ArtifactStaged,
            Self::ArtifactVerificationStarted(_) => EventKind::ArtifactVerificationStarted,
            Self::ArtifactVerificationSucceeded(_) => EventKind::ArtifactVerificationSucceeded,
            Self::ArtifactVerificationFailed(_) => EventKind::ArtifactVerificationFailed,
            Self::ArtifactCommitStarted(_) => EventKind::ArtifactCommitStarted,
            Self::ArtifactCommitCompleted(_) => EventKind::ArtifactCommitCompleted,
            Self::ArtifactCommitFailedPreMutation(_) => EventKind::ArtifactCommitFailedPreMutation,
            Self::ArtifactCommitRecoveryRequired(_) => EventKind::ArtifactCommitRecoveryRequired,
            Self::ArtifactTranscodeStarted(_) => EventKind::ArtifactTranscodeStarted,
            Self::ArtifactTranscodeProgress(_) => EventKind::ArtifactTranscodeProgress,
            Self::ArtifactTranscodeSucceeded(_) => EventKind::ArtifactTranscodeSucceeded,
            Self::ArtifactTranscodeFailed(_) => EventKind::ArtifactTranscodeFailed,
            Self::ArtifactRemuxStarted(_) => EventKind::ArtifactRemuxStarted,
            Self::ArtifactRemuxProgress(_) => EventKind::ArtifactRemuxProgress,
            Self::ArtifactRemuxSucceeded(_) => EventKind::ArtifactRemuxSucceeded,
            Self::ArtifactRemuxFailed(_) => EventKind::ArtifactRemuxFailed,
            Self::ArtifactAudioTranscodeStarted(_) => EventKind::ArtifactAudioTranscodeStarted,
            Self::ArtifactAudioTranscodeProgress(_) => EventKind::ArtifactAudioTranscodeProgress,
            Self::ArtifactAudioTranscodeSucceeded(_) => EventKind::ArtifactAudioTranscodeSucceeded,
            Self::ArtifactAudioTranscodeFailed(_) => EventKind::ArtifactAudioTranscodeFailed,
            Self::ArtifactAudioExtractStarted(_) => EventKind::ArtifactAudioExtractStarted,
            Self::ArtifactAudioExtractProgress(_) => EventKind::ArtifactAudioExtractProgress,
            Self::ArtifactAudioExtractSucceeded(_) => EventKind::ArtifactAudioExtractSucceeded,
            Self::ArtifactAudioExtractFailed(_) => EventKind::ArtifactAudioExtractFailed,
            Self::IssueOpened(_) => EventKind::IssueOpened,
            Self::IssueUpdated(_) => EventKind::IssueUpdated,
            Self::IssueResolved(_) => EventKind::IssueResolved,
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
            Self::UseLeaseAcquired(_) => EventKind::UseLeaseAcquired,
            Self::UseLeaseReleased(_) => EventKind::UseLeaseReleased,
            Self::UseLeaseExpired(_) => EventKind::UseLeaseExpired,
            Self::UseLeaseForceReleased(_) => EventKind::UseLeaseForceReleased,
            Self::UseLeaseRecoveredStaleIssuer(_) => EventKind::UseLeaseRecoveredStaleIssuer,
            Self::UseLeaseReanchoredByMove(_) => EventKind::UseLeaseReanchoredByMove,
            Self::CommitIntentRecorded(_) => EventKind::CommitIntentRecorded,
            Self::CommitAbortedByUseLease(_) => EventKind::CommitAbortedByUseLease,
            Self::CommitAbortedByStaleEvidence(_) => EventKind::CommitAbortedByStaleEvidence,
            Self::CommitAbortedByClosureIncomplete(_) => {
                EventKind::CommitAbortedByClosureIncomplete
            }
            Self::CommitAbortedByPendingCommit(_) => EventKind::CommitAbortedByPendingCommit,
            Self::CommitAuthorized(_) => EventKind::CommitAuthorized,
            Self::CommitAbortedByClosureGrew(_) => EventKind::CommitAbortedByClosureGrew,
            Self::CommitCompleted(_) => EventKind::CommitCompleted,
            Self::CommitAbortedPreMutation(_) => EventKind::CommitAbortedPreMutation,
            Self::CommitAbortedPostMutation(_) => EventKind::CommitAbortedPostMutation,
            Self::CommitRecoveryRequired(_) => EventKind::CommitRecoveryRequired,
            Self::CommitForcedOverride(_) => EventKind::CommitForcedOverride,
        }
    }
}

#[cfg(test)]
#[path = "payload_test.rs"]
mod tests;
