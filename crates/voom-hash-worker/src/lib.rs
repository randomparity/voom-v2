#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests favor unwrap/panic! over plumbing Result<()> through every assertion"
    )
)]
//! BLAKE3 hashing worker operations for storage-owner node execution.
//!
//! The crate resolves a root-relative locator to an open file via a
//! component-wise `O_NOFOLLOW` descent from the canonical storage root,
//! hashes the file bytes with BLAKE3, and exposes a worker-protocol handler
//! for `HashFile` dispatch. One dispatch hashes exactly one file: sidecars
//! arrive as separate `HashFile` dispatches correlated by the agent pump.

pub mod descent;
pub mod handler;
pub mod hash;

pub use descent::{DescentError, ResolvedFile, resolve_in_root};
pub use handler::operation_handler;
pub use hash::{
    HASH_CHUNK_BYTES, HashWorkerError, StatFacts, assert_stable, file_key, hash_file_in_root,
    read_hash, stat_facts,
};
