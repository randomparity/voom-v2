//! HTTP surface for the control plane. Shared envelope without the host-only
//! `local` block.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Serialize;
use voom_control_plane::{ControlPlane, DbStatus};

pub const SCHEMA_VERSION: &str = "0";

#[derive(Clone, Debug)]
pub struct AppState {
    pub control_plane: ControlPlane,
}

pub fn router(control_plane: ControlPlane) -> axum::Router {
    axum::Router::new()
        .route("/health", get(health))
        .with_state(AppState { control_plane })
}

#[derive(Debug, Serialize)]
struct Envelope<T: Serialize> {
    schema_version: &'static str,
    command: &'static str,
    status: &'static str,
    data: Option<T>,
    warnings: Vec<String>,
    error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

#[derive(Debug, Serialize)]
struct HealthData {
    db: HealthDb,
    runtime: HealthRuntime,
}

#[derive(Debug, Serialize)]
struct HealthDb {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_init_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    migration_count: Option<u32>,
}

#[derive(Debug, Serialize)]
struct HealthRuntime {
    tokio_workers: usize,
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match state.control_plane.health().await {
        Ok(snap) => match snap.db_status {
            DbStatus::Uninitialized => err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "DB_UNINITIALIZED",
                "database has no migrations applied".into(),
                Some("Run `voom init` on the host that owns this database".into()),
            ),
            DbStatus::Partial => err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "DB_PARTIAL_SCHEMA",
                format!(
                    "database partially migrated (applied={:?}, expected={:?})",
                    snap.migration_count, snap.expected_migrations
                ),
                Some("Run `voom init` against the current binary".into()),
            ),
            DbStatus::TooNew => err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "DB_SCHEMA_TOO_NEW",
                format!(
                    "database has migrations this binary does not know about \
                     (applied={:?}, expected={:?})",
                    snap.migration_count, snap.expected_migrations
                ),
                Some("Upgrade the server binary or roll the database back".into()),
            ),
            DbStatus::Current => {
                let env = Envelope {
                    schema_version: SCHEMA_VERSION,
                    command: "health",
                    status: "ok",
                    data: Some(HealthData {
                        db: HealthDb {
                            status: "current",
                            schema_init_at: snap.schema_init_at.map(|t| {
                                t.format(&time::format_description::well_known::Iso8601::DEFAULT)
                                    .unwrap_or_default()
                            }),
                            migration_count: snap.migration_count,
                        },
                        runtime: HealthRuntime {
                            tokio_workers: std::thread::available_parallelism()
                                .map_or(1, std::num::NonZero::get),
                        },
                    }),
                    warnings: Vec::new(),
                    error: None,
                };
                (StatusCode::OK, Json(env)).into_response()
            }
        },
        Err(err) => {
            // Known database/schema failures are dependency problems, not
            // handler bugs — return 503 with a recovery hint so operators see
            // the same actionable status as Partial/TooNew. Reserve 500 for
            // genuinely unexpected internal errors.
            let (status, hint) = match err.code() {
                "DB_UNREACHABLE" => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Some(
                        "Database file is missing or unreachable from this host \
                         — verify the configured path and filesystem permissions"
                            .to_owned(),
                    ),
                ),
                "DB_PARTIAL_SCHEMA" => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Some(
                        "Schema metadata is missing or corrupted; run `voom init` \
                         against the current binary or restore from backup"
                            .to_owned(),
                    ),
                ),
                "DB_SCHEMA_TOO_NEW" => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Some("Upgrade the server binary or roll the database back".to_owned()),
                ),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, None),
            };
            err_response(status, err.code(), err.to_string(), hint)
        }
    }
}

fn err_response(
    status: StatusCode,
    code: &'static str,
    message: String,
    hint: Option<String>,
) -> axum::response::Response {
    let env: Envelope<()> = Envelope {
        schema_version: SCHEMA_VERSION,
        command: "health",
        status: "error",
        data: None,
        warnings: Vec::new(),
        error: Some(ErrorBody {
            code,
            message,
            hint,
        }),
    };
    (status, Json(env)).into_response()
}
