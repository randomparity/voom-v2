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
pub mod encoder_caps;
pub mod error;
pub mod failure;
pub mod ids;
pub mod issue;
pub mod operation_kind;
pub mod remux;
#[cfg(any(test, feature = "test-support"))]
pub mod rng_test_support;
pub mod transcode_video_profile;
pub mod version;

pub use clock::{Clock, SystemClock, format_iso8601};
pub use config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
pub use encoder_caps::{EncoderDescriptor, PresetDomain, encoder_descriptor};
pub use error::{ErrorCode, VoomError};
pub use failure::{FailureClass, FailureRetryClass};
pub use ids::{
    ArtifactHandleId, ArtifactLocationId, BundleId, CommitId, EventId, EvidenceId, FileAssetId,
    FileLocationId, FileVersionId, IssueId, JobId, LeaseId, MediaSnapshotId, MediaVariantId,
    MediaWorkId, NodeId, PolicyDocumentId, PolicyInputSetId, PolicySyntheticTargetId,
    PolicyVersionId, TicketId, UseLeaseId, WorkerId,
};
pub use issue::{IssuePriority, IssueSeverity};
pub use operation_kind::OperationKind;
pub use remux::{REMUX_CONTAINER_MKV, RemuxTrackGroup, is_supported_remux_container};
pub use transcode_video_profile::{
    TRANSCODE_VIDEO_CODEC, TRANSCODE_VIDEO_CODEC_ALIAS_H265, TRANSCODE_VIDEO_CODEC_AV1,
    TRANSCODE_VIDEO_CONTAINER, TRANSCODE_VIDEO_CONTAINER_MP4, TRANSCODE_VIDEO_PROFILE,
    TranscodeVideoProfile, canonical_video_codec, is_supported_transcode_video_codec,
    is_supported_transcode_video_container, normalize_codec_token,
    validate_profile_against_descriptor,
};
pub use version::VersionInfo;

/// Worker-protocol wire version (Sprint 2). Consumed by
/// `voom-worker-protocol`'s handshake and middleware. Bumped only
/// when the on-wire shape changes in an incompatible way.
pub const PROTOCOL_VERSION: u32 = 1;

/// Minimum protocol version this binary will accept on a handshake.
/// A worker offering a lower value is rejected with
/// `ProtocolError::UnsupportedProtocolVersion`.
pub const PROTOCOL_VERSION_SUPPORTED_MIN: u32 = 1;

/// Maximum protocol version this binary will accept on a handshake.
/// A worker offering a higher value is rejected with
/// `ProtocolError::UnsupportedProtocolVersion`.
pub const PROTOCOL_VERSION_SUPPORTED_MAX: u32 = 1;
