//! Remote execution HTTP routes.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use secrecy::SecretString;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use voom_control_plane::ControlPlane;
use voom_control_plane::execution::{
    RemoteAcquireInput, RemoteActivateInput, RemoteCompleteInput, RemoteDeactivateInput,
    RemoteFailInput, RemoteLeaseHeartbeatInput, RemoteNodeHeartbeatInput, RemoteWorkerDeclaration,
    RemoteWorkerReadinessInput,
};
use voom_core::{
    ErrorCode, FailureClass, LeaseId, NodeId, NodeIncarnationEndReason, NodeIncarnationId,
    WorkerId, WorkerReadiness,
};

use crate::{
    AppState, bad_args_response, ok_response, unauthorized_response, voom_route_error_response,
};

const ACQUIRE_COMMAND: &str = "execution.acquire";
const ACTIVATE_COMMAND: &str = "execution.activate";
const DEACTIVATE_COMMAND: &str = "execution.deactivate";
const WORKER_READINESS_COMMAND: &str = "execution.worker_readiness";
const NODE_HEARTBEAT_COMMAND: &str = "execution.node_heartbeat";
const LEASE_HEARTBEAT_COMMAND: &str = "execution.lease_heartbeat";
const COMPLETE_COMMAND: &str = "execution.complete";
const FAIL_COMMAND: &str = "execution.fail";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AcquireRequest {
    node_id: u64,
    worker_id: u64,
    incarnation_id: NodeIncarnationId,
    #[serde(default = "default_lease_ttl_seconds")]
    lease_ttl_seconds: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NodeHeartbeatRequest {
    incarnation_id: NodeIncarnationId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ActivateRequest {
    incarnation_id: NodeIncarnationId,
    workers: Vec<RemoteWorkerDeclaration>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerReadinessRequest {
    incarnation_id: NodeIncarnationId,
    readiness: WorkerReadiness,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeactivateRequest {
    incarnation_id: NodeIncarnationId,
    reason: NodeIncarnationEndReason,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LeaseHeartbeatRequest {
    node_id: u64,
    worker_id: u64,
    incarnation_id: NodeIncarnationId,
    #[serde(default = "default_lease_ttl_seconds")]
    lease_ttl_seconds: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompleteRequest {
    node_id: u64,
    worker_id: u64,
    incarnation_id: NodeIncarnationId,
    result: JsonValue,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FailRequest {
    node_id: u64,
    worker_id: u64,
    incarnation_id: NodeIncarnationId,
    reason: String,
    class: FailureClass,
    #[serde(default)]
    evidence: JsonValue,
}

pub(crate) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/execution/node/{node_id}/activate", post(activate))
        .route("/v1/execution/node/{node_id}/deactivate", post(deactivate))
        .route(
            "/v1/execution/node/{node_id}/heartbeat",
            post(node_heartbeat),
        )
        .route(
            "/v1/execution/node/{node_id}/worker/{worker_id}/readiness",
            post(worker_readiness),
        )
        .route("/v1/execution/lease/acquire", post(acquire))
        .route(
            "/v1/execution/lease/{lease_id}/heartbeat",
            post(lease_heartbeat),
        )
        .route("/v1/execution/lease/{lease_id}/complete", post(complete))
        .route("/v1/execution/lease/{lease_id}/fail", post(fail))
}

async fn activate(
    State(state): State<AppState>,
    path: Result<Path<u64>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(ACTIVATE_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(ACTIVATE_COMMAND);
    };
    let node_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(ACTIVATE_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(ACTIVATE_COMMAND, error),
    };
    let request: ActivateRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(ACTIVATE_COMMAND, message),
    };
    let route_instance = format!("/v1/execution/node/{node_id}/activate");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(ACTIVATE_COMMAND, message),
    };

    match control_plane
        .remote_activate(RemoteActivateInput {
            node_id: NodeId(node_id),
            token,
            idempotency_key,
            request_hash,
            incarnation_id: request.incarnation_id,
            workers: request.workers,
        })
        .await
    {
        Ok(outcome) => ok_response(ACTIVATE_COMMAND, outcome),
        Err(err) => voom_route_error_response(ACTIVATE_COMMAND, &err),
    }
}

async fn worker_readiness(
    State(state): State<AppState>,
    path: Result<Path<(u64, u64)>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, _idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(WORKER_READINESS_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(WORKER_READINESS_COMMAND);
    };
    let (node_id, worker_id) = match path {
        Ok(Path(ids)) => ids,
        Err(error) => {
            return bad_args_response(
                WORKER_READINESS_COMMAND,
                format!("invalid route identifier: {error}"),
            );
        }
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(WORKER_READINESS_COMMAND, error),
    };
    let request: WorkerReadinessRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(WORKER_READINESS_COMMAND, message),
    };

    match control_plane
        .remote_worker_readiness(RemoteWorkerReadinessInput {
            node_id: NodeId(node_id),
            token,
            incarnation_id: request.incarnation_id,
            worker_id: WorkerId(worker_id),
            readiness: request.readiness,
        })
        .await
    {
        Ok(outcome) => ok_response(WORKER_READINESS_COMMAND, outcome),
        Err(error) => voom_route_error_response(WORKER_READINESS_COMMAND, &error),
    }
}

async fn deactivate(
    State(state): State<AppState>,
    path: Result<Path<u64>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(DEACTIVATE_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(DEACTIVATE_COMMAND);
    };
    let node_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(DEACTIVATE_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(DEACTIVATE_COMMAND, error),
    };
    let request: DeactivateRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(DEACTIVATE_COMMAND, message),
    };
    let route_instance = format!("/v1/execution/node/{node_id}/deactivate");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(DEACTIVATE_COMMAND, message),
    };

    match control_plane
        .remote_deactivate(RemoteDeactivateInput {
            node_id: NodeId(node_id),
            token,
            idempotency_key,
            request_hash,
            incarnation_id: request.incarnation_id,
            reason: request.reason,
        })
        .await
    {
        Ok(outcome) => ok_response(DEACTIVATE_COMMAND, outcome),
        Err(err) => voom_route_error_response(DEACTIVATE_COMMAND, &err),
    }
}

async fn acquire(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(ACQUIRE_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(ACQUIRE_COMMAND);
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(ACQUIRE_COMMAND, error),
    };
    let request: AcquireRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(ACQUIRE_COMMAND, message),
    };
    let request_hash = match stable_request_hash("POST", "/v1/execution/lease/acquire", &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(ACQUIRE_COMMAND, message),
    };

    match control_plane
        .remote_acquire(RemoteAcquireInput {
            node_id: NodeId(request.node_id),
            token,
            incarnation_id: request.incarnation_id,
            worker_id: WorkerId(request.worker_id),
            idempotency_key,
            request_hash,
            lease_ttl_seconds: request.lease_ttl_seconds,
        })
        .await
    {
        Ok(outcome) => ok_response(ACQUIRE_COMMAND, outcome),
        Err(err) => voom_route_error_response(ACQUIRE_COMMAND, &err),
    }
}

async fn node_heartbeat(
    State(state): State<AppState>,
    path: Result<Path<u64>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(NODE_HEARTBEAT_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(NODE_HEARTBEAT_COMMAND);
    };
    let node_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(NODE_HEARTBEAT_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(NODE_HEARTBEAT_COMMAND, error),
    };
    let request: NodeHeartbeatRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(NODE_HEARTBEAT_COMMAND, message),
    };
    let route_instance = format!("/v1/execution/node/{node_id}/heartbeat");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(NODE_HEARTBEAT_COMMAND, message),
    };

    match control_plane
        .remote_node_heartbeat(RemoteNodeHeartbeatInput {
            node_id: NodeId(node_id),
            token,
            incarnation_id: request.incarnation_id,
            idempotency_key,
            request_hash,
        })
        .await
    {
        Ok(outcome) => ok_response(NODE_HEARTBEAT_COMMAND, outcome),
        Err(err) => voom_route_error_response(NODE_HEARTBEAT_COMMAND, &err),
    }
}

async fn lease_heartbeat(
    State(state): State<AppState>,
    path: Result<Path<u64>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(LEASE_HEARTBEAT_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(LEASE_HEARTBEAT_COMMAND);
    };
    let lease_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(LEASE_HEARTBEAT_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(LEASE_HEARTBEAT_COMMAND, error),
    };
    let request: LeaseHeartbeatRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(LEASE_HEARTBEAT_COMMAND, message),
    };
    let route_instance = format!("/v1/execution/lease/{lease_id}/heartbeat");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(LEASE_HEARTBEAT_COMMAND, message),
    };

    match control_plane
        .remote_lease_heartbeat(RemoteLeaseHeartbeatInput {
            node_id: NodeId(request.node_id),
            token,
            incarnation_id: request.incarnation_id,
            worker_id: WorkerId(request.worker_id),
            lease_id: LeaseId(lease_id),
            idempotency_key,
            request_hash,
            lease_ttl_seconds: request.lease_ttl_seconds,
        })
        .await
    {
        Ok(outcome) => ok_response(LEASE_HEARTBEAT_COMMAND, outcome),
        Err(err) => voom_route_error_response(LEASE_HEARTBEAT_COMMAND, &err),
    }
}

async fn complete(
    State(state): State<AppState>,
    path: Result<Path<u64>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(COMPLETE_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(COMPLETE_COMMAND);
    };
    let lease_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(COMPLETE_COMMAND, error),
    };
    let request: CompleteRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };
    let route_instance = format!("/v1/execution/lease/{lease_id}/complete");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };

    match control_plane
        .remote_complete(RemoteCompleteInput {
            node_id: NodeId(request.node_id),
            token,
            incarnation_id: request.incarnation_id,
            worker_id: WorkerId(request.worker_id),
            lease_id: LeaseId(lease_id),
            idempotency_key,
            request_hash,
            result: request.result,
        })
        .await
    {
        Ok(outcome) => ok_response(COMPLETE_COMMAND, outcome),
        Err(err) => voom_route_error_response(COMPLETE_COMMAND, &err),
    }
}

async fn fail(
    State(state): State<AppState>,
    path: Result<Path<u64>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(FAIL_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(FAIL_COMMAND);
    };
    let lease_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(FAIL_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(FAIL_COMMAND, error),
    };
    let request: FailRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(FAIL_COMMAND, message),
    };
    let route_instance = format!("/v1/execution/lease/{lease_id}/fail");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(FAIL_COMMAND, message),
    };

    match control_plane
        .remote_fail(RemoteFailInput {
            node_id: NodeId(request.node_id),
            token,
            incarnation_id: request.incarnation_id,
            worker_id: WorkerId(request.worker_id),
            lease_id: LeaseId(lease_id),
            idempotency_key,
            request_hash,
            reason: request.reason,
            class: request.class,
            evidence: request.evidence,
        })
        .await
    {
        Ok(outcome) => ok_response(FAIL_COMMAND, outcome),
        Err(err) => voom_route_error_response(FAIL_COMMAND, &err),
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

fn credentials_error_response(
    command: &'static str,
    error: RequestCredentialsError,
) -> axum::response::Response {
    match error {
        RequestCredentialsError::Unauthorized => unauthorized_response(command),
        RequestCredentialsError::BadArgs(message) => bad_args_response(command, message),
    }
}

fn not_configured_response(command: &'static str) -> axum::response::Response {
    crate::err_response(
        StatusCode::NOT_FOUND,
        command,
        ErrorCode::NotFound.as_str(),
        "remote execution routes are not configured".to_owned(),
        None,
    )
}

enum JsonBodyError {
    PayloadTooLarge,
    BadArgs(String),
}

fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, JsonBodyError> {
    match body {
        Ok(Json(value)) => Ok(value),
        Err(error) if error.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            Err(JsonBodyError::PayloadTooLarge)
        }
        Err(error) => Err(JsonBodyError::BadArgs(format!(
            "invalid JSON body: {error}"
        ))),
    }
}

fn json_body_error_response(
    command: &'static str,
    error: JsonBodyError,
) -> axum::response::Response {
    match error {
        JsonBodyError::PayloadTooLarge => crate::server::payload_too_large_response(),
        JsonBodyError::BadArgs(message) => bad_args_response(command, message),
    }
}

fn parse_request_body<T: DeserializeOwned>(body: &JsonValue) -> Result<T, String> {
    serde_json::from_value(body.clone()).map_err(|err| format!("invalid JSON body: {err}"))
}

fn path_id(path: Result<Path<u64>, PathRejection>) -> Result<u64, String> {
    path.map(|Path(id)| id)
        .map_err(|err| format!("invalid path id: {err}"))
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
        .map_err(|e| format!("request hash serialization failed: {e}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

const fn default_lease_ttl_seconds() -> i64 {
    60
}

#[cfg(test)]
#[path = "execution_test.rs"]
mod tests;
