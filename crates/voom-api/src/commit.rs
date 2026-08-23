//! Fenced node-local commit intent HTTP routes (ADR 0074). Handler pattern
//! mirrors `execution.rs`: header credentials, stable request hash, shared
//! response envelopes.

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
use voom_control_plane::artifact_commit::{
    CommitOutcomeEvidence, RemoteCommitApplyingInput, RemoteCommitAuthorizeInput,
    RemoteCommitCompleteInput, RemoteCommitIntentsOpenInput, RemoteCommitOutcomeInput,
};
use voom_core::ids::ArtifactCommitIntentId;
use voom_core::{ErrorCode, NodeId, NodeIncarnationId};

use crate::{
    AppState, bad_args_response, ok_response, unauthorized_response, voom_route_error_response,
};

const OPEN_COMMAND: &str = "artifact.commit.open";
const AUTHORIZE_COMMAND: &str = "artifact.commit.authorize";
const APPLYING_COMMAND: &str = "artifact.commit.applying";
const OUTCOME_COMMAND: &str = "artifact.commit.outcome";
const COMPLETE_COMMAND: &str = "artifact.commit.complete";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    node_id: u64,
    incarnation_id: NodeIncarnationId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthorizeRequest {
    node_id: u64,
    incarnation_id: NodeIncarnationId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ApplyingRequest {
    node_id: u64,
    incarnation_id: NodeIncarnationId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OutcomeRequest {
    node_id: u64,
    incarnation_id: NodeIncarnationId,
    evidence: CommitOutcomeEvidence,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompleteRequest {
    node_id: u64,
    incarnation_id: NodeIncarnationId,
    /// Hex-encoded one-time 32-byte commit fence from the authorize outcome.
    fence_hex: String,
}

impl std::fmt::Debug for CompleteRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The one-time fence is capability material: never leak it through
        // a log or telemetry surface.
        f.debug_struct("CompleteRequest")
            .field("node_id", &self.node_id)
            .field("incarnation_id", &self.incarnation_id)
            .field("fence_hex", &"[REDACTED]")
            .finish()
    }
}

pub(crate) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/artifact/commit/open", post(open))
        .route("/v1/artifact/commit/{intent_id}/authorize", post(authorize))
        .route("/v1/artifact/commit/{intent_id}/applying", post(applying))
        .route("/v1/artifact/commit/{intent_id}/outcome", post(outcome))
        .route("/v1/artifact/commit/{intent_id}/complete", post(complete))
}

async fn open(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, _) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(OPEN_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(OPEN_COMMAND);
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(OPEN_COMMAND, error),
    };
    let request: OpenRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(OPEN_COMMAND, message),
    };

    match control_plane
        .remote_open_commit_intents(RemoteCommitIntentsOpenInput {
            node_id: NodeId(request.node_id),
            token,
            incarnation_id: request.incarnation_id,
        })
        .await
    {
        Ok(outcome) => ok_response(OPEN_COMMAND, outcome),
        Err(err) => voom_route_error_response(OPEN_COMMAND, &err),
    }
}

async fn authorize(
    State(state): State<AppState>,
    path: Result<Path<u64>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(AUTHORIZE_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(AUTHORIZE_COMMAND);
    };
    let intent_id = match path_intent_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(AUTHORIZE_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(AUTHORIZE_COMMAND, error),
    };
    let request: AuthorizeRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(AUTHORIZE_COMMAND, message),
    };
    let route_instance = format!("/v1/artifact/commit/{intent_id}/authorize");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(AUTHORIZE_COMMAND, message),
    };

    match control_plane
        .remote_authorize_commit_intent(RemoteCommitAuthorizeInput {
            intent_id: ArtifactCommitIntentId(intent_id),
            node_id: NodeId(request.node_id),
            token,
            incarnation_id: request.incarnation_id,
            idempotency_key,
            request_hash,
        })
        .await
    {
        Ok(outcome) => ok_response(AUTHORIZE_COMMAND, outcome),
        Err(err) => voom_route_error_response(AUTHORIZE_COMMAND, &err),
    }
}

async fn applying(
    State(state): State<AppState>,
    path: Result<Path<u64>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(APPLYING_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(APPLYING_COMMAND);
    };
    let intent_id = match path_intent_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(APPLYING_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(APPLYING_COMMAND, error),
    };
    let request: ApplyingRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(APPLYING_COMMAND, message),
    };
    let route_instance = format!("/v1/artifact/commit/{intent_id}/applying");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(APPLYING_COMMAND, message),
    };

    match control_plane
        .remote_report_commit_applying(RemoteCommitApplyingInput {
            intent_id: ArtifactCommitIntentId(intent_id),
            node_id: NodeId(request.node_id),
            token,
            incarnation_id: request.incarnation_id,
            idempotency_key,
            request_hash,
        })
        .await
    {
        Ok(outcome) => ok_response(APPLYING_COMMAND, outcome),
        Err(err) => voom_route_error_response(APPLYING_COMMAND, &err),
    }
}

async fn outcome(
    State(state): State<AppState>,
    path: Result<Path<u64>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<JsonValue>, JsonRejection>,
) -> axum::response::Response {
    let (token, idempotency_key) = match request_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(error) => return credentials_error_response(OUTCOME_COMMAND, error),
    };
    let Some(control_plane) = configured_control_plane(state) else {
        return not_configured_response(OUTCOME_COMMAND);
    };
    let intent_id = match path_intent_id(path) {
        Ok(id) => id,
        Err(message) => return bad_args_response(OUTCOME_COMMAND, message),
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(error) => return json_body_error_response(OUTCOME_COMMAND, error),
    };
    let request: OutcomeRequest = match parse_request_body(&body) {
        Ok(request) => request,
        Err(message) => return bad_args_response(OUTCOME_COMMAND, message),
    };
    let route_instance = format!("/v1/artifact/commit/{intent_id}/outcome");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(OUTCOME_COMMAND, message),
    };

    match control_plane
        .remote_report_commit_outcome(RemoteCommitOutcomeInput {
            intent_id: ArtifactCommitIntentId(intent_id),
            node_id: NodeId(request.node_id),
            token,
            incarnation_id: request.incarnation_id,
            idempotency_key,
            request_hash,
            evidence: request.evidence,
        })
        .await
    {
        Ok(outcome) => ok_response(OUTCOME_COMMAND, outcome),
        Err(err) => voom_route_error_response(OUTCOME_COMMAND, &err),
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
    let intent_id = match path_intent_id(path) {
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
    let route_instance = format!("/v1/artifact/commit/{intent_id}/complete");
    let request_hash = match stable_request_hash("POST", &route_instance, &body) {
        Ok(hash) => hash,
        Err(message) => return bad_args_response(COMPLETE_COMMAND, message),
    };

    match control_plane
        .remote_complete_commit_intent(RemoteCommitCompleteInput {
            intent_id: ArtifactCommitIntentId(intent_id),
            node_id: NodeId(request.node_id),
            token,
            incarnation_id: request.incarnation_id,
            idempotency_key,
            request_hash,
            fence_hex: request.fence_hex,
        })
        .await
    {
        Ok(outcome) => ok_response(COMPLETE_COMMAND, outcome),
        Err(err) => voom_route_error_response(COMPLETE_COMMAND, &err),
    }
}

fn configured_control_plane(state: AppState) -> Option<voom_control_plane::ControlPlane> {
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
        "remote commit intent routes are not configured".to_owned(),
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

fn path_intent_id(path: Result<Path<u64>, PathRejection>) -> Result<u64, String> {
    path.map(|Path(id)| id)
        .map_err(|err| format!("invalid path intent id: {err}"))
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

#[cfg(test)]
#[path = "commit_test.rs"]
mod tests;
