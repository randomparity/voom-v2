use std::marker::PhantomData;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use voom_core::ids::{ArtifactCommitIntentId, ArtifactCommitRecordId};
use voom_core::{
    ArtifactAccessMode, ArtifactHandleId, FailureClass, FileLocationId, FileVersionId, LeaseId,
    NodeId, NodeIncarnationEndReason, NodeIncarnationId, NodeIncarnationStatus, StorageRootId,
    TicketId, VoomError, WorkerId,
};

use crate::config::LoadedAgentConfig;

const RESPONSE_LIMIT_BYTES: usize = 1024 * 1024;
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const DEFAULT_MAXIMUM_ATTEMPTS: usize = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct RetryRequest<T> {
    body: Vec<u8>,
    idempotency_key: String,
    request_hash: String,
    marker: PhantomData<T>,
}

impl<T: Serialize> RetryRequest<T> {
    /// Serialize and freeze a replayable request.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when the body cannot be encoded as JSON.
    pub fn new(idempotency_key: String, body: &T) -> Result<Self, VoomError> {
        if idempotency_key.is_empty() {
            return Err(VoomError::Config(
                "idempotency key must not be empty".to_owned(),
            ));
        }
        let body = serde_json::to_vec(body)
            .map_err(|error| VoomError::Config(format!("serialize request: {error}")))?;
        let request_hash = sha256_hex(&body);
        Ok(Self {
            body,
            idempotency_key,
            request_hash,
            marker: PhantomData,
        })
    }
}

impl<T> RetryRequest<T> {
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    #[must_use]
    pub fn request_hash(&self) -> &str {
        &self.request_hash
    }
}

#[derive(Clone)]
pub struct ControlPlaneClient {
    http: reqwest::Client,
    base_url: reqwest::Url,
    node_token: SecretString,
    retry: RetrySettings,
}

#[derive(Debug, Clone, Copy)]
struct RetrySettings {
    initial_delay: Duration,
    maximum_delay: Duration,
    maximum_attempts: Option<usize>,
}

impl std::fmt::Debug for ControlPlaneClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlPlaneClient")
            .field("base_url", &self.base_url)
            .field("node_token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl ControlPlaneClient {
    /// Build a fail-closed API client from validated agent configuration.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when the URL, custom CA, or HTTP client is invalid.
    pub fn from_config(config: &LoadedAgentConfig) -> Result<Self, VoomError> {
        Self::from_config_with_settings(
            config,
            RetrySettings {
                initial_delay: INITIAL_RETRY_DELAY,
                maximum_delay: MAX_RETRY_DELAY,
                maximum_attempts: Some(DEFAULT_MAXIMUM_ATTEMPTS),
            },
            REQUEST_TIMEOUT,
        )
    }

    /// Build a client that retries indefinitely for exceptional operator-directed recovery.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when the URL, custom CA, or HTTP client is invalid.
    pub fn from_config_with_unbounded_retries(
        config: &LoadedAgentConfig,
    ) -> Result<Self, VoomError> {
        Self::from_config_with_settings(
            config,
            RetrySettings {
                initial_delay: INITIAL_RETRY_DELAY,
                maximum_delay: MAX_RETRY_DELAY,
                maximum_attempts: None,
            },
            REQUEST_TIMEOUT,
        )
    }

    fn from_config_with_settings(
        config: &LoadedAgentConfig,
        retry: RetrySettings,
        timeout: Duration,
    ) -> Result<Self, VoomError> {
        let base_url = reqwest::Url::parse(&config.config.control_plane_url).map_err(|error| {
            VoomError::Config(format!(
                "parse control_plane_url {:?}: {error}",
                config.config.control_plane_url
            ))
        })?;
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(path) = &config.config.ca_cert {
            let pem = std::fs::read(path).map_err(|error| {
                VoomError::Config(format!("read CA certificate {}: {error}", path.display()))
            })?;
            let certificates = reqwest::Certificate::from_pem_bundle(&pem).map_err(|error| {
                VoomError::Config(format!("parse CA certificate {}: {error}", path.display()))
            })?;
            if certificates.is_empty() {
                return Err(VoomError::Config(format!(
                    "CA certificate {} contains no certificates",
                    path.display()
                )));
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        let http = builder
            .build()
            .map_err(|error| VoomError::Config(format!("build node-agent HTTP client: {error}")))?;
        Ok(Self {
            http,
            base_url,
            node_token: config.node_token.clone(),
            retry,
        })
    }

    /// Send a frozen request until it succeeds or a terminal response is received.
    ///
    /// # Errors
    ///
    /// Returns a typed [`VoomError`] for terminal HTTP status, invalid envelopes, and
    /// bounded-response violations. Transport failures and 408/429/5xx responses retry.
    pub async fn send<TResponse, TBody>(
        &self,
        path: &str,
        request: &RetryRequest<TBody>,
    ) -> Result<TResponse, VoomError>
    where
        TResponse: DeserializeOwned,
    {
        let url = self.route_url(path)?;
        let mut attempt = 0_usize;
        let mut backoff = RetryBackoff::new(self.retry.initial_delay, self.retry.maximum_delay);
        loop {
            attempt += 1;
            let result = self.send_attempt(url.clone(), request).await;
            match result {
                AttemptResult::Complete(result) => return result,
                AttemptResult::Retry(error) => {
                    if self
                        .retry
                        .maximum_attempts
                        .is_some_and(|maximum| attempt >= maximum)
                    {
                        return Err(VoomError::ExternalSystemUnavailable(format!(
                            "control-plane request exhausted after {attempt} attempts: {error}"
                        )));
                    }
                    let delay = {
                        let mut rng = StdRng::from_os_rng();
                        backoff.next_delay(&mut rng)
                    };
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    async fn send_attempt<TResponse, TBody>(
        &self,
        url: reqwest::Url,
        request: &RetryRequest<TBody>,
    ) -> AttemptResult<TResponse>
    where
        TResponse: DeserializeOwned,
    {
        let response = self
            .http
            .post(url)
            .bearer_auth(self.node_token.expose_secret())
            .header("X-Voom-Idempotency-Key", request.idempotency_key())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request.body().to_vec())
            .send()
            .await;
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                return AttemptResult::Retry(VoomError::ExternalSystemUnavailable(format!(
                    "control-plane request failed: {error}"
                )));
            }
        };
        let status = response.status();
        if status == StatusCode::REQUEST_TIMEOUT
            || status == StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
        {
            return AttemptResult::Retry(retry_status_error(status));
        }
        let body = match read_bounded_body(&mut response).await {
            Ok(body) => body,
            Err(error) => return AttemptResult::Complete(Err(error)),
        };
        AttemptResult::Complete(decode_envelope(status, &body))
    }

    fn route_url(&self, path: &str) -> Result<reqwest::Url, VoomError> {
        if !path.starts_with('/') || path.starts_with("//") {
            return Err(VoomError::Config(format!(
                "control-plane route must be an absolute path; got {path:?}"
            )));
        }
        let url = self.base_url.join(path).map_err(|error| {
            VoomError::Config(format!("join control-plane route {path:?}: {error}"))
        })?;
        if url.origin() != self.base_url.origin() {
            return Err(VoomError::Config(format!(
                "control-plane route escaped configured origin: {path:?}"
            )));
        }
        Ok(url)
    }

    pub async fn activate(
        &self,
        node_id: NodeId,
        request: &RetryRequest<ActivateRequest>,
    ) -> Result<ActivateOutcome, VoomError> {
        self.send(
            &format!("/v1/execution/node/{}/activate", node_id.0),
            request,
        )
        .await
    }

    pub async fn deactivate(
        &self,
        node_id: NodeId,
        request: &RetryRequest<DeactivateRequest>,
    ) -> Result<DeactivateOutcome, VoomError> {
        self.send(
            &format!("/v1/execution/node/{}/deactivate", node_id.0),
            request,
        )
        .await
    }

    pub async fn node_heartbeat(
        &self,
        node_id: NodeId,
        request: &RetryRequest<NodeHeartbeatRequest>,
    ) -> Result<NodeHeartbeatOutcome, VoomError> {
        self.send(
            &format!("/v1/execution/node/{}/heartbeat", node_id.0),
            request,
        )
        .await
    }

    pub async fn acquire(
        &self,
        request: &RetryRequest<AcquireRequest>,
    ) -> Result<AcquireOutcome, VoomError> {
        self.send("/v1/execution/lease/acquire", request).await
    }

    pub async fn lease_heartbeat(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<LeaseHeartbeatRequest>,
    ) -> Result<LeaseHeartbeatOutcome, VoomError> {
        self.send(
            &format!("/v1/execution/lease/{}/heartbeat", lease_id.0),
            request,
        )
        .await
    }

    pub async fn complete(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<CompleteRequest>,
    ) -> Result<CompleteOutcome, VoomError> {
        self.send(
            &format!("/v1/execution/lease/{}/complete", lease_id.0),
            request,
        )
        .await
    }

    pub async fn fail(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<FailRequest>,
    ) -> Result<FailOutcome, VoomError> {
        self.send(&format!("/v1/execution/lease/{}/fail", lease_id.0), request)
            .await
    }

    pub async fn commit_open(
        &self,
        request: &RetryRequest<CommitOpenRequest>,
    ) -> Result<CommitOpenOutcome, VoomError> {
        self.send("/v1/artifact/commit/open", request).await
    }

    pub async fn authorize_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitAuthorizeRequest>,
    ) -> Result<CommitAuthorizeOutcome, VoomError> {
        self.send(
            &format!("/v1/artifact/commit/{}/authorize", intent_id.0),
            request,
        )
        .await
    }

    pub async fn report_commit_applying(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitApplyingRequest>,
    ) -> Result<CommitApplyingOutcome, VoomError> {
        self.send(
            &format!("/v1/artifact/commit/{}/applying", intent_id.0),
            request,
        )
        .await
    }

    pub async fn report_commit_outcome(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitOutcomeRequest>,
    ) -> Result<CommitReceiptOutcome, VoomError> {
        self.send(
            &format!("/v1/artifact/commit/{}/outcome", intent_id.0),
            request,
        )
        .await
    }

    pub async fn complete_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitCompleteRequest>,
    ) -> Result<CommitCompleteOutcome, VoomError> {
        self.send(
            &format!("/v1/artifact/commit/{}/complete", intent_id.0),
            request,
        )
        .await
    }
}

enum AttemptResult<T> {
    Complete(Result<T, VoomError>),
    Retry(VoomError),
}

struct RetryBackoff {
    ceiling: Duration,
    maximum: Duration,
}

impl RetryBackoff {
    const fn new(initial: Duration, maximum: Duration) -> Self {
        Self {
            ceiling: initial,
            maximum,
        }
    }

    fn next_delay(&mut self, rng: &mut impl Rng) -> Duration {
        let ceiling_nanos = u64::try_from(self.ceiling.as_nanos()).unwrap_or(u64::MAX);
        let delay = Duration::from_nanos(rng.random_range(0..=ceiling_nanos));
        self.ceiling = self.ceiling.saturating_mul(2).min(self.maximum);
        delay
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiEnvelope<T> {
    schema_version: String,
    command: String,
    status: String,
    data: Option<T>,
    warnings: Vec<String>,
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiError {
    code: String,
    message: String,
    hint: Option<String>,
}

async fn read_bounded_body(response: &mut reqwest::Response) -> Result<Vec<u8>, VoomError> {
    if response
        .content_length()
        .is_some_and(|length| length > RESPONSE_LIMIT_BYTES as u64)
    {
        return Err(VoomError::ExternalSystemUnavailable(format!(
            "control-plane response exceeds {RESPONSE_LIMIT_BYTES} bytes"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        VoomError::ExternalSystemUnavailable(format!("read control-plane response: {error}"))
    })? {
        if body.len().saturating_add(chunk.len()) > RESPONSE_LIMIT_BYTES {
            return Err(VoomError::ExternalSystemUnavailable(format!(
                "control-plane response exceeds {RESPONSE_LIMIT_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn decode_envelope<T: DeserializeOwned>(status: StatusCode, body: &[u8]) -> Result<T, VoomError> {
    let envelope: ApiEnvelope<T> = serde_json::from_slice(body).map_err(|error| {
        VoomError::ExternalSystemUnavailable(format!(
            "decode control-plane response envelope: {error}"
        ))
    })?;
    if envelope.schema_version != "0" {
        return Err(VoomError::ExternalSystemUnavailable(format!(
            "unsupported control-plane schema_version {:?}",
            envelope.schema_version
        )));
    }
    if status.is_success() {
        if envelope.status != "ok" || envelope.error.is_some() {
            return Err(VoomError::ExternalSystemUnavailable(format!(
                "control-plane command {:?} returned an inconsistent success envelope",
                envelope.command
            )));
        }
        return envelope.data.ok_or_else(|| {
            VoomError::ExternalSystemUnavailable(format!(
                "control-plane command {:?} omitted success data",
                envelope.command
            ))
        });
    }
    let error = envelope.error.ok_or_else(|| {
        VoomError::ExternalSystemUnavailable(format!(
            "control-plane command {:?} omitted error details for HTTP {status}",
            envelope.command
        ))
    })?;
    let message = if let Some(hint) = error.hint {
        format!("{} ({hint})", error.message)
    } else {
        error.message
    };
    let _warnings = envelope.warnings;
    Err(map_terminal_error(status, &error.code, message))
}

fn map_terminal_error(status: StatusCode, code: &str, message: String) -> VoomError {
    match (status, code) {
        (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN, _) | (_, "UNAUTHORIZED") => {
            VoomError::Unauthorized(message)
        }
        (StatusCode::CONFLICT, _) | (_, "CONFLICT") => VoomError::Conflict(message),
        (StatusCode::NOT_FOUND, _) | (_, "NOT_FOUND") => VoomError::NotFound(message),
        (StatusCode::BAD_REQUEST, _) | (_, "BAD_ARGS" | "CONFIG_INVALID") => {
            VoomError::Config(message)
        }
        _ => VoomError::ExternalSystemUnavailable(format!(
            "control-plane HTTP {status} ({code}): {message}"
        )),
    }
}

fn retry_status_error(status: StatusCode) -> VoomError {
    if status == StatusCode::TOO_MANY_REQUESTS {
        VoomError::ExternalSystemRateLimited(format!("control-plane HTTP {status}"))
    } else {
        VoomError::ExternalSystemUnavailable(format!("control-plane HTTP {status}"))
    }
}

fn sha256_hex(body: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(body);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerDeclaration {
    pub logical_name: String,
    pub operations: Vec<voom_core::OperationKind>,
    pub artifact_access: Vec<ArtifactAccessMode>,
    pub max_parallel: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivateRequest {
    pub incarnation_id: NodeIncarnationId,
    pub workers: Vec<WorkerDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivatedWorker {
    pub logical_name: String,
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivateOutcome {
    pub node_id: NodeId,
    pub node_epoch: u64,
    pub incarnation_id: NodeIncarnationId,
    pub heartbeat_ttl_seconds: u32,
    pub workers: Vec<ActivatedWorker>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeactivateRequest {
    pub incarnation_id: NodeIncarnationId,
    pub reason: NodeIncarnationEndReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeactivateOutcome {
    pub node_id: NodeId,
    pub node_epoch: u64,
    pub incarnation_id: NodeIncarnationId,
    pub status: NodeIncarnationStatus,
    pub reason: NodeIncarnationEndReason,
    pub retired_worker_ids: Vec<WorkerId>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeHeartbeatRequest {
    pub incarnation_id: NodeIncarnationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeHeartbeatOutcome {
    pub node_id: NodeId,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcquireRequest {
    pub node_id: NodeId,
    pub worker_id: WorkerId,
    pub incarnation_id: NodeIncarnationId,
    pub lease_ttl_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AcquireOutcome {
    Idle(AcquireIdle),
    NoCandidate(AcquireNoCandidate),
    Leased(LeaseDispatch),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquireIdle {
    pub worker_id: WorkerId,
    pub scheduler_decision_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquireNoCandidate {
    pub worker_id: WorkerId,
    pub scheduler_decision_id: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseDispatch {
    pub lease_id: LeaseId,
    pub scheduler_decision_id: u64,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub operation: String,
    pub dispatch_payload: JsonValue,
    pub lease_ttl_seconds: i64,
    pub heartbeat_after_seconds: i64,
    pub artifact_access_plan: ArtifactAccessPlan,
}

/// The dispatch plan's owner-local proof (ADR 0071). `owner_node_id` and
/// `access_evidence` are present together when the ticket declared byte work
/// and absent together otherwise; legacy mode/handle fields are rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAccessPlan {
    pub id: u64,
    pub owner_node_id: Option<u64>,
    pub access_evidence: Option<voom_core::OwnerAccessEvidence>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseHeartbeatRequest {
    pub node_id: NodeId,
    pub worker_id: WorkerId,
    pub incarnation_id: NodeIncarnationId,
    pub lease_ttl_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseHeartbeatOutcome {
    pub lease_id: LeaseId,
    pub worker_id: WorkerId,
    pub ttl_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteRequest {
    pub node_id: NodeId,
    pub worker_id: WorkerId,
    pub incarnation_id: NodeIncarnationId,
    pub result: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteOutcome {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub artifact_access_plan: ArtifactAccessPlan,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FailRequest {
    pub node_id: NodeId,
    pub worker_id: WorkerId,
    pub incarnation_id: NodeIncarnationId,
    pub reason: String,
    pub class: FailureClass,
    pub evidence: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailOutcome {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub artifact_access_plan: ArtifactAccessPlan,
}

/// Request body for `POST /v1/artifact/commit/open` (ADR 0074).
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitOpenRequest {
    pub node_id: NodeId,
    pub incarnation_id: NodeIncarnationId,
}

/// The pinned expected facts of a commit intent (size + content hash).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitExpectedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
}

/// One open commit intent from the listing for the caller's owned roots.
/// Never carries the fence.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCommitIntent {
    pub id: ArtifactCommitIntentId,
    pub state: String,
    pub artifact_handle_id: ArtifactHandleId,
    pub staging_storage_root_id: StorageRootId,
    pub staging_provider_relative_locator: String,
    pub staging_location_epoch: u64,
    /// Where the staged bytes come from: the pinned source rooted address
    /// this agent materializes staging from during `applying` (ADR 0075).
    pub source_storage_root_id: StorageRootId,
    pub source_provider_relative_locator: String,
    pub target_storage_root_id: StorageRootId,
    pub target_provider_relative_locator: String,
    pub target_root_epoch: u64,
    pub intent_epoch: u64,
    pub expected_facts: CommitExpectedFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitOpenOutcome {
    pub intents: Vec<OpenCommitIntent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitAuthorizeRequest {
    pub node_id: NodeId,
    pub incarnation_id: NodeIncarnationId,
}

/// Mirror of the fenced authorization payload returned by
/// `POST /v1/artifact/commit/{intent_id}/authorize`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitAuthorizeOutcome {
    pub intent_id: ArtifactCommitIntentId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub staging_storage_root_id: StorageRootId,
    pub staging_provider_relative_locator: String,
    pub target_storage_root_id: StorageRootId,
    pub target_provider_relative_locator: String,
    /// The pinned source rooted address staging is materialized from.
    pub source_storage_root_id: StorageRootId,
    pub source_provider_relative_locator: String,
    pub expected_size_bytes: u64,
    pub expected_content_hash: String,
    /// Hex-encoded one-time 32-byte commit fence. Never logged or re-sent
    /// anywhere except the matching complete request.
    pub fence_hex: String,
}

impl std::fmt::Debug for CommitAuthorizeOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The one-time fence is capability material: never leak it through
        // a log or telemetry surface.
        f.debug_struct("CommitAuthorizeOutcome")
            .field("intent_id", &self.intent_id)
            .field("commit_record_id", &self.commit_record_id)
            .field("staging_storage_root_id", &self.staging_storage_root_id)
            .field(
                "staging_provider_relative_locator",
                &self.staging_provider_relative_locator,
            )
            .field("target_storage_root_id", &self.target_storage_root_id)
            .field(
                "target_provider_relative_locator",
                &self.target_provider_relative_locator,
            )
            .field("source_storage_root_id", &self.source_storage_root_id)
            .field(
                "source_provider_relative_locator",
                &self.source_provider_relative_locator,
            )
            .field("expected_size_bytes", &self.expected_size_bytes)
            .field("expected_content_hash", &self.expected_content_hash)
            .field("fence_hex", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitApplyingRequest {
    pub node_id: NodeId,
    pub incarnation_id: NodeIncarnationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitApplyingOutcome {
    pub intent_id: ArtifactCommitIntentId,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitOutcomeRequest {
    pub node_id: NodeId,
    pub incarnation_id: NodeIncarnationId,
    pub evidence: CommitOutcomeEvidence,
}

/// Typed receipt evidence reported by the node (ADR 0074). The tag
/// discriminator rejects unknown variant names; each variant rejects unknown
/// fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitOutcomeEvidence {
    Applied(CommitAppliedEvidence),
    Mismatched(CommitMismatchedEvidence),
    OutcomeUnknown(CommitOutcomeUnknownEvidence),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitObservedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitAppliedEvidence {
    pub observed: CommitObservedFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitMismatchedEvidence {
    pub reason: String,
    pub observed: Option<CommitObservedFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitOutcomeUnknownEvidence {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitReceiptOutcome {
    pub intent_id: ArtifactCommitIntentId,
    pub kind: String,
}

#[derive(Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitCompleteRequest {
    pub node_id: NodeId,
    pub incarnation_id: NodeIncarnationId,
    pub fence_hex: String,
}

impl std::fmt::Debug for CommitCompleteRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitCompleteRequest")
            .field("node_id", &self.node_id)
            .field("incarnation_id", &self.incarnation_id)
            .field("fence_hex", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitCompleteOutcome {
    pub intent_id: ArtifactCommitIntentId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
