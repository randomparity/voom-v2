//! Artifact domain helpers shared outside the control-plane application shell.
//!
//! Keep this crate focused on artifact rules with stable inputs and outputs.
//! Filesystem promotion, worker dispatch, and use-case assembly remain in
//! `voom-control-plane`, which owns application workflow coordination.

pub mod commit_pipeline;
