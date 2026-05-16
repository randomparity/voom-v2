#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Storage layer: SQLite pool, migrations, repositories.

pub mod migrator;
pub mod pool;
pub mod schema;

pub use migrator::MIGRATOR;
pub use pool::{connect, connect_or_create};
pub use schema::{SchemaState, expected_migrations, probe_schema};
