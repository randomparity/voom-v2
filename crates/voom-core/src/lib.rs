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

pub mod artifact_access_declaration {
    pub use crate::taxonomy::artifact_access_declaration::{
        ArtifactAccessDeclaration, ArtifactAccessEntry, ArtifactAccessRight, ArtifactAccessTarget,
        ExistingArtifactAccess, FileLocationAccess, PlannedArtifactAccess, StorageRootAccess,
    };
}

pub mod artifact_access_mode {
    pub use crate::taxonomy::artifact_access_mode::ArtifactAccessMode;
}

#[cfg(any(test, feature = "test"))]
pub mod clock_test_support {
    pub use crate::runtime::clock_test_support::{FrozenClock, ManualClock};
}

pub mod config {
    pub use crate::runtime::config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
}

pub mod encoder_caps {
    pub use crate::media::encoder_caps::{
        EncoderDescriptor, NVIDIA_VIDEO_DECODERS, PresetDomain, QualityDomain,
        VAAPI_VIDEO_DECODERS, VideoEncoderBackend, encoder_descriptor,
        nvidia_decoder_for_video_codec, vaapi_video_decode_codec, video_pixel_format_depth,
    };
}

pub mod error;

pub mod failure {
    pub use crate::taxonomy::failure::{FailureClass, FailureRetryClass};
}

pub mod ids {
    pub use crate::taxonomy::ids::{
        ArtifactCommitRecordId, ArtifactHandleId, ArtifactLocationId, ArtifactVerificationId,
        BackupId, BundleId, CommitId, EventId, EvidenceId, ExternalPathMappingId, ExternalSystemId,
        ExternalSystemLinkId, FileAssetId, FileLocationId, FileVersionId, IssueId, JobId, LeaseId,
        LibraryId, LibraryRootId, MediaSnapshotId, MediaVariantId, MediaWorkId, NodeId,
        NodeIncarnationId, PolicyDocumentId, PolicyInputSetId, PolicySyntheticTargetId,
        PolicyVersionId, ScanSessionId, StorageRootId, TicketId, UseLeaseId, WorkerId,
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
        REMUX_CONTAINER_MKV, RemuxTrackGroup, is_font_attachment_mime_type,
        is_supported_remux_container,
    };
}

pub mod storage {
    pub use crate::taxonomy::storage::{
        ProviderLocator, ProviderRelativeLocator, StorageProviderKind, StorageRootState,
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
        NvidiaVideoDecode, SoftwareVideoDecode, TRANSCODE_VIDEO_CODEC,
        TRANSCODE_VIDEO_CODEC_ALIAS_H265, TRANSCODE_VIDEO_CODEC_AV1, TRANSCODE_VIDEO_CODEC_H264,
        TRANSCODE_VIDEO_CONTAINER, TRANSCODE_VIDEO_CONTAINER_MP4, TRANSCODE_VIDEO_PROFILE,
        TranscodeVideoProfile, VaapiVideoDecode, VideoDecodeMode, VideoToolboxVideoDecode,
        canonical_video_codec, expected_output_pixel_format, is_supported_transcode_video_codec,
        is_supported_transcode_video_container, normalize_codec_token,
        validate_profile_against_descriptor,
    };
}

pub mod version {
    pub use crate::runtime::version::VersionInfo;
}

pub use artifact_access_declaration::{
    ArtifactAccessDeclaration, ArtifactAccessEntry, ArtifactAccessRight, ArtifactAccessTarget,
    ExistingArtifactAccess, FileLocationAccess, PlannedArtifactAccess, StorageRootAccess,
};
pub use artifact_access_mode::ArtifactAccessMode;
pub use clock::{Clock, SystemClock, format_iso8601};
pub use config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
pub use encoder_caps::{
    EncoderDescriptor, NVIDIA_VIDEO_DECODERS, PresetDomain, QualityDomain, VAAPI_VIDEO_DECODERS,
    VideoEncoderBackend, encoder_descriptor, nvidia_decoder_for_video_codec,
    vaapi_video_decode_codec, video_pixel_format_depth,
};
pub use error::{ErrorCode, VoomError};
pub use failure::{FailureClass, FailureRetryClass};
pub use ids::{
    ArtifactHandleId, ArtifactLocationId, BackupId, BundleId, CommitId, EventId, EvidenceId,
    ExternalPathMappingId, ExternalSystemId, ExternalSystemLinkId, FileAssetId, FileLocationId,
    FileVersionId, IssueId, JobId, LeaseId, LibraryId, LibraryRootId, MediaSnapshotId,
    MediaVariantId, MediaWorkId, NodeId, NodeIncarnationId, PolicyDocumentId, PolicyInputSetId,
    PolicySyntheticTargetId, PolicyVersionId, ScanSessionId, StorageRootId, TicketId, UseLeaseId,
    WorkerId,
};
pub use issue::{IssuePriority, IssueSeverity};
pub use operation_kind::OperationKind;
pub use remux::{
    REMUX_CONTAINER_MKV, RemuxTrackGroup, is_font_attachment_mime_type,
    is_supported_remux_container,
};
pub use storage::{
    ProviderLocator, ProviderRelativeLocator, StorageProviderKind, StorageRootState,
};
pub use taxonomy::execution_vocab::{
    NodeIncarnationEndReason, NodeIncarnationStatus, NodeKind, NodeStatus, WorkerKind, WorkerStatus,
};
pub use taxonomy::scan::{ScanSessionStatus, ScanTerminalReason};
pub use ticket_operation::TicketOperation;
pub use transcode_video_profile::{
    NvidiaVideoDecode, SoftwareVideoDecode, TRANSCODE_VIDEO_CODEC,
    TRANSCODE_VIDEO_CODEC_ALIAS_H265, TRANSCODE_VIDEO_CODEC_AV1, TRANSCODE_VIDEO_CODEC_H264,
    TRANSCODE_VIDEO_CONTAINER, TRANSCODE_VIDEO_CONTAINER_MP4, TRANSCODE_VIDEO_PROFILE,
    TranscodeVideoProfile, VaapiVideoDecode, VideoDecodeMode, VideoToolboxVideoDecode,
    canonical_video_codec, expected_output_pixel_format, is_supported_transcode_video_codec,
    is_supported_transcode_video_container, normalize_codec_token,
    validate_profile_against_descriptor,
};
pub use version::VersionInfo;

/// Worker-protocol wire version consumed by `voom-worker-protocol`'s handshake
/// and middleware.
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
pub const PROTOCOL_VERSION: u32 = 2;
