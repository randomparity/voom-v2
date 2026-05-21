#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "conformance tests favor direct fixture assertions"
    )
)]
//! Black-box protocol conformance harness for VOOM worker binaries.
//!
//! Phase 1 commit 10 ships the public `Harness` API and the
//! `WorkerLaunch` handle (process child + bound addr + stdin pipe +
//! credentials). Phase 1 commit 12 will fill in the typed and
//! raw-wire suites; commit 10's `run_*` methods return empty
//! `SuiteResult`s as scaffolding so consumers (the echo-worker
//! smoke test in commit 11) can wire against the public API today.

pub mod harness;
pub mod manifest;
pub mod raw_wire_suite;
pub mod typed_suite;

pub use harness::{Harness, SuiteResult, WorkerLaunch};
