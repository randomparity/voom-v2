#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "conformance tests favor direct fixture assertions"
    )
)]
//! Black-box protocol conformance harness for VOOM worker binaries.
//!
//! The harness launches workers out of process, drives typed and raw-wire
//! suites through `voom-worker-protocol`, and reports each named check in a
//! `SuiteResult`. Manifests and negative fixtures describe the binaries and
//! protocol failures under test; worker implementations remain outside this
//! crate.

pub mod failure_taxonomy;
pub mod harness;
pub mod manifest;
pub mod negative_fixture;
pub mod raw_wire_suite;
pub mod typed_suite;

pub use harness::{Harness, SuiteResult, WorkerLaunch};
