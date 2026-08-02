use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, BundleId, FailureClass, FileAssetId, FileLocationId,
    FileVersionId, JobId, LeaseId, MediaSnapshotId, TicketId, UseLeaseId, WorkerId,
};

// --- artifacts -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactHandleCreatedPayload {
    pub artifact_handle_id: ArtifactHandleId,
    pub privacy_class: String,
    pub durability_class: String,
    pub mutability: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactLocationRecordedPayload {
    pub artifact_location_id: ArtifactLocationId,
    pub artifact_handle_id: ArtifactHandleId,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactLocationRetiredPayload {
    pub artifact_location_id: ArtifactLocationId,
    pub artifact_handle_id: ArtifactHandleId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactLineageRecordedPayload {
    pub artifact_lineage_id: u64,
    pub parent_artifact_id: ArtifactHandleId,
    pub child_artifact_id: ArtifactHandleId,
    pub operation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactStagedPayload {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: Option<FileLocationId>,
    pub staging_path: String,
    pub size_bytes: u64,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactVerificationStartedPayload {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub worker_id: WorkerId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactVerificationSucceededPayload {
    pub verification_id: ArtifactVerificationId,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub worker_id: WorkerId,
    pub observed_size_bytes: u64,
    pub observed_checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactVerificationFailedPayload {
    pub verification_id: ArtifactVerificationId,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub worker_id: WorkerId,
    pub error_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCommitStartedPayload {
    pub commit_record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub source_file_version_id: FileVersionId,
    pub verification_id: ArtifactVerificationId,
    pub target_path: String,
    pub temp_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCommitCompletedPayload {
    pub commit_record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub target_path: String,
    /// Use-lease ids the commit safety gate evaluated against the affected
    /// scope at prepare time (none blocked, or the commit would not have
    /// completed). Audit trail for #270. `#[serde(default)]` per the
    /// durable-payload evolution contract (ADR 0013): records written before
    /// this field decode to an empty vec.
    #[serde(default)]
    pub gate_evaluated_lease_ids: Vec<UseLeaseId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCommitFailedPreMutationPayload {
    pub artifact_handle_id: ArtifactHandleId,
    pub commit_record_id: Option<ArtifactCommitRecordId>,
    pub target_path: String,
    pub error_code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCommitRecoveryRequiredPayload {
    pub commit_record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub target_path: String,
    pub temp_path: String,
    pub recovery_reason: String,
    pub error_code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactTranscodeStartedPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
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
#[serde(deny_unknown_fields)]
pub struct ArtifactTranscodeProgressPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
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
#[serde(deny_unknown_fields)]
pub struct ArtifactTranscodeSucceededPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
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
    #[serde(default)]
    pub hardware_backend: Option<String>,
    #[serde(default)]
    pub hardware_token: Option<String>,
    #[serde(default)]
    pub hardware_device_uuid: Option<String>,
    #[serde(default)]
    pub hardware_resource_id: Option<String>,
    pub provider: String,
    pub provider_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactTranscodeFailedPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: Option<FileLocationId>,
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
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactRemuxStreamPayload>,
    pub default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub clear_default_streams: Vec<ArtifactRemuxStreamPayload>,
    #[serde(default)]
    pub head_streams: Vec<ArtifactRemuxStreamPayload>,
    pub track_order: Vec<String>,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRemuxProgressPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactRemuxStreamPayload>,
    pub default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub clear_default_streams: Vec<ArtifactRemuxStreamPayload>,
    #[serde(default)]
    pub head_streams: Vec<ArtifactRemuxStreamPayload>,
    pub percent_bps: Option<u16>,
    pub message: Option<String>,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRemuxSucceededPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactRemuxStreamPayload>,
    pub default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub clear_default_streams: Vec<ArtifactRemuxStreamPayload>,
    #[serde(default)]
    pub head_streams: Vec<ArtifactRemuxStreamPayload>,
    pub kept_snapshot_stream_ids: Vec<String>,
    pub default_snapshot_stream_ids: Vec<String>,
    pub output_container: String,
    pub provider: String,
    pub provider_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRemuxFailedPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: Option<FileLocationId>,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_location_id: Option<ArtifactLocationId>,
    pub staging_path: Option<String>,
    pub selected_streams: Vec<ArtifactRemuxStreamPayload>,
    pub default_streams: Vec<ArtifactRemuxStreamPayload>,
    pub clear_default_streams: Vec<ArtifactRemuxStreamPayload>,
    #[serde(default)]
    pub head_streams: Vec<ArtifactRemuxStreamPayload>,
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
pub struct ArtifactAudioSynthesisCompanionPayload {
    pub companion_id: String,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub result_snapshot_stream_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_provider_stream_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<ArtifactAudioDispositionPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioTranscodeStartedPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactAudioStreamPayload>,
    pub target_codec: String,
    pub output_container: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_operation_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synthesized_companions: Vec<ArtifactAudioSynthesisCompanionPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioTranscodeProgressPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub source_media_snapshot_id: MediaSnapshotId,
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
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub staging_path: String,
    pub selected_streams: Vec<ArtifactAudioStreamPayload>,
    pub selected_snapshot_stream_ids: Vec<String>,
    pub selected_output_streams: Vec<ArtifactAudioOutputStreamPayload>,
    pub output_container: String,
    pub output_audio_codecs: Vec<String>,
    pub provider: String,
    pub provider_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_operation_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synthesized_companions: Vec<ArtifactAudioSynthesisCompanionPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioTranscodeFailedPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: Option<FileLocationId>,
    pub source_media_snapshot_id: Option<MediaSnapshotId>,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_location_id: Option<ArtifactLocationId>,
    pub staging_path: Option<String>,
    pub selected_streams: Vec<ArtifactAudioStreamPayload>,
    pub selected_output_streams: Vec<ArtifactAudioOutputStreamPayload>,
    pub failure_class: FailureClass,
    pub error_code: String,
    pub message: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_operation_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synthesized_companions: Vec<ArtifactAudioSynthesisCompanionPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractMemberPayload {
    pub ordinal: u64,
    pub output_id: Option<String>,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub role: String,
    pub staging_path: String,
    pub target_path: String,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_location_id: Option<ArtifactLocationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractStartedPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_bundle_id: BundleId,
    pub staging_path: String,
    pub selected_stream: ArtifactAudioStreamPayload,
    pub role: String,
    pub target_codec: String,
    pub output_container: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<ArtifactAudioExtractMemberPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractProgressPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_bundle_id: BundleId,
    pub staging_path: String,
    pub selected_stream: ArtifactAudioStreamPayload,
    pub percent_bps: Option<u16>,
    pub message: Option<String>,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractOutputPayload {
    pub output_id: Option<String>,
    pub source_file_version_id: FileVersionId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub role: String,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_file_asset_id: FileAssetId,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub bundle_member_id: u64,
    pub lineage_id: u64,
    pub staging_path: String,
    pub target_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractSucceededPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_bundle_id: BundleId,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub staging_path: String,
    pub selected_stream: ArtifactAudioStreamPayload,
    pub selected_snapshot_stream_id: String,
    pub role: String,
    pub output_container: String,
    pub output_audio_codec: String,
    pub provider: String,
    pub provider_version: String,
    #[serde(default)]
    pub outputs: Vec<ArtifactAudioExtractOutputPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractFailedPayload {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: Option<LeaseId>,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: Option<FileLocationId>,
    pub source_media_snapshot_id: Option<MediaSnapshotId>,
    pub source_bundle_id: BundleId,
    pub artifact_handle_id: Option<ArtifactHandleId>,
    pub artifact_location_id: Option<ArtifactLocationId>,
    pub staging_path: Option<String>,
    pub selected_stream: Option<ArtifactAudioStreamPayload>,
    pub role: Option<String>,
    pub failure_class: FailureClass,
    pub error_code: String,
    pub message: String,
    pub provider: Option<String>,
    pub provider_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<ArtifactAudioExtractMemberPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAudioExtractQuiescedPayload {
    pub operation_key: String,
    pub generation: u32,
    pub attempt_id: u64,
    pub worker_id: WorkerId,
    pub worker_epoch: u32,
    pub idempotency_key: String,
    pub acknowledged_by: String,
    pub acknowledged_at: OffsetDateTime,
}

#[cfg(test)]
#[path = "artifact_test.rs"]
mod tests;
