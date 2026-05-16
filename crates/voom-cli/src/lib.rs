#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Internal library exposing CLI plumbing to integration tests.

pub mod cli;
pub mod commands;
pub mod envelope;
pub mod logging;
