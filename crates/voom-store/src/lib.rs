#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Storage layer: SQLite pool, migrations, repositories.

pub mod init;
pub mod migrator;
pub mod pool;
pub mod schema;

pub use init::{InitReport, init};
pub use migrator::MIGRATOR;
pub use pool::{connect, connect_or_create};
pub use schema::{SchemaState, expected_migrations, probe_schema};

// `init_on` is deliberately NOT re-exported. It lives at
// `voom_store::init::init_on` and is gated behind the `test-support` feature
// so production crates cannot reach the pool-injection migration path.
