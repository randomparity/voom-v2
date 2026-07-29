use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::json;
use voom_core::ids::ArtifactVerificationId;
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, ErrorCode, FailureClass, LeaseId, TicketId, VoomError,
    WorkerId,
};
use voom_events::payload::{
    ArtifactVerificationFailedPayload, ArtifactVerificationStartedPayload,
    ArtifactVerificationSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::artifacts::{
    ArtifactLocation, ArtifactVerification, ArtifactVerificationStatus, NewArtifactVerification,
    PolicyArtifactTarget,
};
use voom_worker_protocol::{
    VerifyArtifactExpectedFacts, VerifyArtifactRequest, VerifyArtifactResult,
};

use crate::ControlPlane;
use crate::artifact::bootstrap::ensure_builtin_verify_artifact_worker_in_tx;
use crate::artifact::worker::{BundledWorkerProcess, VerifyWorkerError};
use crate::cases::{append_event, begin_immediate_tx, commit_tx};

#[derive(Debug)]
pub struct VerifyArtifactInput {
    pub artifact_handle_id: ArtifactHandleId,
    /// Staging directory the artifact must reside within. Sent to the worker,
    /// which rejects any artifact path not contained by this root.
    pub staging_root: PathBuf,
}

impl VerifyArtifactInput {
    /// Build an input whose containment root is the directory holding the
    /// just-staged file. The workflow wrote the artifact there, so that
    /// directory is the authoritative staging boundary — and because it is
    /// derived from the workflow's own path rather than the DB-read location
    /// value, it still catches a location that has been corrupted to point
    /// elsewhere.
    #[must_use]
    pub(crate) fn for_staged_file(
        artifact_handle_id: ArtifactHandleId,
        staged_path: &Path,
    ) -> Self {
        Self {
            artifact_handle_id,
            staging_root: staged_path
                .parent()
                .map_or_else(|| PathBuf::from("/"), Path::to_path_buf),
        }
    }
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

#[derive(Debug, Clone)]
pub(crate) struct PolicyVerifyArtifactInput {
    pub target: PolicyArtifactTarget,
    pub worker_id: WorkerId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
}

#[async_trait]
pub(crate) trait VerifyArtifactDispatcher: Send + Sync {
    async fn dispatch_verify_artifact(
        &self,
        worker_id: WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, VerifyWorkerError>;

    fn start_session(&self) -> Box<dyn VerifyArtifactSession + '_> {
        Box::new(PerDispatchVerifyArtifactSession { dispatcher: self })
    }
}

#[async_trait]
pub(crate) trait VerifyArtifactSession: VerifyArtifactDispatcher {
    async fn shutdown(self: Box<Self>);
}

struct PerDispatchVerifyArtifactSession<
    'a,
    D: VerifyArtifactDispatcher + ?Sized = dyn VerifyArtifactDispatcher + 'a,
> {
    dispatcher: &'a D,
}

#[async_trait]
impl<D> VerifyArtifactDispatcher for PerDispatchVerifyArtifactSession<'_, D>
where
    D: VerifyArtifactDispatcher + ?Sized,
{
    async fn dispatch_verify_artifact(
        &self,
        worker_id: WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, VerifyWorkerError> {
        self.dispatcher
            .dispatch_verify_artifact(worker_id, request)
            .await
    }

    fn start_session(&self) -> Box<dyn VerifyArtifactSession + '_> {
        Box::new(Self {
            dispatcher: self.dispatcher,
        })
    }
}

#[async_trait]
impl<D> VerifyArtifactSession for PerDispatchVerifyArtifactSession<'_, D>
where
    D: VerifyArtifactDispatcher + ?Sized,
{
    async fn shutdown(self: Box<Self>) {}
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

    fn start_session(&self) -> Box<dyn VerifyArtifactSession + '_> {
        Box::new(BundledVerifyArtifactSession::default())
    }
}

#[derive(Debug, Default)]
struct BundledVerifyArtifactSession {
    worker: tokio::sync::Mutex<Option<(WorkerId, BundledWorkerProcess)>>,
}

#[async_trait]
impl VerifyArtifactDispatcher for BundledVerifyArtifactSession {
    async fn dispatch_verify_artifact(
        &self,
        worker_id: WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, VerifyWorkerError> {
        let mut session_worker = self.worker.lock().await;
        let result = {
            let worker = if let Some((session_worker_id, worker)) = session_worker.as_mut() {
                if *session_worker_id != worker_id {
                    return Err(VerifyWorkerError::terminal_error(
                        FailureClass::MalformedWorkerResult,
                        ErrorCode::MalformedWorkerResult,
                        format!(
                            "verify session worker changed from {session_worker_id} \
                                 to {worker_id}"
                        ),
                    ));
                }
                worker
            } else {
                let launched =
                    BundledWorkerProcess::launch_bundled_verify_artifact(worker_id).await?;
                &mut session_worker.insert((worker_id, launched)).1
            };
            worker.dispatch_verify_artifact(request).await
        };
        if result
            .as_ref()
            .is_err_and(VerifyWorkerError::should_shutdown_worker)
        {
            *session_worker = None;
        }
        result
    }
}

#[async_trait]
impl VerifyArtifactSession for BundledVerifyArtifactSession {
    async fn shutdown(self: Box<Self>) {
        let Some((_worker_id, worker)) = self.worker.into_inner() else {
            return;
        };
        if let Err(error) = worker.shutdown(std::time::Duration::from_secs(5)).await {
            tracing::warn!(%error, "verify artifact worker session shutdown failed");
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct VerifyArtifactPersistContext<'a> {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub worker_id: WorkerId,
    pub path: &'a str,
    pub location_kind: &'a str,
    pub require_only_live_kind: bool,
    pub workflow_ticket_id: Option<TicketId>,
    pub workflow_lease_id: Option<LeaseId>,
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
        staging_root: input.staging_root.to_string_lossy().into_owned(),
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

    let context = VerifyArtifactPersistContext {
        artifact_handle_id: input.artifact_handle_id,
        artifact_location_id: location.id,
        worker_id,
        path: &path,
        location_kind: "staging",
        require_only_live_kind: true,
        workflow_ticket_id: None,
        workflow_lease_id: None,
    };
    hooks.before_persist(cp, context).await?;

    persist_verification_outcome(cp, context, expected, outcome, hooks).await
}

pub(crate) async fn verify_policy_artifact_with_dispatcher(
    cp: &ControlPlane,
    input: &PolicyVerifyArtifactInput,
    dispatcher: &dyn VerifyArtifactDispatcher,
    hooks: &dyn VerifyArtifactHooks,
) -> Result<VerifyArtifactReport, VoomError> {
    if let Some(existing) = cp
        .artifacts
        .verification_for_workflow_lease(input.lease_id)
        .await?
    {
        require_matching_policy_verification(&existing, input)?;
        return report_from_verification(existing);
    }

    let expected = load_expected_artifact_facts(cp, input.target.artifact_handle_id).await?;
    require_matching_policy_expected_facts(&expected, &input.target)?;
    record_verification_started_for_worker(
        cp,
        input.target.artifact_handle_id,
        input.target.artifact_location_id,
        &input.target.path,
        input.worker_id,
    )
    .await?;
    let containment_root = Path::new(&input.target.path)
        .parent()
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
    let request = VerifyArtifactRequest {
        path: input.target.path.clone(),
        staging_root: containment_root.to_string_lossy().into_owned(),
        expected: VerifyArtifactExpectedFacts {
            size_bytes: expected.size_bytes,
            content_hash: expected.checksum.clone(),
            modified_at: None,
            local_file_key: None,
        },
    };
    let outcome = dispatcher
        .dispatch_verify_artifact(input.worker_id, request)
        .await
        .map_or_else(VerifyOutcome::Failed, VerifyOutcome::Succeeded);
    let context = VerifyArtifactPersistContext {
        artifact_handle_id: input.target.artifact_handle_id,
        artifact_location_id: input.target.artifact_location_id,
        worker_id: input.worker_id,
        path: &input.target.path,
        location_kind: "local_path",
        require_only_live_kind: false,
        workflow_ticket_id: Some(input.ticket_id),
        workflow_lease_id: Some(input.lease_id),
    };
    hooks.before_persist(cp, context).await?;
    persist_verification_outcome(cp, context, expected, outcome, hooks).await
}

#[derive(Debug, Clone)]
struct ExpectedArtifactFacts {
    size_bytes: u64,
    checksum: String,
}

fn require_matching_policy_expected_facts(
    expected: &ExpectedArtifactFacts,
    target: &PolicyArtifactTarget,
) -> Result<(), VoomError> {
    if expected.size_bytes != target.size_bytes || expected.checksum != target.checksum {
        return Err(VoomError::Conflict(format!(
            "artifact_handle {} facts changed after resolving file_version {}",
            target.artifact_handle_id, target.file_version_id
        )));
    }
    Ok(())
}

fn require_matching_policy_verification(
    verification: &ArtifactVerification,
    input: &PolicyVerifyArtifactInput,
) -> Result<(), VoomError> {
    if verification.workflow_ticket_id != Some(input.ticket_id)
        || verification.workflow_lease_id != Some(input.lease_id)
        || verification.artifact_handle_id != input.target.artifact_handle_id
        || verification.artifact_location_id != input.target.artifact_location_id
        || verification.path != input.target.path
        || verification.worker_id != input.worker_id
        || verification.expected_size_bytes != input.target.size_bytes
        || verification.expected_checksum != input.target.checksum
    {
        return Err(VoomError::Conflict(format!(
            "verification {} for workflow lease {} does not match its policy target",
            verification.id, input.lease_id
        )));
    }
    Ok(())
}

fn report_from_verification(
    verification: ArtifactVerification,
) -> Result<VerifyArtifactReport, VoomError> {
    Ok(VerifyArtifactReport {
        artifact_handle_id: verification.artifact_handle_id,
        artifact_location_id: verification.artifact_location_id,
        verification_id: verification.id,
        worker_id: verification.worker_id,
        status: verification.status,
        path: PathBuf::from(verification.path),
        expected_size_bytes: verification.expected_size_bytes,
        expected_checksum: verification.expected_checksum,
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
        .map_err(|e| VoomError::database_context("artifact_handles facts", e))?;
    let Some(row) = row else {
        return Err(VoomError::NotFound(format!(
            "artifact_handles {handle_id} missing"
        )));
    };
    let size_bytes: Option<i64> = sqlx::Row::try_get(&row, "size_bytes")
        .map_err(|e| VoomError::database_context("artifact_handles.size_bytes", e))?;
    let checksum: Option<String> = sqlx::Row::try_get(&row, "checksum")
        .map_err(|e| VoomError::database_context("artifact_handles.checksum", e))?;
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
            VoomError::database_context("artifact_handles.size_bytes negative", err)
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
    let mut tx = begin_immediate_tx(&cp.pool).await?;
    let worker = ensure_builtin_verify_artifact_worker_in_tx(cp, &mut tx).await?;
    append_verification_started_event(cp, &mut tx, handle_id, location_id, path, worker.id).await?;
    commit_tx(tx).await?;
    Ok(worker.id)
}

async fn record_verification_started_for_worker(
    cp: &ControlPlane,
    handle_id: ArtifactHandleId,
    location_id: ArtifactLocationId,
    path: &str,
    worker_id: WorkerId,
) -> Result<(), VoomError> {
    let mut tx = begin_immediate_tx(&cp.pool).await?;
    append_verification_started_event(cp, &mut tx, handle_id, location_id, path, worker_id).await?;
    commit_tx(tx).await
}

async fn append_verification_started_event(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    handle_id: ArtifactHandleId,
    location_id: ArtifactLocationId,
    path: &str,
    worker_id: WorkerId,
) -> Result<(), VoomError> {
    let now = cp.clock().now();
    append_event(
        &cp.events,
        tx,
        SubjectType::ArtifactHandle,
        Some(handle_id.0),
        now,
        Event::ArtifactVerificationStarted(ArtifactVerificationStartedPayload {
            artifact_handle_id: handle_id.0,
            artifact_location_id: location_id.0,
            worker_id: worker_id.0,
            path: path.to_owned(),
        }),
    )
    .await
}

async fn persist_verification_outcome(
    cp: &ControlPlane,
    context: VerifyArtifactPersistContext<'_>,
    expected: ExpectedArtifactFacts,
    outcome: VerifyOutcome,
    hooks: &dyn VerifyArtifactHooks,
) -> Result<VerifyArtifactReport, VoomError> {
    let mut tx = begin_immediate_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let outcome = validate_success_facts(&expected, outcome);
    let outcome = match revalidate_selected_live_location(
        &mut tx,
        context.artifact_handle_id,
        context.artifact_location_id,
        context.path,
        context.location_kind,
        context.require_only_live_kind,
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
    let input = new_verification_input(context, &expected, &outcome, now)?;
    let verification = cp
        .artifacts
        .record_verification_in_tx(&mut tx, input)
        .await?;
    hooks.before_terminal_event(context).await?;
    append_terminal_event(cp, &mut tx, &verification, &outcome, now, context).await?;
    commit_tx(tx).await?;

    report_from_verification(verification)
}

async fn revalidate_selected_live_location(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    handle_id: ArtifactHandleId,
    location_id: ArtifactLocationId,
    path: &str,
    expected_kind: &str,
    require_only_live_kind: bool,
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
    .map_err(|err| VoomError::database_context("artifact_locations verify selected", err))?;
    let Some((owner_id, kind, value, retired_at)) = selected else {
        return Err(VoomError::NotFound(format!(
            "artifact_locations {location_id} missing"
        )));
    };
    if u64::try_from(owner_id).ok() != Some(handle_id.0) || kind != expected_kind || value != path {
        return Err(VoomError::Conflict(format!(
            "artifact_locations {location_id} no longer matches artifact_handle {handle_id}"
        )));
    }
    if retired_at.is_some() {
        return Err(VoomError::Config(format!(
            "artifact_location {location_id} is no longer live {expected_kind}"
        )));
    }

    if !require_only_live_kind {
        return Ok(());
    }
    let live_locations: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, value FROM artifact_locations \
         WHERE artifact_handle_id = ? AND kind = ? AND retired_at IS NULL \
         ORDER BY id ASC",
    )
    .bind(i64::try_from(handle_id.0).map_err(|err| {
        VoomError::Internal(format!("artifact handle id exceeds SQLite integer: {err}"))
    })?)
    .bind(expected_kind)
    .fetch_all(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("artifact_locations verify live staging", err))?;
    let [(live_id, live_path)] = live_locations.as_slice() else {
        return Err(VoomError::Config(format!(
            "artifact_handle {handle_id} must still have exactly one live {expected_kind} \
             location; found {}",
            live_locations.len()
        )));
    };
    if u64::try_from(*live_id).ok() != Some(location_id.0) || live_path != path {
        return Err(VoomError::Config(format!(
            "artifact_handle {handle_id} live {expected_kind} location changed during verification"
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
    context: VerifyArtifactPersistContext<'_>,
    expected: &ExpectedArtifactFacts,
    outcome: &VerifyOutcome,
    now: time::OffsetDateTime,
) -> Result<NewArtifactVerification, VoomError> {
    match outcome {
        VerifyOutcome::Succeeded(result) => Ok(NewArtifactVerification {
            artifact_handle_id: context.artifact_handle_id,
            artifact_location_id: context.artifact_location_id,
            path: context.path.to_owned(),
            worker_id: context.worker_id,
            workflow_ticket_id: context.workflow_ticket_id,
            workflow_lease_id: context.workflow_lease_id,
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
            artifact_handle_id: context.artifact_handle_id,
            artifact_location_id: context.artifact_location_id,
            path: context.path.to_owned(),
            worker_id: context.worker_id,
            workflow_ticket_id: context.workflow_ticket_id,
            workflow_lease_id: context.workflow_lease_id,
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
