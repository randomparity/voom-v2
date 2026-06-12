#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Core domain types shared by every voom-* crate.

mod media;
mod runtime;
mod taxonomy;

pub mod clock {
    pub use crate::runtime::clock::{Clock, SystemClock, format_iso8601};
}

#[cfg(any(test, feature = "test"))]
pub mod clock_test_support {
    pub use crate::runtime::clock_test_support::{FrozenClock, ManualClock};
}

pub mod config {
    pub use crate::runtime::config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
}

pub mod encoder_caps {
    pub use crate::media::encoder_caps::{EncoderDescriptor, PresetDomain, encoder_descriptor};
}

pub mod error;

pub mod failure {
    pub use crate::taxonomy::failure::{FailureClass, FailureRetryClass};
}

pub mod ids {
    pub use crate::taxonomy::ids::{
        ArtifactCommitRecordId, ArtifactHandleId, ArtifactLocationId, ArtifactVerificationId,
        BundleId, CommitId, EventId, EvidenceId, FileAssetId, FileLocationId, FileVersionId,
        IssueId, JobId, LeaseId, MediaSnapshotId, MediaVariantId, MediaWorkId, NodeId,
        PolicyDocumentId, PolicyInputSetId, PolicySyntheticTargetId, PolicyVersionId, TicketId,
        UseLeaseId, WorkerId,
    };
}

pub mod issue {
    pub use crate::taxonomy::issue::{IssuePriority, IssueSeverity};
}

pub mod operation_kind {
    pub use crate::taxonomy::operation_kind::OperationKind;
}

pub mod remux {
    pub use crate::media::remux::{
        REMUX_CONTAINER_MKV, RemuxTrackGroup, is_supported_remux_container,
    };
}

#[cfg(any(test, feature = "test"))]
pub mod rng_test_support {
    pub use crate::runtime::rng_test_support::{FrozenRng, SeededRng};
}

pub mod ticket_operation {
    pub use crate::taxonomy::ticket_operation::TicketOperation;
}

pub mod transcode_video_profile {
    pub use crate::media::transcode_video_profile::{
        TRANSCODE_VIDEO_CODEC, TRANSCODE_VIDEO_CODEC_ALIAS_H265, TRANSCODE_VIDEO_CODEC_AV1,
        TRANSCODE_VIDEO_CONTAINER, TRANSCODE_VIDEO_CONTAINER_MP4, TRANSCODE_VIDEO_PROFILE,
        TranscodeVideoProfile, canonical_video_codec, is_supported_transcode_video_codec,
        is_supported_transcode_video_container, normalize_codec_token,
        validate_profile_against_descriptor,
    };
}

pub mod version {
    pub use crate::runtime::version::VersionInfo;
}

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
pub use taxonomy::execution_vocab::{NodeKind, NodeStatus, WorkerKind, WorkerStatus};
pub use ticket_operation::TicketOperation;
pub use transcode_video_profile::{
    TRANSCODE_VIDEO_CODEC, TRANSCODE_VIDEO_CODEC_ALIAS_H265, TRANSCODE_VIDEO_CODEC_AV1,
    TRANSCODE_VIDEO_CONTAINER, TRANSCODE_VIDEO_CONTAINER_MP4, TRANSCODE_VIDEO_PROFILE,
    TranscodeVideoProfile, canonical_video_codec, is_supported_transcode_video_codec,
    is_supported_transcode_video_container, normalize_codec_token,
    validate_profile_against_descriptor,
};
pub use version::VersionInfo;

/// Worker-protocol wire version (Sprint 2). Consumed by
/// `voom-worker-protocol`'s handshake and middleware.
///
/// Workers are bundled, co-deployed, and version-locked with the
/// control-plane build (ADR-0002), so the contract is an **exact match**:
/// a worker whose offered version is not equal to `PROTOCOL_VERSION` is
/// rejected at the `/v1/handshake` negotiation — and again by the
/// operations-path middleware — with
/// `ProtocolError::UnsupportedProtocolVersion`. There is no supported
/// version range; skew is rejected by design. Bumping this constant is a
/// flag day: every worker and the control plane move together because they
/// are the same release. See ADR-0016
/// (`docs/adr/0016-worker-protocol-exact-version-match.md`).
pub const PROTOCOL_VERSION: u32 = 1;
