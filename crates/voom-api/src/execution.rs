//! Remote execution HTTP routes.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use secrecy::SecretString;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use voom_control_plane::cases::remote_execution::RemoteAcquireInput;
use voom_core::{NodeId, WorkerId};

use crate::{AppState, bad_args_response, ok_response, voom_route_error_response};

const ACQUIRE_COMMAND: &str = "execution.acquire";

#[derive(Debug, Deserialize, Serialize)]
struct AcquireRequest {
    node_id: u64,
    worker_id: u64,
    #[serde(default = "default_lease_ttl_seconds")]
    lease_ttl_seconds: i64,
}

pub(crate) fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/v1/execution/lease/acquire", post(acquire))
}

async fn acquire(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<JsonValue>,
) -> axum::response::Response {
    let Some(control_plane) = state.control_plane else {
        return (
            StatusCode::NOT_FOUND,
            "remote execution routes are not configured",
        )
            .into_response();
    };
    let token = match bearer(&headers) {
        Ok(token) => token,
        Err(message) => return bad_args_response(ACQUIRE_COMMAND, message),
    };
    let idempotency_key = match idempotency_key(&headers) {
        Ok(key) => key,
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
