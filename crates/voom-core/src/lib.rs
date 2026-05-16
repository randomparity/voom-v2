#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Core domain types shared by every voom-* crate.

pub mod config;
pub mod error;
pub mod version;

pub use config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
pub use error::VoomError;
pub use version::VersionInfo;
