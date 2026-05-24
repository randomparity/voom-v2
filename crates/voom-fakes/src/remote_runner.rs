//! Remote synthetic runner that drives fake providers through VOOM's HTTP API.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use rand::RngCore;
use rand::SeedableRng;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use voom_core::{FailureClass, LeaseId, WorkerId};
use voom_fake_support::{dispatch_provider, provider_definition_for_operation};
use voom_worker_protocol::http::OperationBody;
use voom_worker_protocol::{OperationKind, OperationRequest, ProgressFrame, ProtocolError};

#[derive(Debug, Clone)]
pub struct RemoteRunnerConfig {
    pub base_url: String,
    pub node_id: voom_core::NodeId,
    pub token: SecretString,
    pub worker_id: WorkerId,
    pub artifact_access: Vec<String>,
    pub max_polls: u32,
    pub idle_timeout: Duration,
    pub lease_heartbeat_interval: Duration,
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
        let mut keys = IdempotencyKeys::new(self.config.worker_id, &run_id);
        let mut summary = RemoteRunnerSummary::default();
        let started = std::time::Instant::now();

        loop {
            self.node_heartbeat(keys.next()).await?;
            let acquire = self.acquire(keys.next()).await?;
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
                    self.lease_heartbeat(lease.lease_id, keys.next()).await?;
                    match Self::dispatch(&lease, &self.config.artifact_access) {
                        Ok(result) => {
                            self.complete(lease.lease_id, result, keys.next()).await?;
                            summary.completed += 1;
                        }
                        Err(err) => {
                            let (class, reason, evidence) = classify_dispatch_error(&err);
                            self.fail(lease.lease_id, class, reason, evidence, keys.next())
                                .await?;
                            summary.failed += 1;
                        }
                    }
                    return Ok(summary);
                }
            }
        }
    }

    async fn node_heartbeat(&self, idempotency_key: String) -> Result<(), RemoteRunnerError> {
        let _: RemoteNodeHeartbeatData = self
            .post(
                &format!("/v1/execution/node/{}/heartbeat", self.config.node_id.0),
                &idempotency_key,
                serde_json::json!({}),
            )
            .await?;
        Ok(())
    }

    async fn acquire(&self, idempotency_key: String) -> Result<AcquireOutcome, RemoteRunnerError> {
        self.post(
            "/v1/execution/lease/acquire",
            &idempotency_key,
            serde_json::json!({
                "node_id": self.config.node_id.0,
                "worker_id": self.config.worker_id.0,
            }),
        )
        .await
    }

    async fn lease_heartbeat(
        &self,
        lease_id: LeaseId,
        idempotency_key: String,
    ) -> Result<(), RemoteRunnerError> {
        let _: RemoteLeaseHeartbeatData = self
            .post(
                &format!("/v1/execution/lease/{}/heartbeat", lease_id.0),
                &idempotency_key,
                serde_json::json!({
                    "node_id": self.config.node_id.0,
                    "worker_id": self.config.worker_id.0,
                }),
            )
            .await?;
        Ok(())
    }

    async fn complete(
        &self,
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
                    "worker_id": self.config.worker_id.0,
                    "result": result,
                }),
            )
            .await?;
        Ok(())
    }

    async fn fail(
        &self,
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
                    "worker_id": self.config.worker_id.0,
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
    worker_id: WorkerId,
    run_id: String,
    sequence: u64,
}

impl IdempotencyKeys {
    fn new(worker_id: WorkerId, run_id: &str) -> Self {
        Self {
            worker_id,
            run_id: run_id.to_owned(),
            sequence: 0,
        }
    }

    fn next(&mut self) -> String {
        let key = format!(
            "runner-{}-{}-{}",
            self.worker_id.0, self.run_id, self.sequence
        );
        self.sequence += 1;
        key
    }
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
    operation: String,
    dispatch_payload: JsonValue,
    lease_ttl_seconds: i64,
    heartbeat_after_seconds: i64,
    artifact_access_plan: RemoteArtifactAccessPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteArtifactAccessPlan {
    id: u64,
    input_handles: Vec<String>,
    output_handles: Vec<String>,
    selected_access_mode: String,
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
