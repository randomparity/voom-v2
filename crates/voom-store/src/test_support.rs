//! Test-only helpers shared across the workspace. Gated behind the
//! `test-support` feature so production crates cannot reach this module.
//!
//! ### Why no centralized lint preamble
//!
//! Integration test files (`crates/*/tests/*.rs`) each ship a 4-line
//! `#![expect(clippy::unwrap_used, clippy::panic, ...)]` preamble. Cargo's
//! workspace `[lints]` table is flat — it does not support per-`cfg` filters
//! to relax a deny only inside `cfg(test)` — so there is no clean recipe to
//! hoist the preamble. A proc-macro attribute would work but is overkill
//! for unchanging boilerplate. The lib-side files already use
//! `#![cfg_attr(test, expect(...))]` to keep production code clean; the
//! integration-test duplication is the load-bearing minimum.
//!
//! ### Why callers manage tempfile lifetime
//!
//! `tempfile::NamedTempFile` is intentionally not used inside these helpers
//! — that would force `tempfile` into voom-store's production dependency
//! graph (cargo unifies features across the workspace, so a dev-dep enabling
//! `test-support` propagates the dep tree). Callers create the temp file in
//! their own dev-deps and pass the path in. The boilerplate saved is the
//! 4-line `format!` / `init` / `connect` ritual.

use std::path::Path;

use serde_json::Value as JsonValue;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::{JobId, VoomError};

use crate::init::init;
use crate::pool::connect;
use crate::repo::tickets::{NewTicket, SqliteTicketRepo, Ticket, TicketRepo};
use crate::repo::workers::{NewWorker, SqliteWorkerRepo, Worker, WorkerKind, WorkerRepo};

/// Shared default timestamp for builder fixtures and tests. Keyed on
/// `OffsetDateTime::UNIX_EPOCH` so snapshot diffs are stable across runs.
/// Hoisted here so the 6+ `const T0` declarations across the test suite
/// import a single source of truth instead of redeclaring it.
pub const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

/// Format a filesystem path as a `sqlite://` URL. Centralizes the
/// `format!("sqlite://{}", path.display())` literal that otherwise appears
/// 20+ times across the test suite.
#[must_use]
pub fn sqlite_url_for(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

/// Run `init` against `path` and return a connected pool. Callers own the
/// path (typically backed by `tempfile::NamedTempFile`) so the temp file's
/// lifetime is explicit at the test site.
///
/// # Errors
///
/// Returns a `VoomError` if init or connect fails.
pub async fn fresh_initialized_pool_at(path: &Path) -> Result<SqlitePool, VoomError> {
    let url = sqlite_url_for(path);
    init(&url).await?;
    connect(&url).await
}

/// Insert a synthetic row into `_sqlx_migrations` so callers can simulate
/// `Dirty`, `TooNew`, or other post-init states without depending on
/// MIGRATOR's actual contents.
///
/// `version` is the migration version (use a number outside MIGRATOR's range
/// — e.g. 99999 — to trigger `TooNew`); `success` controls the success flag
/// (use `false` to trigger `Dirty`).
///
/// # Errors
///
/// Returns the underlying `sqlx::Error` if the insert fails.
pub async fn insert_synthetic_migration(
    pool: &SqlitePool,
    version: i64,
    success: bool,
) -> Result<(), sqlx::Error> {
    let success_int = i32::from(success);
    sqlx::query(
        "INSERT INTO _sqlx_migrations \
         (version, description, installed_on, success, checksum, execution_time) \
         VALUES (?, 'synthetic', strftime('%s','now'), ?, X'00', 0)",
    )
    .bind(version)
    .bind(success_int)
    .execute(pool)
    .await?;
    Ok(())
}

// -- builders ---------------------------------------------------------------
//
// Deterministic fixtures for repo tests. Each builder ships with sane defaults
// keyed on `OffsetDateTime::UNIX_EPOCH` so snapshot diffs are stable across
// runs. Builders call the BARE repo methods (not `_in_tx`) because they own
// their own transaction boundary; tests that need event emission go through
// the `ControlPlane` use-cases (Task 14) directly.

#[derive(Debug, Clone)]
pub struct TicketBuilder {
    job_id: Option<JobId>,
    kind: String,
    priority: i64,
    payload: JsonValue,
    max_attempts: u32,
    created_at: OffsetDateTime,
}

impl Default for TicketBuilder {
    fn default() -> Self {
        Self {
            job_id: None,
            kind: "test.noop".to_owned(),
            priority: 0,
            payload: serde_json::json!({}),
            max_attempts: 1,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }
}

impl TicketBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_kind(mut self, k: impl Into<String>) -> Self {
        self.kind = k.into();
        self
    }

    #[must_use]
    pub fn with_priority(mut self, p: i64) -> Self {
        self.priority = p;
        self
    }

    #[must_use]
    pub fn with_max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n;
        self
    }

    #[must_use]
    pub fn with_payload(mut self, v: JsonValue) -> Self {
        self.payload = v;
        self
    }

    #[must_use]
    pub fn with_created_at(mut self, t: OffsetDateTime) -> Self {
        self.created_at = t;
        self
    }

    #[must_use]
    pub fn with_job(mut self, j: JobId) -> Self {
        self.job_id = Some(j);
        self
    }

    /// Build via the bare `create` (opens its own tx).
    ///
    /// # Errors
    ///
    /// Propagates `TicketRepo::create` errors.
    pub async fn build(self, repo: &SqliteTicketRepo) -> Result<Ticket, VoomError> {
        repo.create(NewTicket {
            job_id: self.job_id,
            kind: self.kind,
            priority: self.priority,
            payload: self.payload,
            max_attempts: self.max_attempts,
            created_at: self.created_at,
        })
        .await
    }
}

#[derive(Debug, Clone)]
pub struct WorkerBuilder {
    name: String,
    kind: WorkerKind,
    registered_at: OffsetDateTime,
}

impl Default for WorkerBuilder {
    fn default() -> Self {
        Self {
            name: "test-worker".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
        }
    }
}

impl WorkerBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_name(mut self, n: impl Into<String>) -> Self {
        self.name = n.into();
        self
    }

    #[must_use]
    pub fn with_kind(mut self, k: WorkerKind) -> Self {
        self.kind = k;
        self
    }

    #[must_use]
    pub fn with_registered_at(mut self, t: OffsetDateTime) -> Self {
        self.registered_at = t;
        self
    }

    /// Build via the bare `register` (opens its own tx).
    ///
    /// # Errors
    ///
    /// Propagates `WorkerRepo::register` errors.
    pub async fn build(self, repo: &SqliteWorkerRepo) -> Result<Worker, VoomError> {
        repo.register(NewWorker {
            name: self.name,
            kind: self.kind,
            registered_at: self.registered_at,
        })
        .await
    }
}
