//! `SqliteLibraryRepo` — durable library and library-root configuration
//! (Sprint 17, T11). Operator config for what a future daemon may observe.
//! Shape and rationale: `docs/adr/0027-library-root-and-scan-configuration.md`.

use sqlx::{Sqlite, SqlitePool, Transaction};
use voom_core::VoomError;

use super::Repository;

pub mod libraries;
pub mod library_roots;

#[derive(Debug, Clone)]
pub struct SqliteLibraryRepo {
    pool: SqlitePool,
}

impl SqliteLibraryRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteLibraryRepo {}

async fn begin(pool: &SqlitePool) -> Result<Transaction<'static, Sqlite>, VoomError> {
    pool.begin()
        .await
        .map_err(|e| VoomError::database_context("begin", e))
}

async fn commit(tx: Transaction<'_, Sqlite>) -> Result<(), VoomError> {
    tx.commit()
        .await
        .map_err(|e| VoomError::database_context("commit", e))
}
