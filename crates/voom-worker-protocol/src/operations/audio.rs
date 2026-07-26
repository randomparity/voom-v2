use std::collections::HashSet;
use std::path::{Component, Path};

use serde::de::Deserializer;
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
/// One stable source selection and destination in an ordered extraction request.
pub struct ExtractAudioOutputDescriptor {
    pub output_id: String,
    pub selection: AudioStreamRef,
    pub output: ExtractAudioOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractAudioRequest {
    pub input: ExtractAudioInput,
    pub output: ExtractAudioOutput,
    pub selection: AudioStreamRef,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_vec"
    )]
    pub outputs: Option<Vec<ExtractAudioOutputDescriptor>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractAudioStatus {
    Extracted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// One observed extraction output correlated to its request descriptor.
pub struct ExtractAudioOutputResult {
    pub output_id: String,
    pub selection: AudioStreamRef,
    pub path: String,
    pub output: AudioObservedFacts,
    pub output_container: String,
    pub output_audio_codec: String,
    pub output_language: Option<String>,
    pub output_title: Option<String>,
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_vec"
    )]
    pub outputs: Option<Vec<ExtractAudioOutputResult>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
/// A deterministic request/result contract violation at the extraction boundary.
pub struct ExtractAudioContractError {
    message: String,
}

/// Validates the legacy extraction fields and any authoritative ordered output list.
///
/// The singular fields remain the exact projection of the first plural output.
pub fn validate_extract_audio_request(
    request: &ExtractAudioRequest,
) -> Result<(), ExtractAudioContractError> {
    validate_extract_input(request)?;
    validate_extract_output(&request.output)?;
    validate_stream_ref(&request.selection)?;
    let Some(outputs) = &request.outputs else {
        return Ok(());
    };
    let Some(first) = outputs.first() else {
        return Err(extract_contract_error(
            "extract_audio outputs must not be empty",
        ));
    };
    if first.output != request.output || first.selection != request.selection {
        return Err(extract_contract_error(
            "extract_audio first output projection must equal output and selection",
        ));
    }
    validate_extract_output_descriptors(outputs)
}

/// Validates a worker result against its extraction request.
///
/// Plural results must match request cardinality and order, and every descriptor
/// is correlated by identity, complete source reference, path, container, and codec.
pub fn validate_extract_audio_result(
    request: &ExtractAudioRequest,
    result: &ExtractAudioResult,
) -> Result<(), ExtractAudioContractError> {
    validate_extract_audio_request(request)?;
    validate_legacy_extract_result(request, result)?;
    match (&request.outputs, &result.outputs) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(extract_contract_error(
            "extract_audio result has an unexpected outputs list",
        )),
        (Some(_), None) => Err(extract_contract_error(
            "extract_audio result is missing the outputs list",
        )),
        (Some(request_outputs), Some(result_outputs)) => {
            validate_plural_extract_result(request_outputs, result, result_outputs)
        }
    }
}

fn deserialize_present_vec<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Vec::<T>::deserialize(deserializer).map(Some)
}

fn validate_extract_input(request: &ExtractAudioRequest) -> Result<(), ExtractAudioContractError> {
    if request.input.path.trim().is_empty() {
        return Err(extract_contract_error(
            "extract_audio input.path must not be empty",
        ));
    }
    Ok(())
}

fn validate_extract_output(output: &ExtractAudioOutput) -> Result<(), ExtractAudioContractError> {
    if output.staging_root.trim().is_empty() {
        return Err(extract_contract_error(
            "extract_audio output.staging_root must not be empty",
        ));
    }
    if output.path.trim().is_empty() {
        return Err(extract_contract_error(
            "extract_audio output.path must not be empty",
        ));
    }
    if output.container != EXTRACT_AUDIO_CONTAINER || output.audio_codec != EXTRACT_AUDIO_CODEC {
        return Err(extract_contract_error(
            "extract_audio output must request opus in ogg",
        ));
    }
    if output.overwrite {
        return Err(extract_contract_error(
            "extract_audio output.overwrite must be false",
        ));
    }
    Ok(())
}

fn validate_stream_ref(selection: &AudioStreamRef) -> Result<(), ExtractAudioContractError> {
    if selection.snapshot_stream_id.trim().is_empty() {
        return Err(extract_contract_error(
            "extract_audio source snapshot_stream_id must not be empty",
        ));
    }
    Ok(())
}

fn validate_extract_output_descriptors(
    outputs: &[ExtractAudioOutputDescriptor],
) -> Result<(), ExtractAudioContractError> {
    let mut output_ids = HashSet::new();
    let mut source_ids = HashSet::new();
    let mut source_indexes = HashSet::new();
    let mut paths = HashSet::new();
    let mut previous_source_index = None;
    for output in outputs {
        validate_extract_output_descriptor(output)?;
        insert_unique(&mut output_ids, &output.output_id, "output_id")?;
        insert_unique(
            &mut source_ids,
            &output.selection.snapshot_stream_id,
            "source snapshot_stream_id",
        )?;
        if !source_indexes.insert(output.selection.provider_stream_index) {
            return Err(extract_contract_error(
                "extract_audio duplicate source provider_stream_index",
            ));
        }
        if previous_source_index
            .is_some_and(|previous| previous >= output.selection.provider_stream_index)
        {
            return Err(extract_contract_error(
                "extract_audio source provider_stream_index values must be strictly increasing",
            ));
        }
        previous_source_index = Some(output.selection.provider_stream_index);
        let path = normalized_path_key(&output.output.path);
        if !paths.insert(path) {
            return Err(extract_contract_error(
                "extract_audio duplicate normalized output path",
            ));
        }
    }
    Ok(())
}

fn validate_extract_output_descriptor(
    descriptor: &ExtractAudioOutputDescriptor,
) -> Result<(), ExtractAudioContractError> {
    if descriptor.output_id.trim().is_empty() {
        return Err(extract_contract_error(
            "extract_audio output_id must not be empty",
        ));
    }
    validate_stream_ref(&descriptor.selection)?;
    validate_extract_output(&descriptor.output)
}

fn insert_unique(
    values: &mut HashSet<String>,
    value: &str,
    field: &str,
) -> Result<(), ExtractAudioContractError> {
    if !values.insert(value.to_owned()) {
        return Err(extract_contract_error(format!(
            "extract_audio duplicate {field}"
        )));
    }
    Ok(())
}

fn normalized_path_key(path: &str) -> String {
    let mut prefix = String::new();
    let mut rooted = false;
    let mut segments = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Prefix(value) => {
                prefix = value.as_os_str().to_string_lossy().into_owned();
            }
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if segments.last().is_some_and(|segment| segment != "..") {
                    segments.pop();
                } else if !rooted {
                    segments.push("..".to_owned());
                }
            }
            Component::Normal(value) => {
                segments.push(value.to_string_lossy().into_owned());
            }
        }
    }
    let mut key = format!("{prefix}\0{rooted}\0{}", segments.join("\0"));
    key.make_ascii_lowercase();
    key
}

fn validate_legacy_extract_result(
    request: &ExtractAudioRequest,
    result: &ExtractAudioResult,
) -> Result<(), ExtractAudioContractError> {
    if result.output_container != request.output.container {
        return Err(extract_contract_error(
            "extract_audio result output_container does not match request",
        ));
    }
    if result.output_audio_codec != request.output.audio_codec {
        return Err(extract_contract_error(
            "extract_audio result output_audio_codec does not match request",
        ));
    }
    if result.selected_snapshot_stream_id != request.selection.snapshot_stream_id {
        return Err(extract_contract_error(
            "extract_audio result selected_snapshot_stream_id does not match request",
        ));
    }
    Ok(())
}

fn validate_plural_extract_result(
    request_outputs: &[ExtractAudioOutputDescriptor],
    result: &ExtractAudioResult,
    result_outputs: &[ExtractAudioOutputResult],
) -> Result<(), ExtractAudioContractError> {
    let Some(first) = result_outputs.first() else {
        return Err(extract_contract_error(
            "extract_audio result outputs must not be empty",
        ));
    };
    if !result_first_projection_matches(result, first) {
        return Err(extract_contract_error(
            "extract_audio result first output projection is inconsistent",
        ));
    }
    if request_outputs.len() != result_outputs.len() {
        return Err(extract_contract_error(format!(
            "extract_audio result output count {} does not match request count {}",
            result_outputs.len(),
            request_outputs.len()
        )));
    }
    for (ordinal, (expected, observed)) in request_outputs.iter().zip(result_outputs).enumerate() {
        validate_correlated_extract_output(ordinal, expected, observed)?;
    }
    Ok(())
}

fn result_first_projection_matches(
    result: &ExtractAudioResult,
    first: &ExtractAudioOutputResult,
) -> bool {
    result.output == first.output
        && result.output_container == first.output_container
        && result.output_audio_codec == first.output_audio_codec
        && result.selected_snapshot_stream_id == first.selection.snapshot_stream_id
        && result.output_language == first.output_language
        && result.output_title == first.output_title
}

fn validate_correlated_extract_output(
    ordinal: usize,
    expected: &ExtractAudioOutputDescriptor,
    observed: &ExtractAudioOutputResult,
) -> Result<(), ExtractAudioContractError> {
    if observed.output_id != expected.output_id {
        return Err(correlation_error(ordinal, "output_id"));
    }
    if observed.selection != expected.selection {
        return Err(correlation_error(ordinal, "selection"));
    }
    if observed.path != expected.output.path {
        return Err(correlation_error(ordinal, "path"));
    }
    if observed.output_container != expected.output.container {
        return Err(correlation_error(ordinal, "output_container"));
    }
    if observed.output_audio_codec != expected.output.audio_codec {
        return Err(correlation_error(ordinal, "output_audio_codec"));
    }
    Ok(())
}

fn correlation_error(ordinal: usize, field: &str) -> ExtractAudioContractError {
    extract_contract_error(format!(
        "extract_audio result outputs[{ordinal}].{field} does not match request"
    ))
}

fn extract_contract_error(message: impl Into<String>) -> ExtractAudioContractError {
    ExtractAudioContractError {
        message: message.into(),
    }
}

#[cfg(test)]
#[path = "audio_test.rs"]
mod tests;
