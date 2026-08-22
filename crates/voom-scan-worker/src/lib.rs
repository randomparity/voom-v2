//! Owner-node library scanning worker (ADR 0077).
//!
//! The worker walks one storage root metadata-only, classifies primary media
//! and sidecars, and answers `scan_library` dispatches with candidate
//! progress frames plus a terminal [`ScanLibraryResult`]. Pure classification
//! lives in [`discover`], the filesystem walk in [`walk`], and the
//! worker-protocol surface in [`handler`].

pub mod discover;
pub mod handler;
pub mod walk;

pub use discover::{SUPPORTED_EXTENSIONS, SidecarKind};
pub use handler::{ScanWorkerError, batch_candidate_payloads, operation_handler};
pub use walk::{RootUnavailable, WalkCandidate, WalkFile, WalkOutcome, scan_root};
