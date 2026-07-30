use serde::{Deserialize, Serialize};

use crate::encoder_caps::{
    EncoderDescriptor, PresetDomain, QualityDomain, VideoEncoderBackend, encoder_descriptor,
};

pub const TRANSCODE_VIDEO_CONTAINER: &str = "mkv";
pub const TRANSCODE_VIDEO_CONTAINER_MP4: &str = "mp4";
pub const TRANSCODE_VIDEO_CODEC: &str = "hevc";
pub const TRANSCODE_VIDEO_CODEC_ALIAS_H265: &str = "h265";
pub const TRANSCODE_VIDEO_CODEC_AV1: &str = "av1";
pub const TRANSCODE_VIDEO_CODEC_H264: &str = "h264";
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
        || codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_H264)
}

/// Normalizes codec profile/level tokens for comparison. ffprobe reports e.g.
/// `"Main 10"` while a profile uses `"main10"`; collapse case and whitespace so
/// the two compare equal.
#[must_use]
pub fn normalize_codec_token(token: &str) -> String {
    token
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
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
    } else if codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_H264) {
        Some(TRANSCODE_VIDEO_CODEC_H264)
    } else {
        None
    }
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
    validate_quality_domain(descriptor, profile)?;
    validate_decode_pairing(descriptor, profile)?;
    validate_preset(descriptor, profile)?;
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
    // Outside the `if let` above, deliberately: the ten-bit direction has to reject
    // an absent `pixel_format` too, because on a surface encoder absent resolves to
    // the eight-bit default and the profile then fails at execution rather than here.
    if !descriptor.profile_requires_ten_bit_pixel_format(
        profile.codec_profile.as_deref(),
        profile.pixel_format.as_deref(),
    ) {
        return Err(format!(
            "codec_profile `{}` requires a ten-bit pixel_format on `{}`, but the profile names \
             `{}`",
            profile.codec_profile.as_deref().unwrap_or("<none>"),
            profile.encoder,
            profile.pixel_format.as_deref().unwrap_or("<none>")
        ));
    }
    validate_videotoolbox_tuple(profile, descriptor.backend)?;
    Ok(())
}

/// The pixel format an output file conforming to `profile` must carry, or `None` when
/// the profile constrains no pixel format.
///
/// Every comparison of a profile's `pixel_format` against an *observed file* format —
/// planner compliance, the stream-copy decision, worker output-fact verification, and
/// worker copy preconditions — must go through this. A hardware profile's
/// `pixel_format` names the surface its encoder consumes, which the file never records
/// (issue #409 design §2.2), so comparing the two directly reports every conforming
/// hardware encode as non-conforming and never converges.
///
/// # Errors
/// Returns `Err` when the encoder is unknown, or when its `pixel_format` has no
/// recorded output format. Both are impossible for a profile that passed
/// [`validate_profile_against_descriptor`], so either is a loud signal that a surface
/// format was added without recording what it writes.
pub fn expected_output_pixel_format(
    profile: &TranscodeVideoProfile,
) -> Result<Option<&str>, String> {
    let Some(pixel_format) = profile.pixel_format.as_deref() else {
        return Ok(None);
    };
    let descriptor = encoder_descriptor(&profile.encoder)
        .ok_or_else(|| format!("unknown encoder `{}`", profile.encoder))?;
    descriptor
        .output_pixel_format(pixel_format)
        .map(Some)
        .ok_or_else(|| {
            format!(
                "`{}` records no output pixel format for `{pixel_format}`",
                profile.encoder
            )
        })
}

/// Exactly one quality field is legal per encoder — the one its `QualityDomain` names.
/// The match is exhaustive over `(quality_domain, crf, cq, qp, bitrate_kbps)` with no
/// wildcard arm, so a new domain or a new quality field cannot be added without deciding
/// this rule.
fn validate_quality_domain(
    descriptor: &EncoderDescriptor,
    profile: &TranscodeVideoProfile,
) -> Result<(), String> {
    match (
        descriptor.quality_domain,
        profile.crf,
        profile.cq,
        profile.qp,
        profile.bitrate_kbps,
    ) {
        (QualityDomain::Crf { min, max }, Some(crf), None, None, None)
            if crf >= min && crf <= max =>
        {
            Ok(())
        }
        (QualityDomain::Cq { min, max }, None, Some(cq), None, None) if cq >= min && cq <= max => {
            Ok(())
        }
        (QualityDomain::Qp { min, max }, None, None, Some(qp), None) if qp >= min && qp <= max => {
            Ok(())
        }
        (QualityDomain::BitrateKbps { min, max }, None, None, None, Some(bitrate))
            if bitrate >= min && bitrate <= max =>
        {
            Ok(())
        }
        (QualityDomain::Crf { min, max }, crf, cq, qp, bitrate) => Err(format!(
            "`{}` requires only crf {min}..={max}; got crf={crf:?}, cq={cq:?}, qp={qp:?}, \
             bitrate_kbps={bitrate:?}",
            profile.encoder
        )),
        (QualityDomain::Cq { min, max }, crf, cq, qp, bitrate) => Err(format!(
            "`{}` requires only cq {min}..={max}; got crf={crf:?}, cq={cq:?}, qp={qp:?}, \
             bitrate_kbps={bitrate:?}",
            profile.encoder
        )),
        (QualityDomain::Qp { min, max }, crf, cq, qp, bitrate) => Err(format!(
            "`{}` requires only qp {min}..={max}; got crf={crf:?}, cq={cq:?}, qp={qp:?}, \
             bitrate_kbps={bitrate:?}",
            profile.encoder
        )),
        (QualityDomain::BitrateKbps { min, max }, crf, cq, qp, bitrate) => Err(format!(
            "`{}` requires only bitrate_kbps {min}..={max}; got crf={crf:?}, cq={cq:?}, \
             qp={qp:?}, bitrate_kbps={bitrate:?}",
            profile.encoder
        )),
    }
}

/// A hardware decode backend and the encoder must name the same accelerator: hardware
/// frames produced by one backend cannot enter the other's encoder. Software decode into
/// a hardware encoder stays legal — that is the explicit upload path.
fn validate_decode_pairing(
    descriptor: &EncoderDescriptor,
    profile: &TranscodeVideoProfile,
) -> Result<(), String> {
    if profile.decode.is_nvidia() && descriptor.backend != VideoEncoderBackend::Nvidia {
        return Err(format!(
            "NVIDIA decode requires an NVIDIA encoder, not `{}`",
            profile.encoder
        ));
    }
    if profile.decode.is_vaapi() && descriptor.backend != VideoEncoderBackend::Vaapi {
        return Err(format!(
            "VAAPI decode requires a VAAPI encoder, not `{}`",
            profile.encoder
        ));
    }
    if profile.decode.is_video_toolbox() && descriptor.backend != VideoEncoderBackend::VideoToolbox
    {
        return Err(format!(
            "VideoToolbox decode requires a VideoToolbox encoder, not `{}`",
            profile.encoder
        ));
    }
    Ok(())
}

/// Preset presence is driven by the encoder's `PresetDomain`. `None` means the encoder
/// has no speed knob at all, so a preset would be an operator setting mapping to no
/// `FFmpeg` flag; every other domain still requires one.
fn validate_preset(
    descriptor: &EncoderDescriptor,
    profile: &TranscodeVideoProfile,
) -> Result<(), String> {
    match (descriptor.preset_domain, profile.preset.as_deref()) {
        (PresetDomain::None, None) => Ok(()),
        (PresetDomain::None, Some(preset)) => Err(format!(
            "`{}` accepts no preset; got `{preset}`",
            profile.encoder
        )),
        (PresetDomain::Named(_) | PresetDomain::NumericRange { .. }, None) => {
            Err(format!("`{}` requires a preset", profile.encoder))
        }
        (PresetDomain::Named(_) | PresetDomain::NumericRange { .. }, Some(preset)) => {
            if descriptor.accepts_preset(preset) {
                Ok(())
            } else {
                Err(format!(
                    "preset `{preset}` invalid for `{}`",
                    profile.encoder
                ))
            }
        }
    }
}

fn validate_videotoolbox_tuple(
    profile: &TranscodeVideoProfile,
    backend: VideoEncoderBackend,
) -> Result<(), String> {
    if backend != VideoEncoderBackend::VideoToolbox {
        return Ok(());
    }
    let tuple = (
        profile.encoder.as_str(),
        profile.codec_profile.as_deref(),
        profile.codec_level.as_deref(),
        profile.pixel_format.as_deref(),
    );
    let valid = tuple
        == (
            "h264_videotoolbox",
            Some("high"),
            Some("4.1"),
            Some("yuv420p"),
        )
        || tuple == ("hevc_videotoolbox", Some("main"), None, Some("yuv420p"))
        || tuple
            == (
                "hevc_videotoolbox",
                Some("main10"),
                None,
                Some("yuv420p10le"),
            );
    if valid {
        Ok(())
    } else {
        Err(format!(
            "incomplete or incompatible VideoToolbox output tuple for `{}`",
            profile.encoder
        ))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoftwareVideoDecode {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NvidiaVideoDecode {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaapiVideoDecode {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoToolboxVideoDecode {}

/// Where video frames are decoded before entering the encoder graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoDecodeMode {
    Software(SoftwareVideoDecode),
    Nvidia(NvidiaVideoDecode),
    Vaapi(VaapiVideoDecode),
    VideoToolbox(VideoToolboxVideoDecode),
}

impl Default for VideoDecodeMode {
    fn default() -> Self {
        Self::Software(SoftwareVideoDecode {})
    }
}

impl VideoDecodeMode {
    #[must_use]
    pub const fn nvidia() -> Self {
        Self::Nvidia(NvidiaVideoDecode {})
    }

    #[must_use]
    pub const fn vaapi() -> Self {
        Self::Vaapi(VaapiVideoDecode {})
    }

    #[must_use]
    pub const fn video_toolbox() -> Self {
        Self::VideoToolbox(VideoToolboxVideoDecode {})
    }

    #[must_use]
    pub const fn is_software(&self) -> bool {
        match self {
            Self::Software(_) => true,
            Self::Nvidia(_) | Self::Vaapi(_) | Self::VideoToolbox(_) => false,
        }
    }

    #[must_use]
    pub const fn is_nvidia(&self) -> bool {
        match self {
            Self::Nvidia(_) => true,
            Self::Software(_) | Self::Vaapi(_) | Self::VideoToolbox(_) => false,
        }
    }

    #[must_use]
    pub const fn is_vaapi(&self) -> bool {
        match self {
            Self::Vaapi(_) => true,
            Self::Software(_) | Self::Nvidia(_) | Self::VideoToolbox(_) => false,
        }
    }

    #[must_use]
    pub const fn is_video_toolbox(&self) -> bool {
        match self {
            Self::VideoToolbox(_) => true,
            Self::Software(_) | Self::Nvidia(_) | Self::Vaapi(_) => false,
        }
    }

    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Software(_) => "software",
            Self::Nvidia(_) => "nvidia",
            Self::Vaapi(_) => "vaapi",
            Self::VideoToolbox(_) => "video_toolbox",
        }
    }

    /// Parses the durable `SQLite` vocabulary.
    ///
    /// # Errors
    /// Returns a descriptive error for a value outside the closed vocabulary.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "software" => Ok(Self::default()),
            "nvidia" => Ok(Self::nvidia()),
            "vaapi" => Ok(Self::vaapi()),
            "video_toolbox" => Ok(Self::video_toolbox()),
            _ => Err(format!("unknown video decode backend `{value}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoProfile {
    pub name: String,
    pub target_codec: String,
    pub encoder: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crf: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cq: Option<u8>,
    /// VAAPI's constant quantization parameter, used with `-rc_mode CQP`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qp: Option<u8>,
    /// `VideoToolbox`'s target bitrate in kilobits per second.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    /// Encoder-specific speed token: named x265 preset, SVT-AV1 `-preset N`, or libaom-av1 `-cpu-used N`.
    /// `None` for an encoder with no speed knob, e.g. `hevc_vaapi`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
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
    #[serde(default, skip_serializing_if = "VideoDecodeMode::is_software")]
    pub decode: VideoDecodeMode,
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
            crf: Some(23),
            cq: None,
            qp: None,
            bitrate_kbps: None,
            preset: Some("medium".to_owned()),
            tune: None,
            codec_profile: None,
            codec_level: None,
            pixel_format: None,
            max_width: None,
            max_height: None,
            decode: VideoDecodeMode::default(),
            copy_compatible: false,
        }
    }
}

#[cfg(test)]
#[path = "transcode_video_profile_test.rs"]
mod tests;
