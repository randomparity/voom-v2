//! Artifact orchestration owned by the control plane.
//!
//! These modules coordinate staged files, artifact repository rows, verification
//! workers, host-side commits, and lifecycle events. The `voom-artifact` crate
//! owns narrow, store-facing artifact helpers that no longer need the full
//! control-plane application surface.

pub(crate) mod bootstrap;
pub(crate) mod commit;
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
    ArtifactDetail, ArtifactInspectionState, ArtifactListInput, ArtifactListPage, ArtifactSummary,
    CommitSummary, PathFacts, PathObservation, RecoverySummary, VerificationSummary,
};
pub use stage::{StageCopyCommandError, StageCopyInput, StageCopyReport};
pub use verify::{VerifyArtifactInput, VerifyArtifactReport};
