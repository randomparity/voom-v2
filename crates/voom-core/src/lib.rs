#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Core domain types shared by every voom-* crate.

pub mod clock;
#[cfg(any(test, feature = "test-support"))]
pub mod clock_test_support;
pub mod config;
pub mod error;
pub mod ids;
pub mod version;

pub use clock::{Clock, SystemClock, format_iso8601};
pub use config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
pub use error::{ErrorCode, VoomError};
pub use ids::{
    ArtifactHandleId, ArtifactLocationId, EventId, JobId, LeaseId, MediaId, TicketId, WorkerId,
};
pub use version::VersionInfo;
