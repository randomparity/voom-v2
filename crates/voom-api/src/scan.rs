//! Authenticated durable scan-session HTTP routes.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use secrecy::SecretString;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Iso8601;
use voom_control_plane::ControlPlane;
use voom_control_plane::scan::sessions::RemoteScanCompleteInput;
use voom_control_plane::scan::{
    RemoteScanBatchInput, RemoteScanFailInput, RemoteScanInspectInput,
    RemoteScanReconciliationInput, RemoteScanStartInput, RemoteScanStartOutcome,
    RemoteScanTerminalOutcome, ScanObservation, ScanReconciliationEvidence, ScanReconciliationPage,
    ScanSession,
};
use voom_core::{
    ErrorCode, FileLocationId, NodeId, NodeIncarnationId, ScanSessionId, ScanSessionStatus,
    ScanTerminalReason, StorageRootId, format_iso8601,
};

use crate::{
    AppState, bad_args_response, ok_response, unauthorized_response, voom_route_error_response,
};

const START_COMMAND: &str = "scan.start";
const BATCH_COMMAND: &str = "scan.batch";
const COMPLETE_COMMAND: &str = "scan.complete";
const FAIL_COMMAND: &str = "scan.fail";
const INSPECT_COMMAND: &str = "scan.inspect";
const RECONCILIATION_COMMAND: &str = "scan.reconciliation";
const DEFAULT_PAGE_LIMIT: u32 = 50;
const MAX_PAGE_LIMIT: u32 = 100;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    incarnation_id: NodeIncarnationId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BatchRequest {
    incarnation_id: NodeIncarnationId,
    observations: Vec<ScanObservationRequest>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScanObservationRequest {
    provider_relative_locator: voom_core::ProviderRelativeLocator,
    provider_object_identity: String,
    size_bytes: u64,
    modified_at: String,
    stability_started_at: String,
    stability_confirmed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<voom_core::ScanObservationEvidence>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompleteRequest {
    incarnation_id: NodeIncarnationId,
    last_sequence: Option<u64>,
    observation_count: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FailRequest {
    incarnation_id: NodeIncarnationId,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectQuery {
    incarnation_id: NodeIncarnationId,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconciliationQuery {
    incarnation_id: NodeIncarnationId,
    after_id: Option<u64>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct ScanSessionResponse {
    id: ScanSessionId,
    storage_root_id: StorageRootId,
    root_epoch: u64,
    owner_node_id: NodeId,
    owner_incarnation_id: Option<NodeIncarnationId>,
    status: ScanSessionStatus,
    next_sequence: u64,
    batch_count: u64,
    observation_count: u64,
    idle_timeout_seconds: u32,
    progress_deadline_at: String,
    location_high_watermark_id: Option<FileLocationId>,
    requested_at: String,
    started_at: Option<String>,
    terminal_at: Option<String>,
    terminal_reason: Option<String>,
    retired_location_count: u64,
}

#[derive(Debug, Serialize)]
struct ScanStartResponse {
    scan_session_id: ScanSessionId,
    status: ScanSessionStatus,
    owner_incarnation_id: NodeIncarnationId,
    location_high_watermark_id: Option<FileLocationId>,
    progress_deadline_at: String,
}

#[derive(Debug, Serialize)]
struct ScanTerminalResponse {
    scan_session_id: ScanSessionId,
    status: ScanSessionStatus,
    terminal_at: String,
    terminal_reason: String,
}

#[derive(Debug, Serialize)]
struct ReconciliationPageResponse {
    items: Vec<ReconciliationEvidenceResponse>,
    next_after_id: Option<FileLocationId>,
}

#[derive(Debug, Serialize)]
struct ReconciliationEvidenceResponse {
    file_location_id: FileLocationId,
    retired_at: String,
    prior_epoch: u64,
    retired_epoch: u64,
}

pub(crate) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/v1/scan/node/{node_id}/session/{session_id}/start",
            post(start),
        )
        .route(
            "/v1/scan/node/{node_id}/session/{session_id}/batch/{sequence}",
            post(batch),
        )
        .route(
            "/v1/scan/node/{node_id}/session/{session_id}/complete",
            post(complete),
        )
        .route(
            "/v1/scan/node/{node_id}/session/{session_id}/fail",
            post(fail),
        )
        .route("/v1/scan/node/{node_id}/session/{session_id}", get(inspect))
        .route(
            "/v1/scan/node/{node_id}/session/{session_id}/reconciliation",
            get(reconciliation),
        )
}

async fn start(
    State(state): State<AppState>,
    path: Result<Path<(u64, u64)>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(START_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(START_COMMAND);
    };
    let (node_id, session_id) = match scan_path(path) {
        Ok(path) => path,
        Err(message) => return bad_args_response(START_COMMAND, message),
    };
    let request = match request_body::<StartRequest>(body) {
        Ok(request) => request,
        Err(response) => return request_error_response(START_COMMAND, response),
    };
    let route = format!("/v1/scan/node/{node_id}/session/{session_id}/start");
    let request_hash = match stable_request_hash("POST", &route, &request) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(START_COMMAND, message),
    };
    match control_plane
        .start_scan_session(RemoteScanStartInput {
            node_id,
            scan_session_id: session_id,
            incarnation_id: request.incarnation_id,
            token,
            idempotency_key,
            request_hash,
        })
        .await
    {
        Ok(outcome) => ok_response(START_COMMAND, ScanStartResponse::from(outcome)),
        Err(error) => voom_route_error_response(START_COMMAND, &error),
    }
}

async fn batch(
    State(state): State<AppState>,
    path: Result<Path<(u64, u64, u64)>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(BATCH_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(BATCH_COMMAND);
    };
    let (node_id, session_id, sequence) = match batch_path(path) {
        Ok(path) => path,
        Err(message) => return bad_args_response(BATCH_COMMAND, message),
    };
    let request = match request_body::<BatchRequest>(body) {
        Ok(request) => request,
        Err(response) => return request_error_response(BATCH_COMMAND, response),
    };
    let observations = match typed_observations(&request) {
        Ok(observations) => observations,
        Err(message) => return bad_args_response(BATCH_COMMAND, message),
    };
    let route = format!("/v1/scan/node/{node_id}/session/{session_id}/batch/{sequence}");
    let request_hash = match stable_request_hash("POST", &route, &request) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(BATCH_COMMAND, message),
    };
    match control_plane
        .accept_scan_observation_batch(RemoteScanBatchInput {
            node_id,
            scan_session_id: session_id,
            incarnation_id: request.incarnation_id,
            token,
            idempotency_key,
            request_hash,
            sequence,
            observations,
        })
        .await
    {
        Ok(outcome) => ok_response(BATCH_COMMAND, outcome),
        Err(error) => voom_route_error_response(BATCH_COMMAND, &error),
    }
}

async fn complete(
    State(state): State<AppState>,
    path: Result<Path<(u64, u64)>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(COMPLETE_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(COMPLETE_COMMAND);
    };
    let (node_id, session_id) = match scan_path(path) {
        Ok(path) => path,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };
    let request = match request_body::<CompleteRequest>(body) {
        Ok(request) => request,
        Err(response) => return request_error_response(COMPLETE_COMMAND, response),
    };
    if let Err(message) = validate_complete_request(&request) {
        return bad_args_response(COMPLETE_COMMAND, message);
    }
    let route = format!("/v1/scan/node/{node_id}/session/{session_id}/complete");
    let request_hash = match stable_request_hash("POST", &route, &request) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };
    match control_plane
        .complete_scan_session(RemoteScanCompleteInput {
            node_id,
            scan_session_id: session_id,
            incarnation_id: request.incarnation_id,
            token,
            idempotency_key,
            request_hash,
            last_sequence: request.last_sequence,
            observation_count: request.observation_count,
        })
        .await
    {
        Ok(outcome) => ok_response(COMPLETE_COMMAND, outcome),
        Err(error) => voom_route_error_response(COMPLETE_COMMAND, &error),
    }
}

async fn fail(
    State(state): State<AppState>,
    path: Result<Path<(u64, u64)>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(FAIL_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(FAIL_COMMAND);
    };
    let (node_id, session_id) = match scan_path(path) {
        Ok(path) => path,
        Err(message) => return bad_args_response(FAIL_COMMAND, message),
    };
    let request = match request_body::<FailRequest>(body) {
        Ok(request) => request,
        Err(response) => return request_error_response(FAIL_COMMAND, response),
    };
    let route = format!("/v1/scan/node/{node_id}/session/{session_id}/fail");
    let request_hash = match stable_request_hash("POST", &route, &request) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(FAIL_COMMAND, message),
    };
    let reason = match ScanTerminalReason::new(request.reason) {
        Ok(reason) => reason,
        Err(error) => return bad_args_response(FAIL_COMMAND, error.to_string()),
    };
    match control_plane
        .fail_scan_session(RemoteScanFailInput {
            node_id,
            scan_session_id: session_id,
            incarnation_id: request.incarnation_id,
            token,
            idempotency_key,
            request_hash,
            reason,
        })
        .await
    {
        Ok(outcome) => ok_response(FAIL_COMMAND, ScanTerminalResponse::from(outcome)),
        Err(error) => voom_route_error_response(FAIL_COMMAND, &error),
    }
}

async fn inspect(
    State(state): State<AppState>,
    path: Result<Path<(u64, u64)>, PathRejection>,
    query: Result<Query<InspectQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    let Ok(token) = bearer(&headers) else {
        return unauthorized_response(INSPECT_COMMAND);
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(INSPECT_COMMAND);
    };
    let (node_id, session_id) = match scan_path(path) {
        Ok(path) => path,
        Err(message) => return bad_args_response(INSPECT_COMMAND, message),
    };
    let Query(query) = match query {
        Ok(query) => query,
        Err(error) => return bad_args_response(INSPECT_COMMAND, format!("invalid query: {error}")),
    };
    match control_plane
        .inspect_remote_scan_session(RemoteScanInspectInput {
            node_id,
            scan_session_id: session_id,
            incarnation_id: query.incarnation_id,
            token,
        })
        .await
    {
        Ok(session) => ok_response(INSPECT_COMMAND, ScanSessionResponse::from(session)),
        Err(error) => voom_route_error_response(INSPECT_COMMAND, &error),
    }
}

async fn reconciliation(
    State(state): State<AppState>,
    path: Result<Path<(u64, u64)>, PathRejection>,
    query: Result<Query<ReconciliationQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    let Ok(token) = bearer(&headers) else {
        return unauthorized_response(RECONCILIATION_COMMAND);
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(RECONCILIATION_COMMAND);
    };
    let (node_id, session_id) = match scan_path(path) {
        Ok(path) => path,
        Err(message) => return bad_args_response(RECONCILIATION_COMMAND, message),
    };
    let Query(query) = match query {
        Ok(query) => query,
        Err(error) => {
            return bad_args_response(RECONCILIATION_COMMAND, format!("invalid query: {error}"));
        }
    };
    let limit = match page_limit(query.limit) {
        Ok(limit) => limit,
        Err(message) => return bad_args_response(RECONCILIATION_COMMAND, message),
    };
    let after_id = match query.after_id.map(file_location_cursor).transpose() {
        Ok(after_id) => after_id,
        Err(message) => return bad_args_response(RECONCILIATION_COMMAND, message),
    };
    let input = RemoteScanReconciliationInput {
        auth: RemoteScanInspectInput {
            node_id,
            scan_session_id: session_id,
            incarnation_id: query.incarnation_id,
            token,
        },
        after_id,
        limit,
    };
    match control_plane
        .inspect_remote_scan_reconciliation(input)
        .await
    {
        Ok(page) => ok_response(
            RECONCILIATION_COMMAND,
            ReconciliationPageResponse::from(page),
        ),
        Err(error) => voom_route_error_response(RECONCILIATION_COMMAND, &error),
    }
}

fn configured_control_plane(state: AppState) -> Option<ControlPlane> {
    state.control_plane
}

enum RequestCredentialsError {
    Unauthorized,
    BadArgs(String),
}

fn request_credentials(
    headers: &HeaderMap,
) -> Result<(SecretString, String), RequestCredentialsError> {
    let token = bearer(headers).map_err(|_| RequestCredentialsError::Unauthorized)?;
    let key = idempotency_key(headers).map_err(RequestCredentialsError::BadArgs)?;
    Ok((token, key))
}

fn credentials_error_response(command: &'static str, error: RequestCredentialsError) -> Response {
    match error {
        RequestCredentialsError::Unauthorized => unauthorized_response(command),
        RequestCredentialsError::BadArgs(message) => bad_args_response(command, message),
    }
}

enum RequestBodyError {
    PayloadTooLarge,
    BadArgs(String),
}

fn request_body<T: DeserializeOwned>(
    body: Result<Json<JsonValue>, JsonRejection>,
) -> Result<T, RequestBodyError> {
    let value = match body {
        Ok(Json(value)) => value,
        Err(error) if error.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return Err(RequestBodyError::PayloadTooLarge);
        }
        Err(error) => {
            return Err(RequestBodyError::BadArgs(format!(
                "invalid JSON body: {error}"
            )));
        }
    };
    serde_json::from_value(value)
        .map_err(|error| RequestBodyError::BadArgs(format!("invalid JSON body: {error}")))
}

fn request_error_response(command: &'static str, error: RequestBodyError) -> Response {
    match error {
        RequestBodyError::PayloadTooLarge => crate::server::payload_too_large_response(),
        RequestBodyError::BadArgs(message) => bad_args_response(command, message),
    }
}

fn scan_path(
    path: Result<Path<(u64, u64)>, PathRejection>,
) -> Result<(NodeId, ScanSessionId), String> {
    let Path((node_id, session_id)) =
        path.map_err(|error| format!("invalid scan path: {error}"))?;
    require_storage_value(node_id, "node ID")?;
    require_storage_value(session_id, "scan session ID")?;
    Ok((NodeId(node_id), ScanSessionId(session_id)))
}

fn batch_path(
    path: Result<Path<(u64, u64, u64)>, PathRejection>,
) -> Result<(NodeId, ScanSessionId, u64), String> {
    let Path((node_id, session_id, sequence)) =
        path.map_err(|error| format!("invalid scan batch path: {error}"))?;
    require_storage_value(node_id, "node ID")?;
    require_storage_value(session_id, "scan session ID")?;
    require_storage_value(sequence, "scan batch sequence")?;
    Ok((NodeId(node_id), ScanSessionId(session_id), sequence))
}

fn bearer(headers: &HeaderMap) -> Result<SecretString, String> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or_else(|| "missing Authorization bearer token".to_owned())?
        .to_str()
        .map_err(|_| "Authorization header is not valid UTF-8".to_owned())?;
    let token = raw
        .strip_prefix("Bearer ")
        .ok_or_else(|| "Authorization header must use Bearer scheme".to_owned())?;
    if token.is_empty() {
        return Err("bearer token must not be empty".to_owned());
    }
    Ok(SecretString::from(token.to_owned()))
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, String> {
    let key = headers
        .get("x-voom-idempotency-key")
        .ok_or_else(|| "missing X-Voom-Idempotency-Key".to_owned())?
        .to_str()
        .map_err(|_| "X-Voom-Idempotency-Key is not valid UTF-8".to_owned())?;
    if key.is_empty() {
        return Err("X-Voom-Idempotency-Key must not be empty".to_owned());
    }
    Ok(key.to_owned())
}

fn stable_request_hash<T: Serialize>(
    method: &str,
    route_instance: &str,
    value: &T,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(method, route_instance, value))
        .map_err(|error| format!("request hash serialization failed: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn page_limit(limit: Option<u32>) -> Result<u32, String> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    if (1..=MAX_PAGE_LIMIT).contains(&limit) {
        Ok(limit)
    } else {
        Err(format!(
            "reconciliation limit {limit} outside 1..={MAX_PAGE_LIMIT}"
        ))
    }
}

fn file_location_cursor(value: u64) -> Result<FileLocationId, String> {
    require_storage_value(value, "reconciliation after_id")?;
    Ok(FileLocationId(value))
}

fn typed_observations(request: &BatchRequest) -> Result<Vec<ScanObservation>, String> {
    if !(1..=1_000).contains(&request.observations.len()) {
        return Err(format!(
            "scan batch observation count {} outside 1..=1000",
            request.observations.len()
        ));
    }
    request.observations.iter().map(typed_observation).collect()
}

fn typed_observation(request: &ScanObservationRequest) -> Result<ScanObservation, String> {
    let observation = ScanObservation {
        provider_relative_locator: request.provider_relative_locator.clone(),
        provider_object_identity: request.provider_object_identity.clone(),
        size_bytes: request.size_bytes,
        modified_at: parse_observation_time("modified_at", &request.modified_at)?,
        stability_started_at: parse_observation_time(
            "stability_started_at",
            &request.stability_started_at,
        )?,
        stability_confirmed_at: parse_observation_time(
            "stability_confirmed_at",
            &request.stability_confirmed_at,
        )?,
        evidence: request.evidence.clone(),
    };
    if let Some(evidence) = &observation.evidence {
        evidence.validate().map_err(|error| error.to_string())?;
    }
    validate_observation(&observation)?;
    Ok(observation)
}

fn parse_observation_time(field: &str, value: &str) -> Result<OffsetDateTime, String> {
    OffsetDateTime::parse(value, &Iso8601::DEFAULT)
        .map_err(|error| format!("scan observation {field} must be ISO-8601: {error}"))
}

fn validate_observation(observation: &ScanObservation) -> Result<(), String> {
    let identity = &observation.provider_object_identity;
    if identity.is_empty() || identity.len() > 4_096 || identity.as_bytes().contains(&0) {
        return Err(
            "scan observation object identity must be 1..=4096 bytes without NUL".to_owned(),
        );
    }
    require_storage_value(observation.size_bytes, "scan observation size")?;
    if observation.stability_confirmed_at < observation.stability_started_at {
        return Err("scan observation stability confirmation precedes start".to_owned());
    }
    Ok(())
}

fn validate_complete_request(request: &CompleteRequest) -> Result<(), String> {
    if let Some(last_sequence) = request.last_sequence {
        require_storage_value(last_sequence, "scan completion last sequence")?;
    }
    require_storage_value(
        request.observation_count,
        "scan completion observation count",
    )
}

fn require_storage_value(value: u64, field: &str) -> Result<(), String> {
    i64::try_from(value)
        .map(|_| ())
        .map_err(|error| format!("{field} {value} exceeds storage: {error}"))
}

fn not_configured_response(command: &'static str) -> Response {
    crate::err_response(
        StatusCode::NOT_FOUND,
        command,
        ErrorCode::NotFound.as_str(),
        "scan session routes are not configured".to_owned(),
        None,
    )
}

impl From<ScanSession> for ScanSessionResponse {
    fn from(session: ScanSession) -> Self {
        Self {
            id: session.id,
            storage_root_id: session.storage_root_id,
            root_epoch: session.root_epoch,
            owner_node_id: session.owner_node_id,
            owner_incarnation_id: session.owner_incarnation_id,
            status: session.status,
            next_sequence: session.next_sequence,
            batch_count: session.batch_count,
            observation_count: session.observation_count,
            idle_timeout_seconds: session.idle_timeout_seconds,
            progress_deadline_at: format_iso8601(session.progress_deadline_at),
            location_high_watermark_id: session.location_high_watermark_id,
            requested_at: format_iso8601(session.requested_at),
            started_at: session.started_at.map(format_iso8601),
            terminal_at: session.terminal_at.map(format_iso8601),
            terminal_reason: session
                .terminal_reason
                .map(|reason| reason.as_str().to_owned()),
            retired_location_count: session.retired_location_count,
        }
    }
}

impl From<RemoteScanStartOutcome> for ScanStartResponse {
    fn from(outcome: RemoteScanStartOutcome) -> Self {
        Self {
            scan_session_id: outcome.scan_session_id,
            status: outcome.status,
            owner_incarnation_id: outcome.owner_incarnation_id,
            location_high_watermark_id: outcome.location_high_watermark_id,
            progress_deadline_at: format_iso8601(outcome.progress_deadline_at),
        }
    }
}

impl From<RemoteScanTerminalOutcome> for ScanTerminalResponse {
    fn from(outcome: RemoteScanTerminalOutcome) -> Self {
        Self {
            scan_session_id: outcome.scan_session_id,
            status: outcome.status,
            terminal_at: format_iso8601(outcome.terminal_at),
            terminal_reason: outcome.terminal_reason.as_str().to_owned(),
        }
    }
}

impl From<ScanReconciliationPage> for ReconciliationPageResponse {
    fn from(page: ScanReconciliationPage) -> Self {
        Self {
            items: page
                .items
                .into_iter()
                .map(ReconciliationEvidenceResponse::from)
                .collect(),
            next_after_id: page.next_after_id,
        }
    }
}

impl From<ScanReconciliationEvidence> for ReconciliationEvidenceResponse {
    fn from(evidence: ScanReconciliationEvidence) -> Self {
        Self {
            file_location_id: evidence.file_location_id,
            retired_at: format_iso8601(evidence.retired_at),
            prior_epoch: evidence.prior_epoch,
            retired_epoch: evidence.retired_epoch,
        }
    }
}

#[cfg(test)]
#[path = "scan_test.rs"]
mod tests;
