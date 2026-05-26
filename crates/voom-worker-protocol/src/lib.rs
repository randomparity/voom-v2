#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Versioned HTTP/JSON worker protocol for VOOM Sprint 2.
//!
//! Public API surface is fixed in `docs/superpowers/specs/2026-05-19-voom-sprint-2-phase-1-design.md`.
//! Sub-modules land incrementally in the Phase 1 commit sequence; this
//! commit replaces the Sprint 0 placeholder with the empty real
//! module skeleton so subsequent commits can fill it without
//! disturbing the build.

pub mod credentials;
pub mod envelope;
pub mod handshake;
pub mod http;
pub mod low_level;
pub mod ndjson;
pub mod operation_kind;
pub mod probe_file;
pub mod remux;
pub mod transcode_video;
pub mod transport;
pub mod verify_artifact;

pub use credentials::{PresentedCredentials, WorkerCredentials, validate_credentials};
pub use envelope::{OperationRequest, OperationResponse, PercentBps, ProgressFrame, ProtocolError};
pub use handshake::{HandshakeRequest, HandshakeResponse, negotiate};
pub use http::{
    HttpClient, HttpServer, OperationDispatch, OperationFuture, OperationHandler, RoutePolicy,
    route_policy,
};
pub use ndjson::{NdjsonOutcome, NdjsonReader, NdjsonWriter};
pub use operation_kind::OperationKind;
pub use probe_file::{
    ExpectedFileFacts, ObservedFileFacts, ProbeFileRequest, ProbeFileResult, ProbeFileStatus,
};
pub use remux::{
    REMUX_CONTAINER_MKV, RemuxExpectedFacts, RemuxInput, RemuxObservedFacts, RemuxOutput,
    RemuxRequest, RemuxResult, RemuxSelection, RemuxStatus, RemuxStreamRef, RemuxTrackGroup,
    is_supported_remux_container,
};
pub use transcode_video::{
    TRANSCODE_VIDEO_CODEC, TRANSCODE_VIDEO_CODEC_ALIAS_H265, TRANSCODE_VIDEO_CONTAINER,
    TRANSCODE_VIDEO_PROFILE, TranscodeVideoExpectedFacts, TranscodeVideoInput,
    TranscodeVideoObservedFacts, TranscodeVideoOutput, TranscodeVideoProfile,
    TranscodeVideoRequest, TranscodeVideoResult, TranscodeVideoStatus, is_default_hevc_profile,
    is_supported_transcode_video_codec, is_supported_transcode_video_container,
};
pub use transport::{ClientHandle, DispatchStream, NdjsonStream, ServerHandle, ServerRunning};
pub use verify_artifact::{
    VerifyArtifactExpectedFacts, VerifyArtifactObservedFacts, VerifyArtifactRequest,
    VerifyArtifactResult, VerifyArtifactStatus,
};
