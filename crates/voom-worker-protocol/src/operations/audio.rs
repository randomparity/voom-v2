use serde::{Deserialize, Serialize};

pub const TRANSCODE_AUDIO_CONTAINER: &str = "mkv";
pub const TRANSCODE_AUDIO_CODEC_AAC: &str = "aac";
pub const TRANSCODE_AUDIO_CODEC_OPUS: &str = "opus";
pub const TRANSCODE_AUDIO_CODEC_EAC3: &str = "eac3";
/// The only audio quality profile defined so far. The control plane emits this
/// value for every transcode-audio request; see ADR 0020.
pub const AUDIO_PROFILE_DEFAULT: &str = "default";
pub const EXTRACT_AUDIO_CONTAINER: &str = "ogg";
pub const EXTRACT_AUDIO_CODEC: &str = "opus";

/// Returns true when `codec` is an audio codec the `transcode audio` operation
/// supports (aac, opus, or eac3).
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires a &T predicate signature"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[must_use]
pub fn is_supported_transcode_audio_codec(codec: &str) -> bool {
    matches!(
        codec,
        TRANSCODE_AUDIO_CODEC_AAC | TRANSCODE_AUDIO_CODEC_OPUS | TRANSCODE_AUDIO_CODEC_EAC3
    )
}

/// Resolves the per-channel target bitrate (kbps) for a `(codec, profile)` pair,
/// or `None` when the codec or profile is unsupported.
///
/// The ffmpeg worker multiplies this by the source stream's channel count to
/// emit a deterministic `-b:a`, so a 5.1 (6-channel) source is encoded at a
/// surround-appropriate bitrate. Only the `default` profile is defined; the
/// per-codec values reflect relative coding efficiency (opus < aac < eac3 for
/// equal quality). See ADR 0020.
#[must_use]
pub fn audio_target_bitrate_kbps_per_channel(codec: &str, profile: &str) -> Option<u32> {
    if profile != AUDIO_PROFILE_DEFAULT {
        return None;
    }
    match codec {
        TRANSCODE_AUDIO_CODEC_AAC => Some(64),
        TRANSCODE_AUDIO_CODEC_OPUS => Some(48),
        TRANSCODE_AUDIO_CODEC_EAC3 => Some(96),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioExpectedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<String>,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioObservedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioStreamRef {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioDispositionFact {
    pub default: Option<bool>,
    pub forced: Option<bool>,
    pub commentary: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioOutputStreamFact {
    pub snapshot_stream_id: String,
    pub output_provider_stream_index: u32,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: Option<bool>,
    pub disposition: Option<AudioDispositionFact>,
    pub channels: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeAudioInput {
    pub path: String,
    pub expected: AudioExpectedFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeAudioOutput {
    pub staging_root: String,
    pub path: String,
    pub container: String,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeAudioSelection {
    pub selected_streams: Vec<AudioStreamRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeAudioSettings {
    pub target_codec: String,
    pub profile: String,
    /// When true, the operation *adds* a downmixed companion track derived from
    /// each selected source stream instead of re-encoding it in place
    /// (`synthesize audio`, ADR 0026, #276). Additive; defaults to the
    /// replace-in-place transcode behavior and is omitted from the wire when
    /// false so the existing transcode request shape is unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub add_track: bool,
    /// Target channel count for the synthesized companion (a downmix). Required
    /// when `add_track` is true; ignored otherwise. Additive since ADR 0026.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_channels: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeAudioRequest {
    pub input: TranscodeAudioInput,
    pub output: TranscodeAudioOutput,
    pub selection: TranscodeAudioSelection,
    pub audio: TranscodeAudioSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscodeAudioStatus {
    Transcoded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeAudioResult {
    pub status: TranscodeAudioStatus,
    pub provider: String,
    pub provider_version: String,
    pub input_pre: AudioObservedFacts,
    pub input_post: AudioObservedFacts,
    pub output: AudioObservedFacts,
    pub output_container: String,
    pub selected_snapshot_stream_ids: Vec<String>,
    pub output_audio_codecs: Vec<String>,
    pub selected_output_streams: Vec<AudioOutputStreamFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractAudioInput {
    pub path: String,
    pub expected: AudioExpectedFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractAudioOutput {
    pub staging_root: String,
    pub path: String,
    pub container: String,
    pub audio_codec: String,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractAudioRequest {
    pub input: ExtractAudioInput,
    pub output: ExtractAudioOutput,
    pub selection: AudioStreamRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractAudioStatus {
    Extracted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractAudioResult {
    pub status: ExtractAudioStatus,
    pub provider: String,
    pub provider_version: String,
    pub input_pre: AudioObservedFacts,
    pub input_post: AudioObservedFacts,
    pub output: AudioObservedFacts,
    pub output_container: String,
    pub output_audio_codec: String,
    pub selected_snapshot_stream_id: String,
    pub output_language: Option<String>,
    pub output_title: Option<String>,
}

#[cfg(test)]
#[path = "audio_test.rs"]
mod tests;
