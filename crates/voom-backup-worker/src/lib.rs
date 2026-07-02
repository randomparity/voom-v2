//! Backup worker operations.
//!
//! The crate copies a source file to a destination path on the local
//! filesystem (the V1 backup target), computing size and a BLAKE3 checksum,
//! fsyncing the copy for durability, and exposes the worker-protocol handler
//! for the `back_up_file` operation.

#![cfg_attr(
    test,
    expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests use direct unwraps and panics for assertion plumbing"
    )
)]

pub mod backup;
pub mod handler;

pub use backup::{BackUpOutcome, BackupIoError, back_up_file};
pub use handler::{BackUpFileError, handle_operation, operation_handler};
