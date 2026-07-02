#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Versioned HTTP/JSON worker protocol for VOOM.
//!
//! Public API surface is fixed in `docs/superpowers/specs/2026-05-19-voom-sprint-2-phase-1-design.md`.
//! Sub-modules land incrementally in the Phase 1 commit sequence; this
//! commit replaces the Sprint 0 placeholder with the empty real
//! module skeleton so subsequent commits can fill it without
//! disturbing the build.

pub mod encoder_caps;
pub mod http;
pub mod low_level;
mod operations;
pub mod startup;
pub mod transport;
mod wire;

pub use encoder_caps::{EncoderDescriptor, PresetDomain, encoder_descriptor};
pub use http::{
    HttpClient, HttpServer, OperationDispatch, OperationFuture, OperationHandler, RoutePolicy,
    route_policy,
};
pub use operations::audio::{
    AUDIO_PROFILE_DEFAULT, AudioDispositionFact, AudioExpectedFacts, AudioObservedFacts,
    AudioOutputStreamFact, AudioStreamRef, EXTRACT_AUDIO_CODEC, EXTRACT_AUDIO_CONTAINER,
    ExtractAudioInput, ExtractAudioOutput, ExtractAudioRequest, ExtractAudioResult,
    ExtractAudioStatus, TRANSCODE_AUDIO_CODEC_AAC, TRANSCODE_AUDIO_CODEC_EAC3,
    TRANSCODE_AUDIO_CODEC_OPUS, TRANSCODE_AUDIO_CONTAINER, TranscodeAudioInput,
    TranscodeAudioOutput, TranscodeAudioRequest, TranscodeAudioResult, TranscodeAudioSelection,
    TranscodeAudioSettings, TranscodeAudioStatus, audio_target_bitrate_kbps_per_channel,
    is_supported_transcode_audio_codec,
};
pub use operations::probe_file::{
    ExpectedFileFacts, ObservedFileFacts, ProbeFileRequest, ProbeFileResult, ProbeFileStatus,
};
pub use operations::remux::{
    REMUX_CONTAINER_MKV, RemuxExpectedFacts, RemuxInput, RemuxObservedFacts, RemuxOutput,
    RemuxRequest, RemuxResult, RemuxSelection, RemuxStatus, RemuxStreamRef, RemuxTrackGroup,
    is_supported_remux_container,
};
pub use operations::transcode_video::{
    TRANSCODE_VIDEO_CODEC, TRANSCODE_VIDEO_CODEC_ALIAS_H265, TRANSCODE_VIDEO_CODEC_AV1,
    TRANSCODE_VIDEO_CONTAINER, TRANSCODE_VIDEO_CONTAINER_MP4, TRANSCODE_VIDEO_PROFILE,
    TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoObservedFacts,
    TranscodeVideoOutput, TranscodeVideoProfile, TranscodeVideoRequest, TranscodeVideoResult,
    TranscodeVideoStatus, canonical_video_codec, is_supported_transcode_video_codec,
    is_supported_transcode_video_container, normalize_codec_token,
    validate_profile_against_descriptor,
};
pub use operations::verify_artifact::{
    VerifyArtifactExpectedFacts, VerifyArtifactObservedFacts, VerifyArtifactRequest,
    VerifyArtifactResult, VerifyArtifactStatus,
};
pub use startup::{
    DEFAULT_WORKER_BIND, WORKER_BIND_ENV, WORKER_EPOCH_ENV, WORKER_ID_ENV, WORKER_SECRET_ENV,
    WorkerStartupError, load_worker_bind_addr_from_env, load_worker_credentials_from_env,
    serve_worker_http,
};
pub use transport::{ClientHandle, DispatchStream, NdjsonStream, ServerHandle, ServerRunning};
pub use voom_core::OperationKind;
pub use wire::credentials::{PresentedCredentials, WorkerCredentials, validate_credentials};
pub use wire::envelope::{
    OperationRequest, OperationResponse, PercentBps, ProgressFrame, ProtocolError,
};
pub use wire::handshake::{HandshakeRequest, HandshakeResponse, negotiate};
pub use wire::ndjson::{NdjsonOutcome, NdjsonReader, NdjsonWriter};
