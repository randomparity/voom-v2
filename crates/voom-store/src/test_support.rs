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

use sqlx::SqlitePool;
use voom_core::VoomError;

use crate::init::init;
use crate::pool::connect;

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
