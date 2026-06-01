//! Artifact orchestration owned by the control plane.
//!
//! These modules coordinate staged files, artifact repository rows, verification
//! workers, host-side commits, and lifecycle events. The empty `voom-artifact`
//! crate is reserved for future reusable domain types; this module is the
//! authoritative runtime surface until that boundary is intentionally moved.

pub(crate) mod bootstrap;
pub(crate) mod commit;
pub(crate) mod commit_pipeline;
pub(crate) mod fs;
pub(crate) mod inspect;
pub(crate) mod stage;
pub(crate) mod verify;
pub(crate) mod worker;

pub use commit::{
    CommitArtifactCommandError, CommitArtifactInput, CommitArtifactPreMutationReport,
    CommitArtifactReport, CommitRecoveryReport,
};
pub use inspect::{
    ArtifactDetail, ArtifactInspectionState, ArtifactListInput, ArtifactSummary, CommitSummary,
    PathFacts, PathObservation, RecoverySummary, VerificationSummary,
};
pub use stage::{StageCopyCommandError, StageCopyInput, StageCopyReport};
pub use verify::{VerifyArtifactInput, VerifyArtifactReport};
