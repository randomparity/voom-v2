use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoExpectedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<String>,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoObservedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoInput {
    pub path: String,
    pub expected: TranscodeVideoExpectedFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoOutput {
    pub staging_root: String,
    pub path: String,
    pub container: String,
    pub video_codec: String,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoProfile {
    pub name: String,
    pub encoder: String,
    pub crf: u8,
    pub preset: String,
}

impl TranscodeVideoProfile {
    #[must_use]
    pub fn default_hevc() -> Self {
        Self {
            name: "default-hevc".to_owned(),
            encoder: "libx265".to_owned(),
            crf: 23,
            preset: "medium".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoRequest {
    pub input: TranscodeVideoInput,
    pub output: TranscodeVideoOutput,
    pub profile: TranscodeVideoProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscodeVideoStatus {
    Transcoded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoResult {
    pub status: TranscodeVideoStatus,
    pub provider: String,
    pub provider_version: String,
    pub input_pre: TranscodeVideoObservedFacts,
    pub input_post: TranscodeVideoObservedFacts,
    pub output: TranscodeVideoObservedFacts,
    pub output_container: String,
    pub output_video_codec: String,
}

#[cfg(test)]
#[path = "transcode_video_test.rs"]
mod tests;
