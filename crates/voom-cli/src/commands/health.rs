use std::io;

use serde::Serialize;
use serde_json::json;
use voom_control_plane::{ControlPlane, DbStatus, HealthSnapshot};

use crate::envelope::{Local, emit_err, emit_ok};

#[derive(Debug, Serialize)]
pub struct HealthData {
    pub db: HealthDb,
    pub runtime: HealthRuntime,
}

#[derive(Debug, Serialize)]
pub struct HealthDb {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_init_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration_count: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct HealthRuntime {
    pub tokio_workers: usize,
}

pub async fn run(cp: &ControlPlane, local: Local) -> io::Result<i32> {
    match cp.health().await {
        Ok(snap) => emit_snapshot(&snap, local),
        Err(err) => {
            emit_err("health", err.code(), err.to_string(), None, Some(local))?;
            Ok(2)
        }
    }
}

fn emit_snapshot(snap: &HealthSnapshot, local: Local) -> io::Result<i32> {
    let status_str = match snap.db_status {
        DbStatus::Uninitialized => {
            emit_err(
                "health",
                "DB_UNINITIALIZED",
                "database has no migrations applied".into(),
                Some("Run: voom init".into()),
                Some(local),
            )?;
            return Ok(2);
        }
        DbStatus::Partial => {
            let detail = json!({
                "applied": snap.migration_count,
                "expected": snap.expected_migrations,
            });
            emit_err(
                "health",
                "DB_PARTIAL_SCHEMA",
                format!("database partially migrated: {detail}"),
                Some("Run: voom init against the current binary".into()),
                Some(local),
            )?;
            return Ok(2);
        }
        DbStatus::TooNew => {
            let detail = json!({
                "applied": snap.migration_count,
                "expected": snap.expected_migrations,
            });
            emit_err(
                "health",
                "DB_SCHEMA_TOO_NEW",
                format!(
                    "database has migrations this binary does not know about: {detail}; \
                     refusing to operate against unknown schema"
                ),
                Some(
                    "Use a newer voom binary or roll the database back to a known migration".into(),
                ),
                Some(local),
            )?;
            return Ok(2);
        }
        DbStatus::Dirty => {
            let detail = json!({
                "failed_version": snap.failed_version,
                "applied": snap.migration_count,
                "expected": snap.expected_migrations,
            });
            emit_err(
                "health",
                "DB_DIRTY_MIGRATION",
                format!(
                    "a previous migration left the schema in a dirty (failed) state: \
                     {detail}; sqlx will not run further migrations until the dirty \
                     row is resolved"
                ),
                Some(
                    "Manual recovery required: remove the failed row from \
                     _sqlx_migrations (e.g. DELETE FROM _sqlx_migrations WHERE version \
                     = <failed_version>) or restore from backup. Do NOT just re-run \
                     voom init — it will fail the same way."
                        .into(),
                ),
                Some(local),
            )?;
            return Ok(2);
        }
        DbStatus::Current => "current",
    };

    let data = HealthData {
        db: HealthDb {
            status: status_str,
            schema_init_at: snap.schema_init_at.map(|t| {
                t.format(&time::format_description::well_known::Iso8601::DEFAULT)
                    .unwrap_or_else(|_| t.unix_timestamp().to_string())
            }),
            migration_count: snap.migration_count,
        },
        runtime: HealthRuntime {
            tokio_workers: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        },
    };
    emit_ok("health", data, Some(local), Vec::new())?;
    Ok(0)
}
