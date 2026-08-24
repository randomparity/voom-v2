//! Data-only audio report types decoded from durable ticket results by the
//! policy-compliance surface, plus the byte-free selection derivation shared
//! with envelope rendering (ADR 0075).
//!
//! The bundled control-plane audio execute path was removed in the T8 sweep:
//! audio synth/transcode/extract now execute exclusively through their
//! storage owner's agent via `media_dispatch` envelopes.
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, MediaSnapshotId,
};

pub(crate) mod selection;

/// One synthesized companion as recorded in a completed synthesis ticket
/// result; decoded data-only for compliance reporting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteSynthesisCompanionReport {
    pub ordinal: u32,
    pub companion_id: String,
    pub source_file_version_id: FileVersionId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub result_snapshot_stream_id: String,
    pub result_provider_stream_index: u32,
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub lineage_id: u64,
    pub location: std::path::PathBuf,
    pub codec: String,
    pub channels: u32,
    pub language: Option<String>,
    pub title: Option<String>,
    pub disposition_default: bool,
    pub disposition_forced: bool,
    pub disposition_commentary: bool,
}

/// One extraction output as recorded in a completed extract ticket result;
/// decoded data-only for compliance reporting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteExtractAudioOutputReport {
    pub operation_output_id: u64,
    pub output_id: Option<String>,
    pub source_file_version_id: FileVersionId,
    pub source_media_snapshot_id: MediaSnapshotId,
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub role: String,
    pub staged_artifact_handle_id: ArtifactHandleId,
    pub staged_artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_file_asset_id: u64,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub bundle_member_id: u64,
    pub lineage_id: u64,
    pub staging_path: std::path::PathBuf,
    pub target_path: std::path::PathBuf,
}
