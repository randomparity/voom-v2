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

pub mod init;
pub mod migrator;
pub mod pool;
pub mod repo;
pub mod schema;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use init::{InitReport, init};
pub use migrator::MIGRATOR;
pub use pool::{connect, connect_or_create};
pub use schema::{SchemaState, expected_migrations, probe_schema};
