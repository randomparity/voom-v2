#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]

pub mod preflight;

pub use preflight::{
    FFMPEG_BIN_ENV, FFPROBE_BIN_ENV, FFmpegPreflightError, FfmpegPreflight,
    preflight_from_process_env, preflight_with_paths,
};
