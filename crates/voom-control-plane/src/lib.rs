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
                     just re-run voom init."
                        .to_owned(),
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_url() -> (tempfile::NamedTempFile, String) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite://{}", tmp.path().display());
        (tmp, url)
    }

    #[tokio::test]
    async fn open_refuses_missing_database() {
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", tmp.path().join("nope.db").display());
        let err = ControlPlane::open(&url).await.unwrap_err();
        assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
    }

    #[tokio::test]
    async fn health_on_existing_but_uninitialized_db_is_uninitialized() {
        let (_keep, url) = fresh_url();
        voom_store::connect_or_create(&url).await.unwrap();

        let cp = ControlPlane::open(&url).await.unwrap();
        let snap = cp.health().await.unwrap();
        assert_eq!(snap, HealthSnapshot::Uninitialized);
    }

    #[tokio::test]
    async fn init_then_health_reports_current() {
        let (_keep, url) = fresh_url();
        let report = voom_store::init(&url).await.unwrap();
        assert!(!report.already_initialized);

        let cp = ControlPlane::open(&url).await.unwrap();
        let snap = cp.health().await.unwrap();
        match snap {
            HealthSnapshot::Current {
                migration_count,
                schema_init_at: _,
            } => assert_eq!(migration_count, 1),
            other => panic!("expected Current, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn second_init_returns_already_initialized() {
        let (_keep, url) = fresh_url();
        voom_store::init(&url).await.unwrap();
        let second = voom_store::init(&url).await.unwrap();
        assert!(second.already_initialized);
        assert_eq!(second.migrations_applied, 0);
    }

    #[tokio::test]
    async fn health_maps_dirty_state() {
        let (_keep, url) = fresh_url();
        voom_store::init(&url).await.unwrap();

        {
            let pool = voom_store::connect(&url).await.unwrap();
            sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
                .execute(&pool)
                .await
                .unwrap();
        }

        let cp = ControlPlane::open(&url).await.unwrap();
        let snap = cp.health().await.unwrap();
        match snap {
            HealthSnapshot::Dirty {
                failed_version,
                applied: _,
                expected: _,
            } => assert_eq!(failed_version, 1),
            other => panic!("expected Dirty, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn health_maps_too_new_state() {
        let (_keep, url) = fresh_url();
        voom_store::init(&url).await.unwrap();

        {
            let pool = voom_store::connect(&url).await.unwrap();
            sqlx::query(
                "INSERT INTO _sqlx_migrations \
                 (version, description, installed_on, success, checksum, execution_time) \
                 VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
            )
            .execute(&pool)
            .await
            .unwrap();
        }

        let cp = ControlPlane::open(&url).await.unwrap();
        let snap = cp.health().await.unwrap();
        match snap {
            HealthSnapshot::TooNew { applied, expected } => {
                assert!(applied > expected);
            }
            other => panic!("expected TooNew, got {other:?}"),
        }
    }

    /// Exhaustive coverage check: every non-Current variant must produce a
    /// diagnostic with a non-empty message. Adding a `HealthSnapshot` variant
    /// without updating `diagnostic()` fails to compile (the match in
    /// `diagnostic()` is exhaustive); this test then catches any new variant
    /// that returns an empty or placeholder message.
    #[test]
    fn diagnostic_covers_every_non_current_variant() {
        let now = OffsetDateTime::UNIX_EPOCH;
        let cases = [
            HealthSnapshot::Uninitialized,
            HealthSnapshot::Partial {
                applied: 0,
                expected: 1,
            },
            HealthSnapshot::TooNew {
                applied: 2,
                expected: 1,
            },
            HealthSnapshot::Dirty {
                failed_version: 1,
                applied: 1,
                expected: 1,
            },
        ];
        for snap in &cases {
            let diag = snap.diagnostic().unwrap_or_else(|| {
                panic!("non-Current variant {snap:?} returned None from diagnostic()")
            });
            assert!(!diag.message.is_empty(), "{snap:?} has empty message");
            assert!(diag.hint.is_some(), "{snap:?} has no hint");
        }

        // Current returns None.
        let current = HealthSnapshot::Current {
            migration_count: 1,
            schema_init_at: now,
        };
        assert!(current.diagnostic().is_none());
    }

    /// Regression guard for the issue #1 ugliness: `Option<u32>` Debug
    /// produced `applied=Some(0)` in operator-facing strings. The ADT
    /// fields are plain integers, so the formatted string must not contain
    /// `Some(`.
    #[test]
    fn diagnostic_messages_have_no_debug_options() {
        let snaps = [
            HealthSnapshot::Partial {
                applied: 0,
                expected: 1,
            },
            HealthSnapshot::TooNew {
                applied: 2,
                expected: 1,
            },
            HealthSnapshot::Dirty {
                failed_version: 1,
                applied: 1,
                expected: 1,
            },
        ];
        for snap in &snaps {
            let diag = snap.diagnostic().unwrap();
            assert!(
                !diag.message.contains("Some("),
                "diagnostic message for {snap:?} leaks Option Debug: {msg}",
                msg = diag.message,
            );
        }
    }
}
