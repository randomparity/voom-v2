//! The fixed operation vocabulary every Sprint 2 worker speaks.
//!
//! Mirrors the architectural-spec list verbatim
//! (`docs/specs/voom-control-plane-design.md` → Policy Compiler).
//! `serde` representation is `snake_case` so the wire JSON matches
//! the spec's vocabulary tokens exactly.

use serde::{Deserialize, Serialize};

/// One variant per architectural-spec fixed-operation. Plugin-defined
/// operations are out of Sprint 2 scope (Sprint 8 plugin SDK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    ScanLibrary,
    ProbeFile,
    HashFile,
    IdentifyMedia,
    ScoreQuality,
    SyncExternalSystem,
    BackUpFile,
    /// Remux / containerize.
    Remux,
    TranscodeVideo,
    TranscodeAudio,
    EditTracks,
    ExtractAudio,
    TranscribeAudio,
    VerifyArtifact,
    CommitArtifact,
    DeleteArtifact,
}

impl OperationKind {
    pub const ALL: &'static [Self] = &[
        Self::ScanLibrary,
        Self::ProbeFile,
        Self::HashFile,
        Self::IdentifyMedia,
        Self::ScoreQuality,
        Self::SyncExternalSystem,
        Self::BackUpFile,
        Self::Remux,
        Self::TranscodeVideo,
        Self::TranscodeAudio,
        Self::EditTracks,
        Self::ExtractAudio,
        Self::TranscribeAudio,
        Self::VerifyArtifact,
        Self::CommitArtifact,
        Self::DeleteArtifact,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScanLibrary => "scan_library",
            Self::ProbeFile => "probe_file",
            Self::HashFile => "hash_file",
            Self::IdentifyMedia => "identify_media",
            Self::ScoreQuality => "score_quality",
            Self::SyncExternalSystem => "sync_external_system",
            Self::BackUpFile => "back_up_file",
            Self::Remux => "remux",
            Self::TranscodeVideo => "transcode_video",
            Self::TranscodeAudio => "transcode_audio",
            Self::EditTracks => "edit_tracks",
            Self::ExtractAudio => "extract_audio",
            Self::TranscribeAudio => "transcribe_audio",
            Self::VerifyArtifact => "verify_artifact",
            Self::CommitArtifact => "commit_artifact",
            Self::DeleteArtifact => "delete_artifact",
        }
    }
}

#[cfg(test)]
#[path = "operation_kind_test.rs"]
mod tests;
