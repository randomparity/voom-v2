//! MKVToolNix-backed worker operations for remux requests.
//!
//! The crate owns local mkvmerge preflight, media fact observation, and
//! worker-protocol handlers for remux dispatch.

#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests use direct unwraps and panics for assertion plumbing"
    )
)]

pub mod handler;
pub mod mkvmerge;
pub mod observe;
pub mod preflight;

pub use handler::{MkvtoolnixWorkerError, handle_operation, handle_remux, operation_handler};
pub use mkvmerge::{
    DEFAULT_PROCESS_TIMEOUT, MkvmergeTrackMapping, build_mkvmerge_args, run_mkvmerge_remux,
    track_mapping_from_identify,
};
pub use observe::{ObserveError, observe_file_facts};
pub use preflight::{
    MKVMERGE_BIN_ENV, MkvmergeConfig, MkvmergeVersion, MkvtoolnixError, parse_mkvmerge_version,
    preflight_from_process_env, preflight_mkvmerge,
};
