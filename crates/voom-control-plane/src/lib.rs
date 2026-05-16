#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! App-services layer: wraps voom-store and exposes commands consumed by API/CLI.

use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::{ErrorCode, VoomError};
use voom_store::{SchemaState, connect, probe_schema};

#[derive(Debug, Clone)]
pub struct ControlPlane {
    pool: SqlitePool,
}

impl ControlPlane {
    /// Open an existing database. **Never creates files or directories** — if
    /// the DB doesn't exist, returns `DB_UNREACHABLE`. The CLI's `init` command
    /// is the only path that creates databases, and it calls
    /// `voom_store::init(url)` directly without going through `ControlPlane`.
    pub async fn open(database_url: &str) -> Result<Self, VoomError> {
        let pool = connect(database_url).await?;
        Ok(Self { pool })
    }

    /// Read-only health snapshot.
    pub async fn health(&self) -> Result<HealthSnapshot, VoomError> {
        let schema = probe_schema(&self.pool).await?;
        Ok(match schema {
            SchemaState::Uninitialized => HealthSnapshot::Uninitialized,
            SchemaState::Partial { applied, expected } => {
                HealthSnapshot::Partial { applied, expected }
            }
            SchemaState::Current {
                migration_count,
                schema_init_at,
            } => HealthSnapshot::Current {
                migration_count,
                schema_init_at,
            },
            SchemaState::TooNew { applied, expected } => {
                HealthSnapshot::TooNew { applied, expected }
            }
            SchemaState::Dirty {
                failed_version,
                applied,
                expected,
            } => HealthSnapshot::Dirty {
                failed_version,
                applied,
                expected,
            },
        })
    }
}

/// State-tagged health snapshot. The ADT shape replaces the previous
/// flat-struct-with-Options so the type system enforces which fields are
/// available in each state — no more `Option<u32>` debug-printed in
/// operator-facing error messages as `Some(0)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthSnapshot {
    /// `_sqlx_migrations` table absent.
    Uninitialized,
    /// Fewer migrations applied than this binary ships. Safe to rerun
    /// `voom init`.
    Partial { applied: u32, expected: u32 },
    /// Exactly as many migrations applied as this binary ships AND every
    /// applied version is known to the embedded MIGRATOR.
    Current {
        migration_count: u32,
        schema_init_at: OffsetDateTime,
    },
    /// At least one applied migration version is not in the embedded MIGRATOR.
    TooNew { applied: u32, expected: u32 },
    /// One or more migration rows are recorded as `success=0`; manual recovery
    /// required before further migrations can run.
    Dirty {
        failed_version: i64,
        applied: u32,
        expected: u32,
    },
}

/// Operator-facing diagnostic triple for a non-Current health snapshot.
/// Surfaces (API, CLI) wrap this into their own envelope format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthDiagnostic {
    pub code: ErrorCode,
    pub message: String,
    pub hint: Option<String>,
}

impl HealthSnapshot {
    /// Map a non-Current snapshot to its diagnostic triple. Returns `None`
    /// for `Current` — that state has no error to surface.
    ///
    /// This is the single source of truth for the error code, message, and
    /// hint for every non-healthy state. Both `voom-api` and `voom-cli` call
    /// it so their prose cannot drift apart.
    #[must_use]
    pub fn diagnostic(&self) -> Option<HealthDiagnostic> {
        match self {
            Self::Current { .. } => None,
            Self::Uninitialized => Some(HealthDiagnostic {
                code: ErrorCode::DbUninitialized,
                message: "database has no migrations applied".to_owned(),
                hint: Some("Run `voom init` on the host that owns this database".to_owned()),
            }),
            Self::Partial { applied, expected } => Some(HealthDiagnostic {
                code: ErrorCode::DbPartialSchema,
                message: format!(
                    "database partially migrated (applied={applied}, expected={expected})"
                ),
                hint: Some("Run `voom init` against the current binary".to_owned()),
            }),
            Self::TooNew { applied, expected } => Some(HealthDiagnostic {
                code: ErrorCode::DbSchemaTooNew,
                message: format!(
                    "database has migrations this binary does not know about \
                     (applied={applied}, expected={expected})"
                ),
                hint: Some("Upgrade the server binary or roll the database back".to_owned()),
            }),
            Self::Dirty {
                failed_version,
                applied,
                expected,
            } => Some(HealthDiagnostic {
                code: ErrorCode::DbDirtyMigration,
                message: format!(
                    "a previous migration left the schema in a dirty (failed) state \
                     (failed_version={failed_version}, applied={applied}, expected={expected}); \
                     sqlx will not run further migrations until the dirty row is resolved"
                ),
                hint: Some(
                    "Manual recovery required: remove the failed row from \
                     _sqlx_migrations (e.g. DELETE FROM _sqlx_migrations WHERE \
                     version = <failed_version>) or restore from backup. Do NOT \
                     just re-run voom init — it will fail the same way."
                        .to_owned(),
                ),
            }),
        }
    }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
