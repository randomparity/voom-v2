use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;
use voom_core::ids::ArtifactVerificationId;
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, ErrorCode, FailureClass, VoomError, WorkerId,
};
use voom_events::payload::{
    ArtifactVerificationFailedPayload, ArtifactVerificationStartedPayload,
    ArtifactVerificationSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::artifacts::{
    ArtifactLocation, ArtifactVerificationStatus, NewArtifactVerification,
};
use voom_worker_protocol::{
    VerifyArtifactExpectedFacts, VerifyArtifactRequest, VerifyArtifactResult,
};

use crate::ControlPlane;
use crate::artifact::bootstrap::ensure_builtin_verify_artifact_worker_in_tx;
use crate::artifact::worker::{BundledWorkerProcess, VerifyWorkerError};
use crate::cases::{append_event, begin_tx, commit_tx};

#[derive(Debug)]
pub struct VerifyArtifactInput {
    pub artifact_handle_id: ArtifactHandleId,
}

#[derive(Debug)]
pub struct VerifyArtifactReport {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub worker_id: WorkerId,
    pub status: ArtifactVerificationStatus,
    pub path: PathBuf,
    pub expected_size_bytes: u64,
    pub expected_checksum: String,
    pub observed_size_bytes: Option<u64>,
    pub observed_checksum: Option<String>,
    pub error_code: Option<ErrorCode>,
    pub message: Option<String>,
}

#[async_trait]
pub(crate) trait VerifyArtifactDispatcher: Send + Sync {
    async fn dispatch_verify_artifact(
        &self,
        worker_id: WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, VerifyWorkerError>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BundledVerifyArtifactDispatcher;

#[async_trait]
impl VerifyArtifactDispatcher for BundledVerifyArtifactDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        worker_id: WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, VerifyWorkerError> {
        let mut worker = BundledWorkerProcess::launch_bundled_verify_artifact(worker_id).await?;
        let result = worker.dispatch_verify_artifact(request).await;
        let _status = worker.shutdown(std::time::Duration::from_secs(5)).await;
        result
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct VerifyArtifactPersistContext<'a> {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub worker_id: WorkerId,
    pub path: &'a str,
}

#[async_trait]
pub(crate) trait VerifyArtifactHooks: Send + Sync {
    async fn before_persist(
        &self,
        _cp: &ControlPlane,
        _context: VerifyArtifactPersistContext<'_>,
    ) -> Result<(), VoomError> {
        Ok(())
    }

    async fn before_terminal_event(
        &self,
        _context: VerifyArtifactPersistContext<'_>,
    ) -> Result<(), VoomError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NoVerifyArtifactHooks;

#[async_trait]
impl VerifyArtifactHooks for NoVerifyArtifactHooks {}

impl ControlPlane {
    /// Verify the one live staging location for an artifact handle through the
    /// bundled out-of-process verify worker and record the durable result.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing handle/location, `Config` when the
    /// handle does not have exactly one live staging location or expected
    /// size/hash facts, worker-domain errors as failed verification rows, and
    /// database errors for durable recording failures.
    pub async fn verify_artifact(
        &self,
        input: VerifyArtifactInput,
    ) -> Result<VerifyArtifactReport, VoomError> {
        verify_artifact_with_dispatcher(
            self,
            input,
            &BundledVerifyArtifactDispatcher,
            &NoVerifyArtifactHooks,
        )
        .await
    }
}

pub(crate) async fn verify_artifact_with_dispatcher(
    cp: &ControlPlane,
    input: VerifyArtifactInput,
    dispatcher: &dyn VerifyArtifactDispatcher,
    hooks: &dyn VerifyArtifactHooks,
) -> Result<VerifyArtifactReport, VoomError> {
    let expected = load_expected_artifact_facts(cp, input.artifact_handle_id).await?;
    let location = select_live_staging_location(cp, input.artifact_handle_id).await?;
    let path = location.value.clone();

    let worker_id =
        record_verification_started(cp, input.artifact_handle_id, location.id, &path).await?;

    let request = VerifyArtifactRequest {
        path: path.clone(),
        expected: VerifyArtifactExpectedFacts {
            size_bytes: expected.size_bytes,
            content_hash: expected.checksum.clone(),
            modified_at: None,
            local_file_key: None,
        },
    };
    let outcome = dispatcher
        .dispatch_verify_artifact(worker_id, request)
        .await
        .map_or_else(VerifyOutcome::Failed, VerifyOutcome::Succeeded);

    hooks
        .before_persist(
            cp,
            VerifyArtifactPersistContext {
                artifact_handle_id: input.artifact_handle_id,
                artifact_location_id: location.id,
                worker_id,
                path: &path,
            },
        )
        .await?;

    persist_verification_outcome(
        cp,
        VerifyArtifactPersistContext {
            artifact_handle_id: input.artifact_handle_id,
            artifact_location_id: location.id,
            worker_id,
            path: &path,
        },
        expected,
        outcome,
        hooks,
    )
    .await
}

#[derive(Debug, Clone)]
struct ExpectedArtifactFacts {
    size_bytes: u64,
    checksum: String,
}

#[derive(Debug)]
enum VerifyOutcome {
    Succeeded(VerifyArtifactResult),
    Failed(VerifyWorkerError),
}

async fn load_expected_artifact_facts(
    cp: &ControlPlane,
    handle_id: ArtifactHandleId,
) -> Result<ExpectedArtifactFacts, VoomError> {
    let row = sqlx::query("SELECT size_bytes, checksum FROM artifact_handles WHERE id = ?")
        .bind(i64::try_from(handle_id.0).map_err(|err| {
            VoomError::Internal(format!("artifact handle id exceeds SQLite integer: {err}"))
        })?)
        .fetch_optional(&cp.pool)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_handles facts: {e}")))?;
    let Some(row) = row else {
        return Err(VoomError::NotFound(format!(
            "artifact_handles {handle_id} missing"
        )));
    };
    let size_bytes: Option<i64> = sqlx::Row::try_get(&row, "size_bytes")
        .map_err(|e| VoomError::Database(format!("artifact_handles.size_bytes: {e}")))?;
    let checksum: Option<String> = sqlx::Row::try_get(&row, "checksum")
        .map_err(|e| VoomError::Database(format!("artifact_handles.checksum: {e}")))?;
    let size_bytes = size_bytes.ok_or_else(|| {
        VoomError::Config(format!(
            "artifact_handle {handle_id} missing expected size_bytes"
        ))
    })?;
    let checksum = checksum.ok_or_else(|| {
        VoomError::Config(format!(
            "artifact_handle {handle_id} missing expected checksum"
        ))
    })?;
    Ok(ExpectedArtifactFacts {
        size_bytes: u64::try_from(size_bytes).map_err(|err| {
            VoomError::Database(format!("artifact_handles.size_bytes negative: {err}"))
        })?,
        checksum,
    })
}

async fn select_live_staging_location(
    cp: &ControlPlane,
    handle_id: ArtifactHandleId,
) -> Result<ArtifactLocation, VoomError> {
    let locations = cp.artifacts.list_locations_for_handle(handle_id).await?;
    let staging = locations
        .into_iter()
        .filter(|location| location.kind == "staging")
        .collect::<Vec<_>>();
    let [location] = staging.as_slice() else {
        return Err(VoomError::Config(format!(
            "artifact_handle {handle_id} must have exactly one live staging location; found {}",
            staging.len()
        )));
    };
    Ok(location.clone())
}

async fn record_verification_started(
    cp: &ControlPlane,
    handle_id: ArtifactHandleId,
    location_id: ArtifactLocationId,
    path: &str,
) -> Result<WorkerId, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let worker = ensure_builtin_verify_artifact_worker_in_tx(cp, &mut tx).await?;
    let now = cp.clock().now();
    append_event(
        &cp.events,
        &mut tx,
        SubjectType::ArtifactHandle,
        Some(handle_id.0),
        now,
        Event::ArtifactVerificationStarted(ArtifactVerificationStartedPayload {
            artifact_handle_id: handle_id.0,
            artifact_location_id: location_id.0,
            worker_id: worker.id.0,
            path: path.to_owned(),
        }),
    )
    .await?;
    commit_tx(tx).await?;
    Ok(worker.id)
}

async fn persist_verification_outcome(
    cp: &ControlPlane,
    context: VerifyArtifactPersistContext<'_>,
    expected: ExpectedArtifactFacts,
    outcome: VerifyOutcome,
    hooks: &dyn VerifyArtifactHooks,
) -> Result<VerifyArtifactReport, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let outcome = validate_success_facts(&expected, outcome);
    let outcome = match revalidate_selected_live_staging_location(
        &mut tx,
        context.artifact_handle_id,
        context.artifact_location_id,
        context.path,
    )
    .await
    {
        Ok(()) => outcome,
        Err(err) if is_stale_location_revalidation(&err) => {
            VerifyOutcome::Failed(VerifyWorkerError::terminal_error(
                FailureClass::ArtifactUnavailable,
                ErrorCode::ArtifactUnavailable,
                format!("verification result rejected because live staging changed: {err}"),
            ))
        }
        Err(err) => return Err(err),
    };
    let input = new_verification_input(
        context.artifact_handle_id,
        context.artifact_location_id,
        context.worker_id,
        context.path,
        &expected,
        &outcome,
        now,
    )?;
    let verification = cp
        .artifacts
        .record_verification_in_tx(&mut tx, input)
        .await?;
    hooks.before_terminal_event(context).await?;
    append_terminal_event(cp, &mut tx, &verification, &outcome, now, context).await?;
    commit_tx(tx).await?;

    Ok(VerifyArtifactReport {
        artifact_handle_id: context.artifact_handle_id,
        artifact_location_id: context.artifact_location_id,
        verification_id: verification.id,
        worker_id: context.worker_id,
        status: verification.status,
        path: PathBuf::from(context.path),
        expected_size_bytes: expected.size_bytes,
        expected_checksum: expected.checksum,
        observed_size_bytes: verification.observed_size_bytes,
        observed_checksum: verification.observed_checksum,
        error_code: verification
            .error_code
            .as_deref()
            .map(parse_error_code)
            .transpose()?,
        message: verification.message,
    })
}

async fn revalidate_selected_live_staging_location(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    handle_id: ArtifactHandleId,
    location_id: ArtifactLocationId,
    path: &str,
) -> Result<(), VoomError> {
    let selected: Option<(i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT artifact_handle_id, kind, value, retired_at \
         FROM artifact_locations WHERE id = ?",
    )
    .bind(i64::try_from(location_id.0).map_err(|err| {
        VoomError::Internal(format!(
            "artifact location id exceeds SQLite integer: {err}"
        ))
    })?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| VoomError::Database(format!("artifact_locations verify selected: {err}")))?;
    let Some((owner_id, kind, value, retired_at)) = selected else {
        return Err(VoomError::NotFound(format!(
            "artifact_locations {location_id} missing"
        )));
    };
    if u64::try_from(owner_id).ok() != Some(handle_id.0) || kind != "staging" || value != path {
        return Err(VoomError::Conflict(format!(
            "artifact_locations {location_id} no longer matches artifact_handle {handle_id}"
        )));
    }
    if retired_at.is_some() {
        return Err(VoomError::Config(format!(
            "artifact_location {location_id} is no longer live staging"
        )));
    }

    let live_staging: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, value FROM artifact_locations \
         WHERE artifact_handle_id = ? AND kind = 'staging' AND retired_at IS NULL \
         ORDER BY id ASC",
    )
    .bind(i64::try_from(handle_id.0).map_err(|err| {
        VoomError::Internal(format!("artifact handle id exceeds SQLite integer: {err}"))
    })?)
    .fetch_all(&mut **tx)
    .await
    .map_err(|err| VoomError::Database(format!("artifact_locations verify live staging: {err}")))?;
    let [(live_id, live_path)] = live_staging.as_slice() else {
        return Err(VoomError::Config(format!(
            "artifact_handle {handle_id} must still have exactly one live staging location; found {}",
            live_staging.len()
        )));
    };
    if u64::try_from(*live_id).ok() != Some(location_id.0) || live_path != path {
        return Err(VoomError::Config(format!(
            "artifact_handle {handle_id} live staging location changed during verification"
        )));
    }
    Ok(())
}

fn is_stale_location_revalidation(err: &VoomError) -> bool {
    matches!(err, VoomError::Config(_) | VoomError::Conflict(_))
}

fn validate_success_facts(
    expected: &ExpectedArtifactFacts,
    outcome: VerifyOutcome,
) -> VerifyOutcome {
    match outcome {
        VerifyOutcome::Succeeded(result)
            if result.observed.size_bytes != expected.size_bytes
                || result.observed.content_hash != expected.checksum =>
        {
            VerifyOutcome::Failed(VerifyWorkerError::terminal_error(
                FailureClass::ArtifactChecksumMismatch,
                ErrorCode::ArtifactChecksumMismatch,
                "verified artifact facts differ from expected size/hash",
            ))
        }
        other => other,
    }
}

fn new_verification_input(
    handle_id: ArtifactHandleId,
    location_id: ArtifactLocationId,
    worker_id: WorkerId,
    path: &str,
    expected: &ExpectedArtifactFacts,
    outcome: &VerifyOutcome,
    now: time::OffsetDateTime,
) -> Result<NewArtifactVerification, VoomError> {
    match outcome {
        VerifyOutcome::Succeeded(result) => Ok(NewArtifactVerification {
            artifact_handle_id: handle_id,
            artifact_location_id: location_id,
            path: path.to_owned(),
            worker_id,
            status: ArtifactVerificationStatus::Succeeded,
            expected_size_bytes: expected.size_bytes,
            expected_checksum: expected.checksum.clone(),
            observed_size_bytes: Some(result.observed.size_bytes),
            observed_checksum: Some(result.observed.content_hash.clone()),
            failure_class: None,
            error_code: None,
            message: None,
            report: json!({
                "provider": result.provider,
                "provider_version": result.provider_version,
                "status": result.status,
                "observed": result.observed,
            }),
            started_at: now,
            finished_at: now,
        }),
        VerifyOutcome::Failed(err) => Ok(NewArtifactVerification {
            artifact_handle_id: handle_id,
            artifact_location_id: location_id,
            path: path.to_owned(),
            worker_id,
            status: ArtifactVerificationStatus::Failed,
            expected_size_bytes: expected.size_bytes,
            expected_checksum: expected.checksum.clone(),
            observed_size_bytes: None,
            observed_checksum: None,
            failure_class: Some(failure_class_wire(err.failure_class())?),
            error_code: Some(err.error_code().as_str().to_owned()),
            message: Some(err.to_string()),
            report: json!({
                "error_code": err.error_code().as_str(),
                "failure_class": failure_class_wire(err.failure_class())?,
                "message": err.to_string(),
            }),
            started_at: now,
            finished_at: now,
        }),
    }
}

async fn append_terminal_event(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    verification: &voom_store::repo::artifacts::ArtifactVerification,
    outcome: &VerifyOutcome,
    occurred_at: time::OffsetDateTime,
    context: VerifyArtifactPersistContext<'_>,
) -> Result<(), VoomError> {
    let payload = match outcome {
        VerifyOutcome::Succeeded(result) => {
            Event::ArtifactVerificationSucceeded(ArtifactVerificationSucceededPayload {
                verification_id: verification.id.0,
                artifact_handle_id: context.artifact_handle_id.0,
                artifact_location_id: context.artifact_location_id.0,
                worker_id: context.worker_id.0,
                observed_size_bytes: result.observed.size_bytes,
                observed_checksum: result.observed.content_hash.clone(),
            })
        }
        VerifyOutcome::Failed(err) => {
            Event::ArtifactVerificationFailed(ArtifactVerificationFailedPayload {
                verification_id: verification.id.0,
                artifact_handle_id: context.artifact_handle_id.0,
                artifact_location_id: context.artifact_location_id.0,
                worker_id: context.worker_id.0,
                error_code: err.error_code().as_str().to_owned(),
            })
        }
    };
    append_event(
        &cp.events,
        tx,
        SubjectType::ArtifactHandle,
        Some(context.artifact_handle_id.0),
        occurred_at,
        payload,
    )
    .await
}

fn failure_class_wire(class: FailureClass) -> Result<String, VoomError> {
    serde_json::to_value(class)
        .map_err(|err| VoomError::Internal(format!("failure class encode: {err}")))?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| VoomError::Internal("failure class encoded as non-string".to_owned()))
}

fn parse_error_code(code: &str) -> Result<ErrorCode, VoomError> {
    ErrorCode::from_wire_str(code).ok_or_else(|| {
        VoomError::Internal(format!(
            "unsupported persisted verification error code {code}"
        ))
    })
}

#[cfg(test)]
#[path = "verify_test.rs"]
mod tests;
