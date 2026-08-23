//! Node-agent media dispatch envelopes (ADR 0075).
//!
//! These are the control-plane→agent payload shapes for byte-touching media
//! operations. They carry only stable location vocabulary — `StorageRootId`
//! plus [`ProviderRelativeLocator`] — never an absolute path: the
//! storage-owning node agent resolves every handle against its own configured
//! bindings and hands absolute paths only to children it supervises locally.
//! The agent↔child wire types stay path-based inside that trusted node-local
//! boundary.
//!
//! The envelope family is lock-stepped with [`PROTOCOL_VERSION`]
//! (exact-match, ADR 0016). Each content struct additionally embeds a
//! `schema` field enforced at decode time by [`decode_media_dispatch`], so a
//! same-version binary whose payload shape drifted still fails before lease
//! execution instead of mid-dispatch.

use serde::{Deserialize, Serialize};

use voom_core::{PROTOCOL_VERSION, ProviderRelativeLocator};

use crate::VideoHardwareAssignment;
use crate::operations::audio::{
    AudioExpectedFacts, AudioStreamRef, TranscodeAudioSelection, TranscodeAudioSettings,
};
use crate::operations::probe_file::ExpectedFileFacts;
use crate::operations::remux::{RemuxExpectedFacts, RemuxSelection};
use crate::operations::transcode_video::{TranscodeVideoExpectedFacts, TranscodeVideoProfile};
use crate::operations::verify_artifact::VerifyArtifactExpectedFacts;

/// One existing, live rooted location on a storage root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaSourceRef {
    pub storage_root_id: voom_core::StorageRootId,
    pub provider_relative_locator: ProviderRelativeLocator,
}

/// One planned output file that does not exist yet, addressed on its
/// destination storage root. The control plane derives the relative locator
/// deterministically from branch/output identity; `overwrite` mirrors the
/// path-based worker outputs it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaPlannedOutput {
    pub storage_root_id: voom_core::StorageRootId,
    pub provider_relative_locator: ProviderRelativeLocator,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaProbeDispatch {
    pub schema: u32,
    pub source: MediaSourceRef,
    pub expected: ExpectedFileFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaTranscodeAudioDispatch {
    pub schema: u32,
    pub source: MediaSourceRef,
    pub expected: AudioExpectedFacts,
    /// Container of the staged output (path-based output vocabulary).
    pub output_container: String,
    pub output: MediaPlannedOutput,
    pub selection: TranscodeAudioSelection,
    pub settings: TranscodeAudioSettings,
}

/// One planned extraction destination in an ordered extraction dispatch —
/// the handle-shaped counterpart of the path-based extraction descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaExtractOutput {
    pub output_id: String,
    pub selection: AudioStreamRef,
    pub audio_codec: String,
    pub output: MediaPlannedOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaExtractAudioDispatch {
    pub schema: u32,
    pub source: MediaSourceRef,
    pub expected: AudioExpectedFacts,
    pub output_container: String,
    pub outputs: Vec<MediaExtractOutput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaTranscodeVideoDispatch {
    pub schema: u32,
    pub source: MediaSourceRef,
    pub expected: TranscodeVideoExpectedFacts,
    pub output_container: String,
    pub output_video_codec: String,
    pub output: MediaPlannedOutput,
    pub profile: TranscodeVideoProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_assignment: Option<VideoHardwareAssignment>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub copy_video: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaRemuxDispatch {
    pub schema: u32,
    pub source: MediaSourceRef,
    pub expected: RemuxExpectedFacts,
    pub output_container: String,
    pub output: MediaPlannedOutput,
    pub selection: RemuxSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaBackUpFileDispatch {
    pub schema: u32,
    pub source: MediaSourceRef,
    pub destination: MediaPlannedOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaVerifyArtifactDispatch {
    pub schema: u32,
    /// Staged artifact location to verify; the resolving agent supplies the
    /// containment root from this handle's storage root.
    pub target: MediaSourceRef,
    pub expected: VerifyArtifactExpectedFacts,
}

/// Handle-shaped dispatch envelope for one byte-touching media operation.
///
/// Tagged enum over annotated content structs per the durable-payload rule:
/// the tag discriminator rejects unknown operation names, and each content
/// struct carries `deny_unknown_fields`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum MediaDispatch {
    Probe(MediaProbeDispatch),
    TranscodeAudio(MediaTranscodeAudioDispatch),
    ExtractAudio(MediaExtractAudioDispatch),
    TranscodeVideo(MediaTranscodeVideoDispatch),
    Remux(MediaRemuxDispatch),
    BackUpFile(MediaBackUpFileDispatch),
    VerifyArtifact(MediaVerifyArtifactDispatch),
}

impl MediaDispatch {
    /// The envelope's embedded exact-match schema value.
    #[must_use]
    pub fn schema(&self) -> u32 {
        match self {
            Self::Probe(dispatch) => dispatch.schema,
            Self::TranscodeAudio(dispatch) => dispatch.schema,
            Self::ExtractAudio(dispatch) => dispatch.schema,
            Self::TranscodeVideo(dispatch) => dispatch.schema,
            Self::Remux(dispatch) => dispatch.schema,
            Self::BackUpFile(dispatch) => dispatch.schema,
            Self::VerifyArtifact(dispatch) => dispatch.schema,
        }
    }
}

/// Deterministic decode of a dispatch payload with exact schema enforcement.
///
/// Fails closed on unknown fields, unknown operation tags, and any schema
/// other than [`PROTOCOL_VERSION`] — the agent calls this **before** touching
/// a child so payload mismatch fails before lease execution.
pub fn decode_media_dispatch(payload: &serde_json::Value) -> Result<MediaDispatch, String> {
    let dispatch: MediaDispatch =
        serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
    if dispatch.schema() != PROTOCOL_VERSION {
        return Err(format!(
            "media dispatch schema {} does not match protocol version {PROTOCOL_VERSION}",
            dispatch.schema()
        ));
    }
    Ok(dispatch)
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if signature"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
#[path = "dispatch_test.rs"]
mod tests;
