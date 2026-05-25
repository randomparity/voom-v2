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
pub use transcode_video::{
    TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoObservedFacts,
    TranscodeVideoOutput, TranscodeVideoProfile, TranscodeVideoRequest, TranscodeVideoResult,
    TranscodeVideoStatus,
};
pub use transport::{ClientHandle, DispatchStream, NdjsonStream, ServerHandle, ServerRunning};
pub use verify_artifact::{
    VerifyArtifactExpectedFacts, VerifyArtifactObservedFacts, VerifyArtifactRequest,
    VerifyArtifactResult, VerifyArtifactStatus,
};
