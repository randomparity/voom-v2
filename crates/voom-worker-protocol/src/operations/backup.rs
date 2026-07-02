use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackUpFileRequest {
    /// Absolute path of the source file to copy.
    pub source_path: String,
    /// Fully-qualified destination path the copy is written to. The control
    /// plane builds this collision-free (namespaced by source file version),
    /// so the worker receives a complete path, not just a directory.
    pub destination_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackUpFileStatus {
    BackedUp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackUpFileResult {
    pub status: BackUpFileStatus,
    pub provider: String,
    pub provider_version: String,
    pub destination_path: String,
    pub size_bytes: u64,
    pub checksum: String,
}

#[cfg(test)]
#[path = "backup_test.rs"]
mod tests;
