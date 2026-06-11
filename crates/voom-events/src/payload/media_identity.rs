use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// --- media identity --------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaWorkCreatedPayload {
    pub media_work_id: u64,
    pub kind: String,
    pub display_title: String,
    pub provisional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaVariantCreatedPayload {
    pub media_variant_id: u64,
    pub media_work_id: u64,
    pub label: String,
    pub provisional: bool,
}

// --- asset bundles ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetBundleCreatedPayload {
    pub bundle_id: u64,
    pub media_variant_id: u64,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetBundleMemberAddedPayload {
    pub bundle_id: u64,
    pub file_asset_id: u64,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetBundleMemberRemovedPayload {
    pub bundle_id: u64,
    pub file_asset_id: u64,
    pub role: String,
}

// --- file identity ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileAssetCreatedPayload {
    pub file_asset_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVersionCreatedPayload {
    pub file_version_id: u64,
    pub file_asset_id: u64,
    pub content_hash: String,
    pub size_bytes: u64,
    pub produced_by: String,
    pub produced_from_version_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLocationRecordedPayload {
    pub file_location_id: u64,
    pub file_version_id: u64,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLocationAliasedPayload {
    pub file_location_id: u64,
    pub file_version_id: u64,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLocationRetiredByMovePayload {
    pub file_location_id: u64,
    pub file_version_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub retired_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct IdentityEvidenceAcceptedPayload {
    pub evidence_id: u64,
    pub target_type: String,
    pub target_id: u64,
    pub accepted_user_id: Option<String>,
    #[serde(with = "time::serde::iso8601")]
    pub accepted_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct MediaSnapshotRecordedPayload {
    pub media_snapshot_id: u64,
    pub file_version_id: u64,
    pub probed_by_worker_id: Option<u64>,
    #[serde(with = "time::serde::iso8601")]
    pub probed_at: OffsetDateTime,
}

#[cfg(test)]
#[path = "media_identity_test.rs"]
mod tests;
