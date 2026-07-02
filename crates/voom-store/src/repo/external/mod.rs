//! `SqliteExternalSystemRepo` — durable external-system registration, health,
//! path mappings, and links (Sprint 17, T15). Operator state a future daemon's
//! health/sync loops (Sprint 20) will read. Shape and rationale:
//! `docs/adr/0029-external-system-registration-health-and-sync.md`.
//!
//! One repo spans the three migration-0004 tables (`external_systems`,
//! `external_path_mappings`, `external_system_links`). Enum columns are mirrored
//! exactly by `str_enum!`-generated `as_str`/`parse`, so an out-of-vocabulary
//! value is unrepresentable on write and fail-loud on read.

use sqlx::SqlitePool;

use super::Repository;

/// Generate `as_str`/`parse` for a CHECK-vocabulary enum, mirroring the column
/// exactly. Shared by every enum in this module.
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

pub mod links;
pub mod path_mappings;
pub mod systems;

#[derive(Debug, Clone)]
pub struct SqliteExternalSystemRepo {
    pool: SqlitePool,
}

impl SqliteExternalSystemRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteExternalSystemRepo {}
