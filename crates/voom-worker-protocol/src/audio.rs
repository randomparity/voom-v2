use serde::{Deserialize, Serialize};

pub const TRANSCODE_AUDIO_CONTAINER: &str = "mkv";
pub const TRANSCODE_AUDIO_CODEC_AAC: &str = "aac";
pub const TRANSCODE_AUDIO_CODEC_OPUS: &str = "opus";
pub const EXTRACT_AUDIO_CONTAINER: &str = "ogg";
pub const EXTRACT_AUDIO_CODEC: &str = "opus";

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
