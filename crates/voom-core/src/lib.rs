#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Core domain types shared by every voom-* crate.

pub mod clock;
pub mod config;
pub mod error;
pub mod ids;
pub mod version;

pub use clock::{Clock, SystemClock};
pub use config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
pub use error::VoomError;
pub use ids::{EventId, JobId, LeaseId, MediaId, TicketId, WorkerId};
pub use version::VersionInfo;
