//! Remote execution HTTP routes.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use secrecy::SecretString;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use voom_control_plane::ControlPlane;
use voom_control_plane::execution::{
    RemoteAcquireInput, RemoteCompleteInput, RemoteFailInput, RemoteLeaseHeartbeatInput,
    RemoteNodeHeartbeatInput,
};
use voom_core::{ErrorCode, FailureClass, LeaseId, NodeId, WorkerId};

use crate::{AppState, bad_args_response, ok_response, voom_route_error_response};

const ACQUIRE_COMMAND: &str = "execution.acquire";
const NODE_HEARTBEAT_COMMAND: &str = "execution.node_heartbeat";
const LEASE_HEARTBEAT_COMMAND: &str = "execution.lease_heartbeat";
const COMPLETE_COMMAND: &str = "execution.complete";
const FAIL_COMMAND: &str = "execution.fail";

#[derive(Debug, Deserialize, Serialize)]
struct AcquireRequest {
    node_id: u64,
    worker_id: u64,
    #[serde(default = "default_lease_ttl_seconds")]
    lease_ttl_seconds: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct LeaseHeartbeatRequest {
    node_id: u64,
    worker_id: u64,
    #[serde(default = "default_lease_ttl_seconds")]
    lease_ttl_seconds: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct CompleteRequest {
    node_id: u64,
    worker_id: u64,
    result: JsonValue,
}

#[derive(Debug, Deserialize, Serialize)]
struct FailRequest {
    node_id: u64,
    worker_id: u64,
    reason: String,
    class: FailureClass,
    #[serde(default)]
    evidence: JsonValue,
}

pub(crate) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/v1/execution/node/{node_id}/heartbeat",
            post(node_heartbeat),
        )
        .route("/v1/execution/lease/acquire", post(acquire))
        .route(
            "/v1/execution/lease/{lease_id}/heartbeat",
            post(lease_heartbeat),
        )
        .route("/v1/execution/lease/{lease_id}/complete", post(complete))
        .route("/v1/execution/lease/{lease_id}/fail", post(fail))
}

async fn acquire(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(ACQUIRE_COMMAND);
    };
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(message) => return bad_args_response(ACQUIRE_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(message) => return bad_args_response(ACQUIRE_COMMAND, message),
    };
    let request_hash = match stable_request_hash("POST", "/v1/execution/lease/acquire", &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(ACQUIRE_COMMAND, message),
    };
    let request: AcquireRequest = match serde_json::from_value(body) {
        Ok(request) => request,
        Err(err) => return bad_args_response(ACQUIRE_COMMAND, format!("invalid JSON body: {err}")),
    };

    match control_plane
        .remote_acquire(RemoteAcquireInput {
            node_id: NodeId(request.node_id),
            token,
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
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(NODE_HEARTBEAT_COMMAND);
    };
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(message) => return bad_args_response(NODE_HEARTBEAT_COMMAND, message),
    };
    let node_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(NODE_HEARTBEAT_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
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
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(LEASE_HEARTBEAT_COMMAND);
    };
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(message) => return bad_args_response(LEASE_HEARTBEAT_COMMAND, message),
    };
    let lease_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(LEASE_HEARTBEAT_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(message) => return bad_args_response(LEASE_HEARTBEAT_COMMAND, message),
    };
    let route_instance = format!("/v1/execution/lease/{lease_id}/heartbeat");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(LEASE_HEARTBEAT_COMMAND, message),
    };
    let request: LeaseHeartbeatRequest = match serde_json::from_value(body) {
        Ok(request) => request,
        Err(err) => {
            return bad_args_response(LEASE_HEARTBEAT_COMMAND, format!("invalid JSON body: {err}"));
        }
    };

    match control_plane
        .remote_lease_heartbeat(RemoteLeaseHeartbeatInput {
            node_id: NodeId(request.node_id),
            token,
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
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(COMPLETE_COMMAND);
    };
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };
    let lease_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };
    let route_instance = format!("/v1/execution/lease/{lease_id}/complete");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };
    let request: CompleteRequest = match serde_json::from_value(body) {
        Ok(request) => request,
        Err(err) => {
            return bad_args_response(COMPLETE_COMMAND, format!("invalid JSON body: {err}"));
        }
    };

    match control_plane
        .remote_complete(RemoteCompleteInput {
            node_id: NodeId(request.node_id),
            token,
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
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(FAIL_COMMAND);
    };
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(message) => return bad_args_response(FAIL_COMMAND, message),
    };
    let lease_id = match path_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(FAIL_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(message) => return bad_args_response(FAIL_COMMAND, message),
    };
    let route_instance = format!("/v1/execution/lease/{lease_id}/fail");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(FAIL_COMMAND, message),
    };
    let request: FailRequest = match serde_json::from_value(body) {
        Ok(request) => request,
        Err(err) => return bad_args_response(FAIL_COMMAND, format!("invalid JSON body: {err}")),
    };

    match control_plane
        .remote_fail(RemoteFailInput {
            node_id: NodeId(request.node_id),
            token,
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

fn request_credentials(headers: &HeaderMap) -> Result<(SecretString, String), String> {
    let token = bearer(headers)?;
    let key = idempotency_key(headers)?;
    Ok((token, key))
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

fn json_body(body: Result<Json<JsonValue>, JsonRejection>) -> Result<JsonValue, String> {
    body.map(|Json(value)| value)
        .map_err(|err| format!("invalid JSON body: {err}"))
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
