//! Test-only helpers shared across the workspace. This module is available
//! only when the `test` feature is enabled; production targets must not enable
//! that feature.
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
//! ### Why callers manage temporary database lifetime
//!
//! `voom_test_support::TempDatabase` is intentionally not constructed inside
//! these helpers. Callers own it so the private directory outlives every pool
//! and `SQLite` sidecar. Keeping the fixture in the dev-only support crate also
//! avoids adding `tempfile` to voom-store's production dependency graph.

use std::{future::Future, path::Path, pin::Pin};

use serde_json::Value as JsonValue;
use sqlx::{SqliteConnection, SqlitePool};
use time::OffsetDateTime;
use voom_core::{
    JobId, ProviderRelativeLocator, StorageRootId, TicketOperation, VoomError, WorkerId,
};

use crate::init::init;
use crate::migrator::MIGRATOR;
use crate::pool::{connect, connect_or_create};
use crate::repo::execution::tickets::{NewTicket, SqliteTicketRepo, Ticket};
use crate::repo::execution::workers::{
    NewCapability, NewGrant, NewWorker, SqliteWorkerRepo, Worker, WorkerKind,
};

/// Shared default timestamp for builder fixtures and tests. Keyed on
/// `OffsetDateTime::UNIX_EPOCH` so snapshot diffs are stable across runs.
/// Hoisted here so the 6+ `const T0` declarations across the test suite
/// import a single source of truth instead of redeclaring it.
pub const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
pub const TEST_STORAGE_ROOT_ID: StorageRootId = StorageRootId(9_000_001);

/// Seed one active storage root for repository tests whose subject is not root
/// administration. The deliberately high stable ids avoid colliding with rows
/// created through the repositories under test.
pub async fn seed_test_storage_root(pool: &SqlitePool) -> Result<StorageRootId, VoomError> {
    sqlx::query(
        "INSERT OR IGNORE INTO nodes \
         (id, name, kind, status, registered_at, last_seen_at, retired_at, \
          heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata, epoch) \
         VALUES (9000001, 'repository-test-root-owner', 'local', 'active', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', NULL, 60, \
                 'hash', 'hint', '{}', 0)",
    )
    .execute(pool)
    .await
    .map_err(|error| VoomError::database_context("seed test storage-root owner", error))?;
    sqlx::query(
        "INSERT OR IGNORE INTO libraries \
         (id, slug, display_name, media_kind, description, enabled, created_at, updated_at) \
         VALUES (9000001, 'repository-test-root', 'Repository Test Root', 'unknown', NULL, 1, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .map_err(|error| VoomError::database_context("seed test storage-root library", error))?;
    sqlx::query(
        "INSERT OR IGNORE INTO library_roots \
         (id, library_id, owner_node_id, provider_kind, provider_locator, display_locator, \
          state, root_epoch, activation_identity, include_globs, exclude_globs, \
          extension_allowlist, scan_mode, symlink_policy, hidden_file_policy, max_depth, \
          stability_seconds, debounce_seconds, default_output_root_id, default_staging_root_id, \
          default_backup_root_id, enabled, created_at, updated_at) \
         VALUES (9000001, 9000001, 9000001, 'local_filesystem', '/', '/', 'active', 1, \
                 'repository-test-root', '[]', '[]', '[]', 'manual_recursive', 'reject', 'ignore', \
                 NULL, 0, 0, NULL, NULL, NULL, 1, '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .map_err(|error| VoomError::database_context("seed test storage root", error))?;
    Ok(TEST_STORAGE_ROOT_ID)
}

/// Point the shared active test root at an isolated fixture directory.
///
/// # Errors
///
/// Returns a database error if the fixture row cannot be updated.
pub async fn set_test_storage_root_path(pool: &SqlitePool, path: &Path) -> Result<(), VoomError> {
    let locator = path.to_string_lossy();
    sqlx::query(
        "UPDATE library_roots SET provider_locator = ?, display_locator = ?, updated_at = \
         '1970-01-01T00:00:00Z' WHERE id = ?",
    )
    .bind(locator.as_ref())
    .bind(locator.as_ref())
    .bind(i64::try_from(TEST_STORAGE_ROOT_ID.0).map_err(|error| {
        VoomError::database(format!("test storage root id conversion: {error}"))
    })?)
    .execute(pool)
    .await
    .map_err(|error| VoomError::database_context("set test storage-root path", error))?;
    Ok(())
}

/// Convert a historical absolute-path fixture into a valid provider-relative
/// locator without preserving any global-path semantics.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "invalid provider-relative fixture input is a test-author error"
)]
pub fn test_relative_locator(value: &str) -> ProviderRelativeLocator {
    let value = value.trim_matches('/');
    ProviderRelativeLocator::new(if value.is_empty() {
        "fixture".to_owned()
    } else {
        value.to_owned()
    })
    .expect("test relative locator must be valid")
}

/// Format a filesystem path as a `sqlite://` URL. Centralizes the
/// `format!("sqlite://{}", path.display())` literal that otherwise appears
/// 20+ times across the test suite.
#[must_use]
pub fn sqlite_url_for(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

/// Create a database without applying migrations so tests can seed partial or
/// invalid schema states. Production callers must use [`crate::init()`] instead.
///
/// # Errors
///
/// Returns a `VoomError` if the database or its parent directory cannot be
/// created.
pub async fn create_uninitialized_pool(url: &str) -> Result<SqlitePool, VoomError> {
    connect_or_create(url).await
}

/// Run `init` against `path` and return a connected pool. Callers own the
/// path (typically backed by `voom_test_support::TempDatabase`) so the temporary
/// directory's lifetime is explicit at the test site.
///
/// # Errors
///
/// Returns a `VoomError` if init or connect fails.
pub async fn fresh_initialized_pool_at(path: &Path) -> Result<SqlitePool, VoomError> {
    let url = sqlite_url_for(path);
    init(&url).await?;
    let pool = connect(&url).await?;
    seed_test_storage_root(&pool).await?;
    Ok(pool)
}

/// Run a test operation with `SQLite` check constraints disabled on one pooled connection.
///
/// The operation must execute every statement that depends on the bypass through the
/// supplied connection. Dropping the connection returns it to the pool and scopes the
/// connection-local pragma to this operation.
pub async fn with_check_constraints_disabled<T, F>(
    pool: &SqlitePool,
    operation: F,
) -> Result<T, sqlx::Error>
where
    F: for<'connection> FnOnce(
        &'connection mut SqliteConnection,
    ) -> Pin<
        Box<dyn Future<Output = Result<T, sqlx::Error>> + Send + 'connection>,
    >,
{
    let mut connection = pool.acquire().await?;
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&mut *connection)
        .await?;
    let result = operation(&mut connection).await;
    drop(connection);
    result
}

/// Return the embedded migration registry for integration-test schema fixtures.
///
/// Production initialization remains owned by [`crate::init()`].
#[must_use]
pub fn embedded_migrator() -> &'static sqlx::migrate::Migrator {
    &MIGRATOR
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

/// Record matching capabilities and one allow grant for test worker operations.
///
/// # Errors
///
/// Returns the first capability or grant insertion error.
pub async fn record_worker_eligibility(
    workers: &SqliteWorkerRepo,
    worker_id: WorkerId,
    operations: &[TicketOperation],
) -> Result<(), VoomError> {
    for operation in operations {
        workers
            .record_capability(NewCapability {
                worker_id,
                operation: operation.clone(),
                codecs: Vec::new(),
                hardware: Vec::new(),
                artifact_access: Vec::new(),
                extra: serde_json::json!({}),
            })
            .await?;
    }
    workers
        .record_grant(NewGrant {
            worker_id,
            can_execute: operations.to_vec(),
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: serde_json::json!({}),
        })
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
    /// Propagates `SqliteTicketRepo::create` errors.
    pub async fn build(self, repo: &SqliteTicketRepo) -> Result<Ticket, VoomError> {
        repo.create(NewTicket {
            job_id: self.job_id,
            kind: TicketOperation::new(self.kind)?,
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
    /// Propagates `SqliteWorkerRepo::register` errors.
    pub async fn build(self, repo: &SqliteWorkerRepo) -> Result<Worker, VoomError> {
        repo.register(NewWorker {
            name: self.name,
            kind: self.kind,
            registered_at: self.registered_at,
            node_id: None,
        })
        .await
    }
}

// -- FailingAliasResolver --------------------------------------------------
//
// Test-only `AliasResolver` that returns `Unreachable` for a configured
// set of `FileVersionId`s and `Ok(empty)` for every other version.
// Used by Phase A / B integration tests (commits 4 / 6 / 10) to drive
// the `BlockedByClosureIncomplete` path deterministically without an
// actual filesystem-offline condition.

use std::collections::BTreeSet;

use voom_core::ids::{FileLocationId, FileVersionId};

use crate::repo::media::commit_safety_gate::{AliasResolutionError, AliasResolver};

#[derive(Debug, Clone)]
pub struct FailingAliasResolver {
    failing: BTreeSet<FileVersionId>,
}

impl FailingAliasResolver {
    /// Construct from any iterable of `FileVersionId`. An empty
    /// iterable produces a resolver that never fails — useful as a
    /// silent stub for callers that just need an `AliasResolver`
    /// implementor.
    #[must_use]
    pub fn new(failing: impl IntoIterator<Item = FileVersionId>) -> Self {
        Self {
            failing: failing.into_iter().collect(),
        }
    }
}

#[async_trait::async_trait]
impl AliasResolver for FailingAliasResolver {
    async fn aliases_for_version(
        &self,
        file_version_id: FileVersionId,
    ) -> Result<Vec<FileLocationId>, AliasResolutionError> {
        if self.failing.contains(&file_version_id) {
            return Err(AliasResolutionError::Unreachable {
                message: format!("FailingAliasResolver: configured to fail for {file_version_id}"),
            });
        }
        Ok(Vec::new())
    }
}
