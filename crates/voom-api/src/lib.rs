//! HTTP surface for the control plane. Shared envelope without the host-only
//! `local` block.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Serialize;
use voom_control_plane::{HealthPlane, HealthSnapshot};
use voom_core::{ErrorCode, VoomError, format_iso8601};

pub const SCHEMA_VERSION: &str = "0";

#[derive(Clone, Debug)]
pub struct AppState {
    pub health_plane: HealthPlane,
    /// Number of tokio worker threads, snapshotted at router construction
    /// so `/health` doesn't re-syscall `available_parallelism()` per request.
    tokio_workers: usize,
}

pub fn router(health_plane: HealthPlane) -> axum::Router {
    let tokio_workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    axum::Router::new()
        .route("/health", get(health))
        .with_state(AppState {
            health_plane,
            tokio_workers,
        })
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
    match state.health_plane.health().await {
        Ok(HealthSnapshot::Current {
            migration_count,
            schema_init_at,
        }) => {
            let env = Envelope {
                schema_version: SCHEMA_VERSION,
                command: "health",
                status: "ok",
                data: Some(HealthData {
                    db: HealthDb {
                        status: "current",
                        schema_init_at: Some(format_iso8601(schema_init_at)),
                        migration_count: Some(migration_count),
                    },
                    runtime: HealthRuntime {
                        tokio_workers: state.tokio_workers,
                    },
                }),
                warnings: Vec::new(),
                error: None,
            };
            (StatusCode::OK, Json(env)).into_response()
        }
        Ok(snap) => {
            // `diagnostic()` returns Some for every non-Current variant —
            // we just matched Current above, so this is infallible.
            let diag = snap
                .diagnostic()
                .unwrap_or_else(|| unreachable!("non-Current snapshot has a diagnostic"));
            err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                diag.code.as_str(),
                diag.message,
                diag.hint,
            )
        }
        Err(err) => voom_error_response(&err),
    }
}

/// Classify a `VoomError` returned from `connect`/`probe_schema` into an HTTP
/// response. Exhaustive over [`ErrorCode`] so a new variant fails compilation
/// here rather than silently falling through to a 500.
fn voom_error_response(err: &VoomError) -> axum::response::Response {
    let (status, hint) = match err.error_code() {
        ErrorCode::DbUnreachable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Some(
                "Database file is missing or unreachable from this host \
                 — verify the configured path and filesystem permissions"
                    .to_owned(),
            ),
        ),
        ErrorCode::DbPartialSchema => (
            StatusCode::SERVICE_UNAVAILABLE,
            Some(
                "Schema metadata is missing or corrupted (e.g. schema_meta \
                 dropped or malformed). `voom init` will re-probe and fail \
                 with the same error — it cannot repair this state. Restore \
                 from backup or manually repair the schema_meta table."
                    .to_owned(),
            ),
        ),
        ErrorCode::DbSchemaTooNew => (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("Upgrade the server binary or roll the database back".to_owned()),
        ),
        ErrorCode::DbDirtyMigration => (
            StatusCode::SERVICE_UNAVAILABLE,
            Some(
                "Manual recovery required: remove the failed row from \
                 _sqlx_migrations or restore from backup. Do NOT just re-run \
                 voom init."
                    .to_owned(),
            ),
        ),
        // `ConfigInvalid` from `probe_schema` is the foreign-database guard
        // (someone else's DB at this path); 503 with the underlying message
        // is more useful to operators than 500.
        ErrorCode::ConfigInvalid => (StatusCode::SERVICE_UNAVAILABLE, None),
        // Codes that `connect`/`probe_schema` cannot produce today; classify
        // as internal so they're visible if they ever do appear.
        ErrorCode::DbUninitialized
        | ErrorCode::NotFound
        | ErrorCode::Internal
        | ErrorCode::BadArgs
        | ErrorCode::DependencyCycle
        | ErrorCode::Conflict
        // FailureClass-derived codes don't surface from `connect` /
        // `probe_schema` — they belong to lease/ticket use cases that
        // are not on this response path yet. Classify as internal if
        // one ever appears here; the visibility is the point.
        | ErrorCode::WorkerTimeout
        | ErrorCode::WorkerCrash
        | ErrorCode::NoEligibleWorker
        | ErrorCode::ArtifactUnavailable
        | ErrorCode::ArtifactChecksumMismatch
        | ErrorCode::ExternalSystemUnavailable
        | ErrorCode::ExternalSystemRateLimited
        | ErrorCode::VerificationFailure
        | ErrorCode::BackupFailure
        | ErrorCode::CommitFailure
        | ErrorCode::PolicyParseError
        | ErrorCode::PolicyValidationError
        | ErrorCode::PlanGenerationError
        | ErrorCode::ComplianceReportError
        | ErrorCode::PolicyExecutionError
        | ErrorCode::MissingCapability
        | ErrorCode::MalformedWorkerResult
        | ErrorCode::UserCancellation
        | ErrorCode::ApprovalRequired
        | ErrorCode::PriorityPolicyConflict
        // Commit-safety-gate codes — not on the connect/probe_schema path.
        | ErrorCode::BlockedByUseLease
        | ErrorCode::BlockedByPendingCommit
        | ErrorCode::BlockedByClosureGrew
        | ErrorCode::StaleIdentityEvidence
        | ErrorCode::ClosureResolutionIncomplete
        // Worker-protocol codes (Sprint 2) — not on the health path.
        | ErrorCode::WorkerRetired
        | ErrorCode::WorkerIncarnationStale
        | ErrorCode::AmbiguousWorkerSelection => (StatusCode::INTERNAL_SERVER_ERROR, None),
    };
    err_response(status, err.code(), err.to_string(), hint)
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
