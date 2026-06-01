//! FFmpeg-backed worker operations for video transcode requests.
//!
//! The crate owns local ffmpeg/ffprobe preflight, media fact observation, and
//! worker-protocol handlers for transcode-video dispatch.

#![cfg_attr(
    test,
    expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests use direct unwraps and panics for assertion plumbing"
    )
)]

pub mod ffmpeg;
pub mod handler;
pub mod observe;
pub mod preflight;

pub use ffmpeg::{
    ALL_VIDEO_ENCODERS, DEFAULT_PROCESS_TIMEOUT, FfmpegConfig, FfmpegError, run_ffmpeg_transcode,
};
pub use handler::{
    TranscodeVideoError, handle_operation, handle_transcode_video, operation_handler,
};
pub use observe::{ObserveError, observe_file_facts};
pub use preflight::{
    FFMPEG_BIN_ENV, FFPROBE_BIN_ENV, FFmpegPreflightError, FfmpegPreflight,
    preflight_from_process_env, preflight_with_paths,
};
