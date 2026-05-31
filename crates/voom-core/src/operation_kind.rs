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
}

#[cfg(test)]
#[path = "operation_kind_test.rs"]
mod tests;
