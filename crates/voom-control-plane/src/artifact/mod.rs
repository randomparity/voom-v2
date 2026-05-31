//! Artifact orchestration owned by the control plane.
//!
//! These modules coordinate staged files, artifact repository rows, verification
//! workers, host-side commits, and lifecycle events. The empty `voom-artifact`
//! crate is reserved for future reusable domain types; this module is the
//! authoritative runtime surface until that boundary is intentionally moved.

pub mod bootstrap;
pub mod commit;
pub(crate) mod commit_pipeline;
pub mod fs;
pub mod inspect;
pub mod stage;
pub mod verify;
pub mod worker;
