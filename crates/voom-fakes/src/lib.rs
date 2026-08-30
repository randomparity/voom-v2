#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor direct assertions over plumbing Result through every fixture helper"
    )
)]
//! Fake worker utilities used by integration tests and manual proofs.

#[cfg(test)]
mod process_supervisor;
pub mod remote_runner;

#[cfg(test)]
mod remote_stress;
