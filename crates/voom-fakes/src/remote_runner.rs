//! Remote synthetic runner that drives fake providers through VOOM's HTTP API.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use rand::RngCore;
use rand::SeedableRng;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use voom_core::{
    FailureClass, LeaseId, NodeId, NodeIncarnationId, OperationKind as ControlPlaneOperationKind,
    TicketId, WorkerId, WorkerReadiness,
};
use voom_fake_support::{dispatch_provider, provider_definition_for_operation};
use voom_worker_protocol::http::OperationBody;
use voom_worker_protocol::{OperationKind, OperationRequest, ProgressFrame, ProtocolError};

#[derive(Debug, Clone)]
pub struct RemoteRunnerConfig {
    pub base_url: String,
    pub node_id: NodeId,
    pub token: SecretString,
    pub worker_logical_name: String,
    pub operations: Vec<ControlPlaneOperationKind>,
    pub artifact_access: Vec<String>,
    pub max_parallel: u32,
    pub max_polls: u32,
    pub idle_timeout: Duration,
    pub lease_heartbeat_interval: Duration,
    pub lease_ttl_seconds: i64,
    pub healthy_heartbeat_ttl_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct RemoteWorkerConfig {
    pub logical_name: String,
    pub operations: Vec<ControlPlaneOperationKind>,
    pub artifact_access: Vec<String>,
    pub max_parallel: u32,
}

#[derive(Debug, Clone)]
pub struct RemoteNodeSessionConfig {
    pub base_url: String,
    pub node_id: NodeId,
    pub token: SecretString,
    pub workers: Vec<RemoteWorkerConfig>,
    pub max_polls: u32,
    pub idle_timeout: Duration,
    pub poll_interval: Duration,
    pub lease_ttl_seconds: i64,
    pub healthy_heartbeat_ttl_seconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionAction {
    Completed,
    Failed,
    StalledThenCompleted,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRecord {
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub worker_id: WorkerId,
    pub acquisition_ordinal: u32,
    pub action: ExecutionAction,
}

pub trait RemoteFaultPolicy: fmt::Debug + Send + Sync {
    fn action(&self, ticket_id: TicketId, acquisition_ordinal: u32) -> ExecutionAction;
}

#[derive(Debug)]
pub struct RemoteExecutionState {
    faults: Arc<dyn RemoteFaultPolicy>,
    inner: tokio::sync::Mutex<ExecutionStateInner>,
}

#[derive(Debug, Clone)]
pub struct RemoteNodeSession {
    config: RemoteNodeSessionConfig,
    executions: Arc<RemoteExecutionState>,
    recovery_gate: Arc<tokio::sync::RwLock<()>>,
    active: Arc<tokio::sync::Mutex<HashMap<WorkerId, (ActiveWorker, RemoteSyntheticRunner)>>>,
}

#[derive(Debug, Default)]
struct ExecutionStateInner {
    ordinals: HashMap<TicketId, u32>,
    records: Vec<ExecutionRecord>,
}

impl RemoteExecutionState {
    #[must_use]
    pub fn new(faults: Arc<dyn RemoteFaultPolicy>) -> Self {
        Self {
            faults,
            inner: tokio::sync::Mutex::new(ExecutionStateInner::default()),
        }
    }

    pub async fn record_acquisition(
        &self,
        ticket_id: TicketId,
        lease_id: LeaseId,
        worker_id: WorkerId,
    ) -> ExecutionRecord {
        let mut inner = self.inner.lock().await;
        let ordinal = inner.ordinals.entry(ticket_id).or_default();
        *ordinal += 1;
        let record = ExecutionRecord {
            ticket_id,
            lease_id,
            worker_id,
            acquisition_ordinal: *ordinal,
            action: self.faults.action(ticket_id, *ordinal),
        };
        inner.records.push(record.clone());
        record
    }

    pub async fn records(&self) -> Vec<ExecutionRecord> {
        self.inner.lock().await.records.clone()
    }
}

impl RemoteNodeSession {
    #[must_use]
    pub fn new(config: RemoteNodeSessionConfig, executions: Arc<RemoteExecutionState>) -> Self {
        Self {
            config,
            executions,
            recovery_gate: Arc::new(tokio::sync::RwLock::new(())),
            active: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    pub async fn recovery_guard(&self) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.recovery_gate.clone().write_owned().await
    }

    /// Heartbeat a held execution while the caller owns the recovery write guard.
    ///
    /// # Errors
    /// Returns an error when the worker is not active or the HTTP mutation fails.
    pub async fn heartbeat_execution(
        &self,
        record: &ExecutionRecord,
    ) -> Result<(), RemoteRunnerError> {
        let (worker, runner) = self
            .active
            .lock()
            .await
            .get(&record.worker_id)
            .cloned()
            .ok_or_else(|| {
                RemoteRunnerError::Protocol(format!(
                    "worker {} is not active in node session",
                    record.worker_id
                ))
            })?;
        runner
            .lease_heartbeat(
                &worker,
                record.lease_id,
                format!("stress-recovery-{}-{}", record.lease_id.0, new_run_id()),
            )
            .await
    }

    /// Activate every configured worker and run its polling lanes until stopped.
    ///
    /// # Errors
    /// Returns the first activation, HTTP, protocol, or joined-task failure.
    pub async fn run_until_stopped(
        &self,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<RemoteRunnerSummary, RemoteRunnerError> {
        let Some(first) = self.config.workers.first() else {
            return Err(RemoteRunnerError::Protocol(
                "remote node session requires at least one worker".to_owned(),
            ));
        };
        let controller = RemoteSyntheticRunner::new(self.runner_config(first));
        let incarnation_id = NodeIncarnationId::generate()
            .map_err(|error| RemoteRunnerError::Protocol(error.to_string()))?;
        let mut keys = IdempotencyKeys::new(&new_run_id());
        let active_workers = controller
            .activate_workers(incarnation_id, &self.config.workers, keys.next())
            .await?;
        for (worker_config, active_worker) in self
            .config
            .workers
            .iter()
            .zip(active_workers.iter().copied())
        {
            let runner = RemoteSyntheticRunner::new(self.runner_config(worker_config));
            runner.worker_readiness(&active_worker, keys.next()).await?;
            self.active
                .lock()
                .await
                .insert(active_worker.worker_id, (active_worker, runner));
        }

        let active = self.active.lock().await.clone();
        let mut tasks = Vec::new();
        for (worker, runner) in active.into_values() {
            for _ in 0..runner.config.max_parallel {
                let runner = runner.clone();
                let executions = self.executions.clone();
                let gate = self.recovery_gate.clone();
                let lane_stop = stop.clone();
                tasks.push(tokio::spawn(async move {
                    run_session_lane(runner, worker, executions, gate, lane_stop).await
                }));
            }
        }

        while !*stop.borrow() {
            if stop.changed().await.is_err() {
                break;
            }
        }
        let mut summary = RemoteRunnerSummary::default();
        for task in tasks {
            let lane = task.await.map_err(|error| {
                RemoteRunnerError::Protocol(format!("runner lane join: {error}"))
            })??;
            summary.acquired += lane.acquired;
            summary.completed += lane.completed;
            summary.failed += lane.failed;
            summary.idle_polls += lane.idle_polls;
        }
        Ok(summary)
    }

    fn runner_config(&self, worker: &RemoteWorkerConfig) -> RemoteRunnerConfig {
        RemoteRunnerConfig {
            base_url: self.config.base_url.clone(),
            node_id: self.config.node_id,
            token: self.config.token.clone(),
            worker_logical_name: worker.logical_name.clone(),
            operations: worker.operations.clone(),
            artifact_access: worker.artifact_access.clone(),
            max_parallel: worker.max_parallel,
            max_polls: self.config.max_polls,
            idle_timeout: self.config.idle_timeout,
            lease_heartbeat_interval: self.config.poll_interval,
            lease_ttl_seconds: self.config.lease_ttl_seconds,
            healthy_heartbeat_ttl_seconds: self.config.healthy_heartbeat_ttl_seconds,
        }
    }
}

async fn run_session_lane(
    runner: RemoteSyntheticRunner,
    active_worker: ActiveWorker,
    executions: Arc<RemoteExecutionState>,
    recovery_gate: Arc<tokio::sync::RwLock<()>>,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<RemoteRunnerSummary, RemoteRunnerError> {
    let mut summary = RemoteRunnerSummary::default();
    let mut keys = IdempotencyKeys::new(&new_run_id());
    while !*stop.borrow() {
        runner.node_heartbeat(&active_worker, keys.next()).await?;
        let acquire = {
            let _permit = recovery_gate.read().await;
            runner.acquire(&active_worker, keys.next()).await?
        };
        match acquire {
            AcquireOutcome::Idle { .. } => {
                summary.idle_polls += 1;
                tokio::time::sleep(runner.config.lease_heartbeat_interval).await;
            }
            AcquireOutcome::Leased(lease) => {
                summary.acquired += 1;
                let record = executions
                    .record_acquisition(lease.ticket_id, lease.lease_id, active_worker.worker_id)
                    .await;
                match record.action {
                    ExecutionAction::Abandoned => continue,
                    ExecutionAction::StalledThenCompleted => {
                        tokio::time::sleep(runner.config.lease_heartbeat_interval).await;
                        let _permit = recovery_gate.read().await;
                        runner
                            .lease_heartbeat(&active_worker, lease.lease_id, keys.next())
                            .await?;
                    }
                    ExecutionAction::Completed | ExecutionAction::Failed => {}
                }
                let dispatched =
                    RemoteSyntheticRunner::dispatch(&lease, &runner.config.artifact_access);
                match (record.action, dispatched) {
                    (ExecutionAction::Failed, _) => {
                        let _permit = recovery_gate.read().await;
                        runner
                            .fail(
                                &active_worker,
                                lease.lease_id,
                                FailureClass::MalformedWorkerResult,
                                "stress fault".to_owned(),
                                serde_json::json!({"stress_fault": true}),
                                keys.next(),
                            )
                            .await?;
                        summary.failed += 1;
                    }
                    (_, Ok(result)) => {
                        let _permit = recovery_gate.read().await;
                        runner
                            .complete(&active_worker, lease.lease_id, result, keys.next())
                            .await?;
                        summary.completed += 1;
                    }
                    (_, Err(error)) => {
                        let (class, reason, evidence) = classify_dispatch_error(&error);
                        let _permit = recovery_gate.read().await;
                        runner
                            .fail(
                                &active_worker,
                                lease.lease_id,
                                class,
                                reason,
                                evidence,
                                keys.next(),
                            )
                            .await?;
                        summary.failed += 1;
                    }
                }
            }
        }
    }
    Ok(summary)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteRunnerSummary {
    pub acquired: u32,
    pub completed: u32,
    pub failed: u32,
    pub idle_polls: u32,
}

#[derive(Debug)]
pub enum RemoteRunnerError {
    Http(String),
    Api { code: String, message: String },
    Protocol(String),
    UnsupportedOperation(String),
    MalformedResponse(String),
}

impl fmt::Display for RemoteRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(message) => write!(f, "http: {message}"),
            Self::Api { code, message } => write!(f, "api {code}: {message}"),
            Self::Protocol(message) => write!(f, "protocol: {message}"),
            Self::UnsupportedOperation(operation) => {
                write!(f, "unsupported remote operation: {operation}")
            }
            Self::MalformedResponse(message) => write!(f, "malformed response: {message}"),
        }
    }
}

impl Error for RemoteRunnerError {}

#[derive(Debug, Clone)]
pub struct RemoteSyntheticRunner {
    config: RemoteRunnerConfig,
    client: reqwest::Client,
}

impl RemoteSyntheticRunner {
    #[must_use]
    pub fn new(config: RemoteRunnerConfig) -> Self {
        let mut config = config;
        let base_url_len = config.base_url.trim_end_matches('/').len();
        config.base_url.truncate(base_url_len);
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Poll until one lease is terminal or the configured idle budget is spent.
    ///
    /// # Errors
    /// Returns HTTP, API-envelope, or fake-provider protocol failures.
    pub async fn run_once_to_completion(&self) -> Result<RemoteRunnerSummary, RemoteRunnerError> {
        let run_id = new_run_id();
        let mut keys = IdempotencyKeys::new(&run_id);
        let incarnation_id = NodeIncarnationId::generate()
            .map_err(|error| RemoteRunnerError::Protocol(error.to_string()))?;
        let active_worker = self.activate(incarnation_id, keys.next()).await?;
        self.worker_readiness(&active_worker, keys.next()).await?;
        let mut summary = RemoteRunnerSummary::default();
        let started = std::time::Instant::now();

        loop {
            self.node_heartbeat(&active_worker, keys.next()).await?;
            let acquire = self.acquire(&active_worker, keys.next()).await?;
            match acquire {
                AcquireOutcome::Idle { .. } => {
                    summary.idle_polls += 1;
                    if summary.idle_polls >= self.config.max_polls
                        || started.elapsed() >= self.config.idle_timeout
                    {
                        return Ok(summary);
                    }
                    tokio::time::sleep(self.config.lease_heartbeat_interval).await;
                }
                AcquireOutcome::Leased(lease) => {
                    summary.acquired += 1;
                    self.lease_heartbeat(&active_worker, lease.lease_id, keys.next())
                        .await?;
                    match Self::dispatch(&lease, &self.config.artifact_access) {
                        Ok(result) => {
                            self.complete(&active_worker, lease.lease_id, result, keys.next())
                                .await?;
                            summary.completed += 1;
                        }
                        Err(err) => {
                            let (class, reason, evidence) = classify_dispatch_error(&err);
                            self.fail(
                                &active_worker,
                                lease.lease_id,
                                class,
                                reason,
                                evidence,
                                keys.next(),
                            )
                            .await?;
                            summary.failed += 1;
                        }
                    }
                    return Ok(summary);
                }
            }
        }
    }

    async fn activate(
        &self,
        incarnation_id: NodeIncarnationId,
        idempotency_key: String,
    ) -> Result<ActiveWorker, RemoteRunnerError> {
        let workers = self
            .activate_workers(
                incarnation_id,
                &[RemoteWorkerConfig {
                    logical_name: self.config.worker_logical_name.clone(),
                    operations: self.config.operations.clone(),
                    artifact_access: self.config.artifact_access.clone(),
                    max_parallel: self.config.max_parallel,
                }],
                idempotency_key,
            )
            .await?;
        let [worker] = workers.as_slice() else {
            return Err(RemoteRunnerError::MalformedResponse(
                "activation response must contain exactly one worker".to_owned(),
            ));
        };
        Ok(*worker)
    }

    async fn activate_workers(
        &self,
        incarnation_id: NodeIncarnationId,
        workers: &[RemoteWorkerConfig],
        idempotency_key: String,
    ) -> Result<Vec<ActiveWorker>, RemoteRunnerError> {
        let outcome: RemoteActivateData = self
            .post(
                &format!("/v1/execution/node/{}/activate", self.config.node_id.0),
                &idempotency_key,
                serde_json::json!({
                    "incarnation_id": incarnation_id,
                    "workers": workers.iter().map(|worker| serde_json::json!({
                        "logical_name": worker.logical_name,
                        "operations": worker.operations,
                        "artifact_access": worker.artifact_access,
                        "accelerator": null,
                        "max_parallel": worker.max_parallel,
                    })).collect::<Vec<_>>(),
                }),
            )
            .await?;
        if outcome.node_id != self.config.node_id
            || outcome.incarnation_id != incarnation_id
            || outcome.node_epoch == 0
            || outcome.heartbeat_ttl_seconds == 0
        {
            return Err(RemoteRunnerError::MalformedResponse(
                "activation response identity does not match the request".to_owned(),
            ));
        }
        if outcome.workers.len() != workers.len() {
            return Err(RemoteRunnerError::MalformedResponse(
                "activation response worker count does not match the declaration".to_owned(),
            ));
        }
        workers
            .iter()
            .zip(outcome.workers)
            .map(|(declared, activated)| {
                if activated.logical_name != declared.logical_name {
                    return Err(RemoteRunnerError::MalformedResponse(
                        "activation response worker does not match the declaration".to_owned(),
                    ));
                }
                Ok(ActiveWorker {
                    incarnation_id,
                    worker_id: activated.worker_id,
                    _worker_epoch: activated.worker_epoch,
                })
            })
            .collect()
    }

    async fn worker_readiness(
        &self,
        active_worker: &ActiveWorker,
        idempotency_key: String,
    ) -> Result<(), RemoteRunnerError> {
        let outcome: RemoteWorkerReadinessData = self
            .post(
                &format!(
                    "/v1/execution/node/{}/worker/{}/readiness",
                    self.config.node_id.0, active_worker.worker_id.0
                ),
                &idempotency_key,
                serde_json::json!({
                    "incarnation_id": active_worker.incarnation_id,
                    "readiness": WorkerReadiness::Ready,
                }),
            )
            .await?;
        if outcome.node_id != self.config.node_id
            || outcome.incarnation_id != active_worker.incarnation_id
            || outcome.worker_id != active_worker.worker_id
            || outcome.readiness != WorkerReadiness::Ready
        {
            return Err(RemoteRunnerError::MalformedResponse(
                "worker readiness response identity does not match the request".to_owned(),
            ));
        }
        Ok(())
    }

    async fn node_heartbeat(
        &self,
        active_worker: &ActiveWorker,
        idempotency_key: String,
    ) -> Result<(), RemoteRunnerError> {
        let _: RemoteNodeHeartbeatData = self
            .post(
                &format!("/v1/execution/node/{}/heartbeat", self.config.node_id.0),
                &idempotency_key,
                serde_json::json!({
                    "incarnation_id": active_worker.incarnation_id,
                }),
            )
            .await?;
        Ok(())
    }

    async fn acquire(
        &self,
        active_worker: &ActiveWorker,
        idempotency_key: String,
    ) -> Result<AcquireOutcome, RemoteRunnerError> {
        self.post(
            "/v1/execution/lease/acquire",
            &idempotency_key,
            serde_json::json!({
                "node_id": self.config.node_id.0,
                "incarnation_id": active_worker.incarnation_id,
                "worker_id": active_worker.worker_id.0,
                "lease_ttl_seconds": self.config.lease_ttl_seconds,
            }),
        )
        .await
    }

    async fn lease_heartbeat(
        &self,
        active_worker: &ActiveWorker,
        lease_id: LeaseId,
        idempotency_key: String,
    ) -> Result<(), RemoteRunnerError> {
        let _: RemoteLeaseHeartbeatData = self
            .post(
                &format!("/v1/execution/lease/{}/heartbeat", lease_id.0),
                &idempotency_key,
                serde_json::json!({
                    "node_id": self.config.node_id.0,
                    "incarnation_id": active_worker.incarnation_id,
                    "worker_id": active_worker.worker_id.0,
                    "lease_ttl_seconds": self.config.healthy_heartbeat_ttl_seconds,
                }),
            )
            .await?;
        Ok(())
    }

    async fn complete(
        &self,
        active_worker: &ActiveWorker,
        lease_id: LeaseId,
        result: JsonValue,
        idempotency_key: String,
    ) -> Result<(), RemoteRunnerError> {
        let _: RemoteTerminalData = self
            .post(
                &format!("/v1/execution/lease/{}/complete", lease_id.0),
                &idempotency_key,
                serde_json::json!({
                    "node_id": self.config.node_id.0,
                    "incarnation_id": active_worker.incarnation_id,
                    "worker_id": active_worker.worker_id.0,
                    "result": result,
                }),
            )
            .await?;
        Ok(())
    }

    async fn fail(
        &self,
        active_worker: &ActiveWorker,
        lease_id: LeaseId,
        class: FailureClass,
        reason: String,
        evidence: JsonValue,
        idempotency_key: String,
    ) -> Result<(), RemoteRunnerError> {
        let _: RemoteTerminalData = self
            .post(
                &format!("/v1/execution/lease/{}/fail", lease_id.0),
                &idempotency_key,
                serde_json::json!({
                    "node_id": self.config.node_id.0,
                    "incarnation_id": active_worker.incarnation_id,
                    "worker_id": active_worker.worker_id.0,
                    "reason": reason,
                    "class": class,
                    "evidence": evidence,
                }),
            )
            .await?;
        Ok(())
    }

    async fn post<T>(
        &self,
        path: &str,
        idempotency_key: &str,
        body: JsonValue,
    ) -> Result<T, RemoteRunnerError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let url = format!("{}{}", self.config.base_url, path);
        let response = self
            .client
            .post(url)
            .bearer_auth(self.config.token.expose_secret())
            .header("x-voom-idempotency-key", idempotency_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| RemoteRunnerError::Http(e.to_string()))?;
        let envelope: ApiEnvelope<T> = response
            .json()
            .await
            .map_err(|e| RemoteRunnerError::Http(e.to_string()))?;
        if envelope.status == "ok" {
            return envelope.data.ok_or_else(|| {
                RemoteRunnerError::MalformedResponse("ok envelope missing data".to_owned())
            });
        }
        let err = envelope.error.ok_or_else(|| {
            RemoteRunnerError::MalformedResponse("error envelope missing error".to_owned())
        })?;
        Err(RemoteRunnerError::Api {
            code: err.code,
            message: err.message,
        })
    }

    fn dispatch(
        lease: &RemoteLeaseDispatch,
        artifact_access: &[String],
    ) -> Result<JsonValue, RemoteRunnerError> {
        let operation = operation_kind(&lease.operation)?;
        let provider = provider_definition_for_operation(operation)
            .ok_or_else(|| RemoteRunnerError::UnsupportedOperation(lease.operation.clone()))?;
        let request = OperationRequest {
            operation,
            lease_id: lease.lease_id,
            payload: dispatch_payload(lease, artifact_access)?,
            heartbeat_deadline_ms: u32::try_from(lease.lease_ttl_seconds.saturating_mul(1_000))
                .unwrap_or(u32::MAX),
            progress_idle_deadline_ms: u32::try_from(
                lease.heartbeat_after_seconds.saturating_mul(1_000),
            )
            .unwrap_or(u32::MAX),
        };
        let dispatch = dispatch_provider(&provider, &request)
            .map_err(|e| RemoteRunnerError::Protocol(e.to_string()))?;
        terminal_payload(dispatch.body, lease.lease_id)
    }
}

#[derive(Debug)]
struct IdempotencyKeys {
    run_id: String,
    sequence: u64,
}

impl IdempotencyKeys {
    fn new(run_id: &str) -> Self {
        Self {
            run_id: run_id.to_owned(),
            sequence: 0,
        }
    }

    fn next(&mut self) -> String {
        let key = format!("runner-{}-{}", self.run_id, self.sequence);
        self.sequence += 1;
        key
    }
}

#[derive(Debug, Clone, Copy)]
struct ActiveWorker {
    incarnation_id: NodeIncarnationId,
    worker_id: WorkerId,
    _worker_epoch: u64,
}

fn new_run_id() -> String {
    let mut rng = rand::rngs::StdRng::from_os_rng();
    let high = rng.next_u64();
    let low = rng.next_u64();
    format!("{high:016x}{low:016x}")
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    status: String,
    data: Option<T>,
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct RemoteNodeHeartbeatData {}

#[derive(Debug, Deserialize)]
struct RemoteActivateData {
    node_id: NodeId,
    node_epoch: u64,
    incarnation_id: NodeIncarnationId,
    heartbeat_ttl_seconds: u32,
    workers: Vec<ActivatedWorkerData>,
}

#[derive(Debug, Deserialize)]
struct ActivatedWorkerData {
    logical_name: String,
    worker_id: WorkerId,
    worker_epoch: u64,
}

#[derive(Debug, Deserialize)]
struct RemoteWorkerReadinessData {
    node_id: NodeId,
    incarnation_id: NodeIncarnationId,
    worker_id: WorkerId,
    readiness: WorkerReadiness,
}
#[derive(Debug, Deserialize)]
struct RemoteLeaseHeartbeatData {}

#[derive(Debug, Deserialize)]
struct RemoteTerminalData {}

#[derive(Debug, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum AcquireOutcome {
    Idle {},
    Leased(Box<RemoteLeaseDispatch>),
}

#[derive(Debug, Clone, Deserialize)]
struct RemoteLeaseDispatch {
    lease_id: LeaseId,
    ticket_id: TicketId,
    operation: String,
    dispatch_payload: JsonValue,
    lease_ttl_seconds: i64,
    heartbeat_after_seconds: i64,
    artifact_access_plan: RemoteArtifactAccessPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteArtifactAccessPlan {
    id: u64,
    owner_node_id: Option<u64>,
    access_evidence: Option<voom_core::OwnerAccessEvidence>,
}

fn operation_kind(operation: &str) -> Result<OperationKind, RemoteRunnerError> {
    serde_json::from_value(serde_json::json!(operation))
        .map_err(|e| RemoteRunnerError::MalformedResponse(format!("operation {operation}: {e}")))
}

fn dispatch_payload(
    lease: &RemoteLeaseDispatch,
    artifact_access: &[String],
) -> Result<JsonValue, RemoteRunnerError> {
    let mut payload = lease.dispatch_payload.clone();
    let object = payload.as_object_mut().ok_or_else(|| {
        RemoteRunnerError::MalformedResponse("dispatch payload must be an object".to_owned())
    })?;
    object.insert(
        "artifact_access_plan".to_owned(),
        serde_json::to_value(&lease.artifact_access_plan)
            .map_err(|e| RemoteRunnerError::MalformedResponse(e.to_string()))?,
    );
    object.insert(
        "advertised_artifact_access".to_owned(),
        serde_json::json!(artifact_access),
    );
    Ok(payload)
}

fn terminal_payload(
    body: OperationBody,
    lease_id: LeaseId,
) -> Result<JsonValue, RemoteRunnerError> {
    let bytes = match body {
        OperationBody::Buffered(bytes) => bytes,
        OperationBody::Streaming(_) => {
            return Err(RemoteRunnerError::Protocol(
                "streaming fake dispatch is not supported by remote runner yet".to_owned(),
            ));
        }
    };
    let mut terminal = None;
    for line in std::str::from_utf8(&bytes)
        .map_err(|e| RemoteRunnerError::Protocol(e.to_string()))?
        .lines()
    {
        let frame: ProgressFrame =
            serde_json::from_str(line).map_err(|e| RemoteRunnerError::Protocol(e.to_string()))?;
        if frame.lease_id() != lease_id {
            return Err(RemoteRunnerError::Protocol(format!(
                "wrong lease id in frame: expected {}, got {}",
                lease_id,
                frame.lease_id()
            )));
        }
        if let ProgressFrame::Result { payload, .. } = frame {
            terminal = Some(payload);
        }
    }
    terminal.ok_or_else(|| RemoteRunnerError::Protocol("missing terminal result frame".to_owned()))
}

fn classify_dispatch_error(err: &RemoteRunnerError) -> (FailureClass, String, JsonValue) {
    let reason = err.to_string();
    let class = match &err {
        RemoteRunnerError::Protocol(message) if message.contains("artifact access mode") => {
            FailureClass::ArtifactUnavailable
        }
        _ => FailureClass::MalformedWorkerResult,
    };
    (
        class,
        reason.clone(),
        serde_json::json!({
            "runner_error": reason,
        }),
    )
}

impl From<ProtocolError> for RemoteRunnerError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value.to_string())
    }
}

#[cfg(test)]
#[path = "remote_runner_test.rs"]
mod tests;
