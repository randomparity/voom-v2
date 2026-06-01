use serde::{Deserialize, Serialize};

pub use voom_core::{REMUX_CONTAINER_MKV, RemuxTrackGroup, is_supported_remux_container};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxExpectedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<String>,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxObservedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxInput {
    pub path: String,
    pub expected: RemuxExpectedFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxOutput {
    pub staging_root: String,
    pub path: String,
    pub container: String,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxStreamRef {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxSelection {
    pub keep_streams: Vec<RemuxStreamRef>,
    pub default_streams: Vec<RemuxStreamRef>,
    pub clear_default_streams: Vec<RemuxStreamRef>,
    pub track_order: Vec<RemuxTrackGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxRequest {
    pub input: RemuxInput,
    pub output: RemuxOutput,
    pub selection: RemuxSelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemuxStatus {
    Remuxed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxResult {
    pub status: RemuxStatus,
    pub provider: String,
    pub provider_version: String,
    pub input_pre: RemuxObservedFacts,
    pub input_post: RemuxObservedFacts,
    pub output: RemuxObservedFacts,
    pub output_container: String,
    pub kept_snapshot_stream_ids: Vec<String>,
    pub default_snapshot_stream_ids: Vec<String>,
}

#[cfg(test)]
#[path = "remux_test.rs"]
mod tests;
