use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyArtifactExpectedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<String>,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyArtifactObservedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyArtifactRequest {
    pub path: String,
    /// Directory the artifact must reside within. The worker rejects any
    /// `path` whose canonical parent is not contained by this root, mirroring
    /// the ffmpeg worker's staging-root containment so a control-plane bug
    /// cannot direct the verifier to read an arbitrary file.
    pub staging_root: String,
    pub expected: VerifyArtifactExpectedFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyArtifactStatus {
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyArtifactResult {
    pub status: VerifyArtifactStatus,
    pub provider: String,
    pub provider_version: String,
    pub observed: VerifyArtifactObservedFacts,
}

#[cfg(test)]
#[path = "verify_artifact_test.rs"]
mod tests;
