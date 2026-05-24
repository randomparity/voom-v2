#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor direct assertions over plumbing Result through every fixture helper"
    )
)]
//! Fake worker utilities used by integration tests and manual proofs.

pub mod remote_runner;

#[cfg(test)]
#[path = "remote_runner_test.rs"]
mod remote_runner_tests;
