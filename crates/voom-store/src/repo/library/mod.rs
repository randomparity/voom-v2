//! `SqliteLibraryRepo` — durable library and library-root configuration
//! (Sprint 17, T11). Operator config for what a future daemon may observe.
//! Shape and rationale: `docs/adr/0027-library-root-and-scan-configuration.md`.

use sqlx::{Sqlite, SqlitePool, Transaction};
use voom_core::VoomError;

use super::Repository;

/// Generate `as_str`/`parse` for a CHECK-vocabulary enum, mirroring the column
/// exactly. Shared by every enum in this module (`libraries`/`library_roots`).
macro_rules! str_enum {
    ($ty:ty, $col:literal, { $($variant:ident => $s:literal),+ $(,)? }) => {
        impl $ty {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $s),+
                }
            }

            /// Parse a wire/DB value.
            ///
            /// # Errors
            /// Returns a database error for a value outside the CHECK vocabulary.
            pub fn parse(s: &str) -> Result<Self, voom_core::VoomError> {
                match s {
                    $($s => Ok(Self::$variant),)+
                    other => Err(voom_core::VoomError::database(format!(
                        "{} {other:?} not in vocab", $col
                    ))),
                }
            }
        }
    };
}

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
