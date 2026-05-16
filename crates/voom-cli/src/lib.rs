#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Internal library exposing CLI plumbing to integration tests.

pub mod cli;
pub mod envelope;
pub mod logging;
