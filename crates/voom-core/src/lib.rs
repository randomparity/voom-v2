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
pub mod failure;
pub mod ids;
pub mod issue;
#[cfg(any(test, feature = "test-support"))]
pub mod rng_test_support;
pub mod version;

pub use clock::{Clock, SystemClock, format_iso8601};
pub use config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
pub use error::{ErrorCode, VoomError};
pub use failure::{FailureClass, FailureRetryClass};
pub use ids::{
    ArtifactHandleId, ArtifactLocationId, BundleId, EventId, EvidenceId, FileAssetId,
    FileLocationId, FileVersionId, IssueId, JobId, LeaseId, MediaSnapshotId, MediaVariantId,
    MediaWorkId, TicketId, WorkerId,
};
pub use issue::{IssuePriority, IssueSeverity};
pub use version::VersionInfo;
