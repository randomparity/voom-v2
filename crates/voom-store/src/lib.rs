#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::too_many_lines,
        reason = "tests favor unwrap/expect/panic over plumbing Result<()> through every \
                  assertion; data-table tests pairing many enum variants exceed the line cap"
    )
)]
//! Storage layer: `SQLite` pool, migrations, repositories.
//!
//! The embedded migration registry is deliberately absent from the default
//! public API:
//!
//! ```compile_fail,E0432
//! use voom_store::MIGRATOR;
//! ```
//!
//! ```compile_fail,E0603
//! use voom_store::migrator;
//! ```

pub mod init;
mod migrator;
pub mod pool;
pub mod repo;
pub mod schema;
pub mod tx;

#[cfg(any(test, feature = "test"))]
pub mod test_support;

pub use init::{InitReport, init};
pub use pool::connect;
pub use schema::{SchemaState, expected_migrations, probe_schema};
