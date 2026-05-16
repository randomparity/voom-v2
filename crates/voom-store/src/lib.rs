#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Storage layer: SQLite pool, migrations, repositories.

pub mod pool;

pub use pool::{connect, connect_or_create};
