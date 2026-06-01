//! FFprobe-backed worker operations for media metadata inspection.
//!
//! The crate runs ffprobe, normalizes its JSON output, observes input file
//! facts, and exposes worker-protocol handlers for identify-media dispatch.

pub mod ffprobe;
pub mod normalize;
pub mod observe;

pub use ffprobe::{
    FFPROBE_BIN_ENV, FfprobeConfig, FfprobeError, handle_operation, operation_handler_with_config,
    run_ffprobe_json,
};
pub use normalize::{WorkerError, normalize_ffprobe_json};
pub use observe::observe_file_facts;
