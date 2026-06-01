use serde::{Deserialize, Serialize};
use voom_core::FailureClass;

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

#[cfg(test)]
#[path = "artifact_test.rs"]
mod tests;
