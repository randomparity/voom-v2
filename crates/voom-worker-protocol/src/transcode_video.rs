use serde::{Deserialize, Serialize};

pub const TRANSCODE_VIDEO_CONTAINER: &str = "mkv";
pub const TRANSCODE_VIDEO_CONTAINER_MP4: &str = "mp4";
pub const TRANSCODE_VIDEO_CODEC: &str = "hevc";
pub const TRANSCODE_VIDEO_CODEC_ALIAS_H265: &str = "h265";
pub const TRANSCODE_VIDEO_CODEC_AV1: &str = "av1";
pub const TRANSCODE_VIDEO_PROFILE: &str = "default-hevc";

#[must_use]
pub fn is_supported_transcode_video_container(container: &str) -> bool {
    container == TRANSCODE_VIDEO_CONTAINER || container == TRANSCODE_VIDEO_CONTAINER_MP4
}

#[must_use]
pub fn is_supported_transcode_video_codec(codec: &str) -> bool {
    codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC)
        || codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_ALIAS_H265)
        || codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_AV1)
}

/// Returns the canonical codec token (`"hevc"` or `"av1"`) for a recognized
/// codec name or alias, or `None` when unrecognized.
#[must_use]
pub fn canonical_video_codec(codec: &str) -> Option<&'static str> {
    if codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC)
        || codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_ALIAS_H265)
    {
        Some(TRANSCODE_VIDEO_CODEC)
    } else if codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_AV1) {
        Some(TRANSCODE_VIDEO_CODEC_AV1)
    } else {
        None
    }
}

use crate::encoder_caps::encoder_descriptor;

#[must_use]
pub fn is_default_hevc_profile(profile: &TranscodeVideoProfile) -> bool {
    profile == &TranscodeVideoProfile::default_hevc()
}

/// Validates a fully-typed profile against its encoder's capability descriptor.
/// Returns a stable, human-readable reason string on the first violation.
///
/// # Errors
/// Returns `Err` when the encoder is unknown, the target codec disagrees with
/// the encoder, or any field falls outside the encoder's vocabulary/range.
pub fn validate_profile_against_descriptor(profile: &TranscodeVideoProfile) -> Result<(), String> {
    let Some(descriptor) = encoder_descriptor(&profile.encoder) else {
        return Err(format!("unknown encoder `{}`", profile.encoder));
    };
    if descriptor.target_codec != profile.target_codec {
        return Err(format!(
            "encoder `{}` produces `{}`, not `{}`",
            profile.encoder, descriptor.target_codec, profile.target_codec
        ));
    }
    if !descriptor.accepts_crf(profile.crf) {
        return Err(format!(
            "crf {} outside {}..={} for `{}`",
            profile.crf, descriptor.crf_min, descriptor.crf_max, profile.encoder
        ));
    }
    if !descriptor.accepts_preset(&profile.preset) {
        return Err(format!(
            "preset `{}` invalid for `{}`",
            profile.preset, profile.encoder
        ));
    }
    if let Some(tune) = &profile.tune
        && !descriptor.accepts_tune(tune)
    {
        return Err(format!("tune `{tune}` invalid for `{}`", profile.encoder));
    }
    if let Some(codec_profile) = &profile.codec_profile
        && !descriptor.accepts_codec_profile(codec_profile)
    {
        return Err(format!(
            "codec_profile `{codec_profile}` invalid for `{}`",
            profile.encoder
        ));
    }
    if let Some(level) = &profile.codec_level
        && !descriptor.accepts_codec_level(level)
    {
        return Err(format!(
            "codec_level `{level}` invalid for `{}`",
            profile.encoder
        ));
    }
    if let Some(pixel_format) = &profile.pixel_format {
        if !descriptor.accepts_pixel_format(pixel_format) {
            return Err(format!(
                "pixel_format `{pixel_format}` invalid for `{}`",
                profile.encoder
            ));
        }
        if !descriptor
            .pixel_format_compatible_with_profile(pixel_format, profile.codec_profile.as_deref())
        {
            return Err(format!(
                "pixel_format `{pixel_format}` incompatible with codec_profile `{}`",
                profile.codec_profile.as_deref().unwrap_or("<none>")
            ));
        }
    }
    Ok(())
}

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
    pub target_codec: String,
    pub encoder: String,
    pub crf: u8,
    /// Encoder-specific speed token: named x265 preset, SVT-AV1 `-preset N`, or libaom-av1 `-cpu-used N`.
    pub preset: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tune: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub copy_compatible: bool,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if signature"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

impl TranscodeVideoProfile {
    #[must_use]
    pub fn default_hevc() -> Self {
        Self {
            name: TRANSCODE_VIDEO_PROFILE.to_owned(),
            target_codec: TRANSCODE_VIDEO_CODEC.to_owned(),
            encoder: "libx265".to_owned(),
            crf: 23,
            preset: "medium".to_owned(),
            tune: None,
            codec_profile: None,
            codec_level: None,
            pixel_format: None,
            max_width: None,
            max_height: None,
            copy_compatible: false,
        }
    }
}

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
