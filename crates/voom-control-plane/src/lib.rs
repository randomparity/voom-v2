#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! App-services layer: wraps voom-store and exposes commands consumed by API/CLI.
//!
//! The `cases` submodule hosts the M1 use-case methods. Every method that
//! mutates durable state composes the matching repo `_in_tx` call with
//! `EventRepo::append_in_tx` inside one `pool.begin()` so the row write
//! and its event row share a transaction.

use std::sync::Arc;

use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::{Clock, ErrorCode, SystemClock, VoomError};
use voom_store::repo::{
    artifacts::SqliteArtifactRepo, events::SqliteEventRepo, jobs::SqliteJobRepo,
    leases::SqliteLeaseRepo, tickets::SqliteTicketRepo, workers::SqliteWorkerRepo,
};
use voom_store::{SchemaState, connect, probe_schema};

pub mod cases;

#[derive(Clone)]
pub struct ControlPlane {
    pool: SqlitePool,
    clock: Arc<dyn Clock>,
    pub(crate) events: SqliteEventRepo,
    pub(crate) jobs: SqliteJobRepo,
    pub(crate) tickets: SqliteTicketRepo,
    pub(crate) workers: SqliteWorkerRepo,
    pub(crate) leases: SqliteLeaseRepo,
    pub(crate) artifacts: SqliteArtifactRepo,
}

impl std::fmt::Debug for ControlPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `dyn Clock` does not require Debug; surface a sentinel rather
        // than widening the trait bound (which would force every concrete
        // Clock implementor — including test fakes — to derive Debug).
        f.debug_struct("ControlPlane")
            .field("pool", &self.pool)
            .field("clock", &"<dyn Clock>")
            .field("events", &self.events)
            .field("jobs", &self.jobs)
            .field("tickets", &self.tickets)
            .field("workers", &self.workers)
            .field("leases", &self.leases)
            .field("artifacts", &self.artifacts)
            .finish()
    }
}

impl ControlPlane {
    /// Open an existing database. **Never creates files or directories** — if
    /// the DB doesn't exist, returns `DB_UNREACHABLE`. The CLI's `init` command
    /// is the only path that creates databases, and it calls
    /// `voom_store::init(url)` directly without going through `ControlPlane`.
    ///
    /// This Sprint 0 surface intentionally does NOT gate on `SchemaState::Current`
    /// so the diagnostic flow (`health()` on a non-Current DB) continues to
    /// work. Callers that intend to invoke use-case writes must use
    /// `open_with_pool`, which enforces the Current invariant.
    ///
    /// # Errors
    /// Returns `VoomError::Database` if the pool cannot be opened.
    pub async fn open(database_url: &str) -> Result<Self, VoomError> {
        let pool = connect(database_url).await?;
        Ok(Self::new_unchecked(pool, Arc::new(SystemClock)))
    }

    /// Wrap an already-connected pool with the supplied clock. The DB MUST
    /// already be at the current schema (use `voom_store::init` on first boot);
    /// any other state is rejected with `VoomError::Migration`. Use-case
    /// methods on `ControlPlane` assume the full M1 schema is present.
    ///
    /// # Errors
    /// Returns `VoomError::Migration` if the schema probe is not `Current`,
    /// or whatever error `probe_schema` itself produces.
    pub async fn open_with_pool(
        pool: SqlitePool,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, VoomError> {
        let probe = probe_schema(&pool).await?;
        if !matches!(probe, SchemaState::Current { .. }) {
            return Err(VoomError::Migration(format!(
                "ControlPlane requires a Current schema; got {probe:?}"
            )));
        }
        Ok(Self::new_unchecked(pool, clock))
    }

    fn new_unchecked(pool: SqlitePool, clock: Arc<dyn Clock>) -> Self {
        Self {
            events: SqliteEventRepo::new(pool.clone()),
            jobs: SqliteJobRepo::new(pool.clone()),
            tickets: SqliteTicketRepo::new(pool.clone()),
            workers: SqliteWorkerRepo::new(pool.clone()),
            leases: SqliteLeaseRepo::new(pool.clone()),
            artifacts: SqliteArtifactRepo::new(pool.clone()),
            pool,
            clock,
        }
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

    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
    #[must_use]
    pub fn clock(&self) -> &dyn Clock {
        &*self.clock
    }
    #[must_use]
    pub fn events(&self) -> &SqliteEventRepo {
        &self.events
    }
    #[must_use]
    pub fn jobs(&self) -> &SqliteJobRepo {
        &self.jobs
    }
    #[must_use]
    pub fn tickets(&self) -> &SqliteTicketRepo {
        &self.tickets
    }
    #[must_use]
    pub fn workers(&self) -> &SqliteWorkerRepo {
        &self.workers
    }
    #[must_use]
    pub fn leases(&self) -> &SqliteLeaseRepo {
        &self.leases
    }
    #[must_use]
    pub fn artifacts(&self) -> &SqliteArtifactRepo {
        &self.artifacts
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
