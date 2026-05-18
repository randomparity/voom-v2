use std::io;

use serde::Serialize;
use voom_control_plane::{HealthPlane, HealthSnapshot};
use voom_core::{ErrorCode, VoomError, format_iso8601};

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

pub async fn run(hp: &HealthPlane, local: Local) -> io::Result<i32> {
    match hp.health().await {
        Ok(snap) => emit_snapshot(snap, local),
        Err(err) => {
            emit_err(
                "health",
                err.code(),
                err.to_string(),
                voom_error_hint(&err),
                Some(local),
            )?;
            Ok(2)
        }
    }
}

/// Per-`ErrorCode` remediation hint for failures bubbling out of
/// `ControlPlane::open` / `health`. Exhaustive so a new variant fails to
/// compile here rather than silently shipping no hint.
///
/// Shared by both `health::run` and the open-path in `main::dispatch` so the
/// two CLI sites cannot drift apart.
#[must_use]
pub fn voom_error_hint(err: &VoomError) -> Option<String> {
    match err.error_code() {
        ErrorCode::DbUnreachable => Some(
            "Database file is missing or unreachable — run `voom init` to \
             create it, or verify --database-url and filesystem permissions"
                .to_owned(),
        ),
        ErrorCode::DbPartialSchema => Some(
            "Schema metadata is corrupted (e.g. schema_meta dropped or \
             malformed). `voom init` cannot repair this state — restore from \
             backup or manually repair the schema_meta table."
                .to_owned(),
        ),
        // Codes the control plane doesn't return today; surface no hint
        // rather than invent generic advice.
        ErrorCode::DbUninitialized
        | ErrorCode::DbSchemaTooNew
        | ErrorCode::DbDirtyMigration
        | ErrorCode::ConfigInvalid
        | ErrorCode::NotFound
        | ErrorCode::Internal
        | ErrorCode::BadArgs
        | ErrorCode::DependencyCycle
        | ErrorCode::Conflict
        // FailureClass-derived codes belong to lease/ticket flows the
        // health command never reaches.
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
        | ErrorCode::MissingCapability
        | ErrorCode::MalformedWorkerResult
        | ErrorCode::UserCancellation
        | ErrorCode::ApprovalRequired
        | ErrorCode::PriorityPolicyConflict
        // Commit-safety-gate codes — health command never reaches these.
        | ErrorCode::BlockedByUseLease
        | ErrorCode::BlockedByPendingCommit
        | ErrorCode::BlockedByClosureGrew
        | ErrorCode::StaleIdentityEvidence
        | ErrorCode::ClosureResolutionIncomplete => None,
    }
}

fn emit_snapshot(snap: HealthSnapshot, local: Local) -> io::Result<i32> {
    match snap {
        HealthSnapshot::Current {
            migration_count,
            schema_init_at,
        } => {
            let data = HealthData {
                db: HealthDb {
                    status: "current",
                    schema_init_at: Some(format_iso8601(schema_init_at)),
                    migration_count: Some(migration_count),
                },
                runtime: HealthRuntime {
                    tokio_workers: std::thread::available_parallelism()
                        .map_or(1, std::num::NonZero::get),
                },
            };
            emit_ok("health", data, Some(local), Vec::new())?;
            Ok(0)
        }
        other => {
            // `diagnostic()` returns Some for every non-Current variant.
            let diag = other
                .diagnostic()
                .unwrap_or_else(|| unreachable!("non-Current snapshot has a diagnostic"));
            emit_err(
                "health",
                diag.code.as_str(),
                diag.message,
                diag.hint,
                Some(local),
            )?;
            Ok(2)
        }
    }
}
