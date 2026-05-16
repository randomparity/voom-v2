#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! App-services layer: wraps voom-store and exposes commands consumed by API/CLI.

use serde::Serialize;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_store::{SchemaState, connect, probe_schema};

#[derive(Debug, Clone)]
pub struct ControlPlane {
    pool: SqlitePool,
    database_url: String,
}

impl ControlPlane {
    /// Open an existing database. **Never creates files or directories** — if
    /// the DB doesn't exist, returns `DB_UNREACHABLE`. The CLI's `init` command
    /// is the only path that creates databases, and it calls
    /// `voom_store::init(url)` directly without going through `ControlPlane`.
    pub async fn open(database_url: String) -> Result<Self, VoomError> {
        let pool = connect(&database_url).await?;
        Ok(Self { pool, database_url })
    }

    #[must_use]
    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    /// Read-only health snapshot.
    pub async fn health(&self) -> Result<HealthSnapshot, VoomError> {
        let schema = probe_schema(&self.pool).await?;
        let (db_status, schema_init_at, migration_count, expected) = match schema {
            SchemaState::Uninitialized => (DbStatus::Uninitialized, None, None, None),
            SchemaState::Partial { applied, expected } => {
                (DbStatus::Partial, None, Some(applied), Some(expected))
            }
            SchemaState::Current { migration_count, schema_init_at } => (
                DbStatus::Current,
                Some(schema_init_at),
                Some(migration_count),
                None,
            ),
            SchemaState::TooNew { applied, expected } => {
                (DbStatus::TooNew, None, Some(applied), Some(expected))
            }
        };
        Ok(HealthSnapshot {
            db_status,
            schema_init_at,
            migration_count,
            expected_migrations: expected,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DbStatus {
    Uninitialized,
    Partial,
    Current,
    TooNew,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthSnapshot {
    pub db_status: DbStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_init_at: Option<OffsetDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration_count: Option<u32>,
    /// Present whenever `db_status` is Partial or TooNew; otherwise None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_migrations: Option<u32>,
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
        let err = ControlPlane::open(url).await.unwrap_err();
        assert_eq!(err.code(), "DB_UNREACHABLE");
    }

    #[tokio::test]
    async fn health_on_existing_but_uninitialized_db_is_uninitialized() {
        let (_keep, url) = fresh_url();
        // Create the DB (empty schema) via connect_or_create, then open via the
        // read-side path so the no-create rule isn't violated.
        voom_store::connect_or_create(&url).await.unwrap();

        let cp = ControlPlane::open(url).await.unwrap();
        let snap = cp.health().await.unwrap();
        assert_eq!(snap.db_status, DbStatus::Uninitialized);
        assert!(snap.schema_init_at.is_none());
        assert!(snap.migration_count.is_none());
    }

    #[tokio::test]
    async fn init_then_health_reports_current() {
        let (_keep, url) = fresh_url();
        let report = voom_store::init(&url).await.unwrap();
        assert!(!report.already_initialized);

        let cp = ControlPlane::open(url).await.unwrap();
        let snap = cp.health().await.unwrap();
        assert_eq!(snap.db_status, DbStatus::Current);
        assert_eq!(snap.migration_count, Some(1));
        assert!(snap.schema_init_at.is_some());
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
    async fn health_maps_too_new_state() {
        let (_keep, url) = fresh_url();
        voom_store::init(&url).await.unwrap();

        // Inject a synthetic future migration row via a sibling no-create pool
        // — the on-disk DB already exists, so connect() suffices.
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

        let cp = ControlPlane::open(url).await.unwrap();
        let snap = cp.health().await.unwrap();
        assert_eq!(snap.db_status, DbStatus::TooNew);
        assert!(snap.migration_count.unwrap() > snap.expected_migrations.unwrap());
        assert!(snap.schema_init_at.is_none());
    }
}
