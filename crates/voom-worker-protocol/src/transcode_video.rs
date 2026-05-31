use serde::{Deserialize, Serialize};

pub use voom_core::{
    TRANSCODE_VIDEO_CODEC, TRANSCODE_VIDEO_CODEC_ALIAS_H265, TRANSCODE_VIDEO_CODEC_AV1,
    TRANSCODE_VIDEO_CONTAINER, TRANSCODE_VIDEO_CONTAINER_MP4, TRANSCODE_VIDEO_PROFILE,
    TranscodeVideoProfile, canonical_video_codec, is_supported_transcode_video_codec,
    is_supported_transcode_video_container, normalize_codec_token,
    validate_profile_against_descriptor,
};

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

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if signature"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// Worker request schema for `transcode_video`.
///
/// ffmpeg workers are co-deployed bundled binaries launched by the control
/// plane (ADR-0002 / `VOOM_FFMPEG_WORKER_BIN`), so this schema is lock-stepped
/// with the control-plane build. The required fields and `deny_unknown_fields`
/// are a deliberate fail-loud choice for an in-build contract — NOT a
/// cross-version durable-replay contract. There is no version skew to tolerate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoRequest {
    pub input: TranscodeVideoInput,
    pub output: TranscodeVideoOutput,
    pub profile: TranscodeVideoProfile,
    #[serde(default, skip_serializing_if = "is_false")]
    pub copy_video: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscodeVideoStatus {
    Transcoded,
}

/// Worker result schema for `transcode_video`.
///
/// Like [`TranscodeVideoRequest`], this schema is lock-stepped with the
/// control-plane build (bundled co-deployed worker, ADR-0002). The strict
/// fields are a fail-loud in-build contract, not a durable-replay format.
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
    pub output_width: u32,
    pub output_height: u32,
    pub output_pixel_format: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub copied_video: bool,
}

#[cfg(test)]
#[path = "transcode_video_test.rs"]
mod tests;
