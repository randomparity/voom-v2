#![cfg_attr(
    test,
    expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests use direct unwraps and panics for assertion plumbing"
    )
)]

pub mod handler;
pub mod observe;

pub use handler::{VerifyArtifactError, handle_operation, operation_handler};
pub use observe::observe_file_facts;
