use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeFileRequest {
    pub path: String,
    pub expected: ExpectedFileFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeFileStatus {
    Probed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeFileResult {
    pub status: ProbeFileStatus,
    pub provider: String,
    pub provider_version: String,
    pub pre_probe: ObservedFileFacts,
    pub post_probe: ObservedFileFacts,
    pub snapshot: serde_json::Value,
}

#[cfg(test)]
#[path = "probe_file_test.rs"]
mod tests;
