use serde::{Deserialize, Serialize};
use voom_core::{FileKeyFacts, HashSource};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFileFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<String>,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedFileFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_file_key: Option<String>,
}

/// Reuse guard the scan pump copies from the owner's hash result (ADR 0120).
///
/// It is compared against the file's stat before and after ffprobe; it is
/// never stored or compared as a content hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeGuard {
    /// Pre-read inode change time of the hashed file, in ns since the epoch.
    pub changed_at_ns: i128,
    pub file_key: FileKeyFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeFileRequest {
    pub path: String,
    pub expected: ExpectedFileFacts,
    /// Present only on scan probes with the owner-node reuse setting on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<ProbeGuard>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeFileStatus {
    Probed,
}

/// Producer bound on `ProbeFileResult.snapshot` as compact JSON (ADR 0122). The 4 KiB below
/// `MAX_FRAME_BYTES` and `voom_core::MAX_EVIDENCE_BYTES` holds the result envelope and the
/// scan evidence fields; `probe_file_test.rs` pins both.
pub const MAX_PROBE_SNAPSHOT_BYTES: usize = 60 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeFileResult {
    pub status: ProbeFileStatus,
    pub provider: String,
    pub provider_version: String,
    pub pre_probe: ObservedFileFacts,
    pub post_probe: ObservedFileFacts,
    pub snapshot: serde_json::Value,
    /// `Reused` only when a guarded probe echoed the expected hash; omitted
    /// when the hash was recomputed from the file bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_source: Option<HashSource>,
}

#[cfg(test)]
#[path = "probe_file_test.rs"]
mod tests;
