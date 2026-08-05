//! Process-level HTTP request bounds and listener lifecycle.

use axum::Router;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::Response;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use voom_core::ErrorCode;

use crate::config::ServerLimits;
use crate::err_response;

const TIMEOUT_MESSAGE: &str = "request processing exceeded the 30-second deadline";
const TIMEOUT_HINT: &str =
    "Retry a mutation with the same idempotency key if its outcome is unknown";
const BODY_LIMIT_MESSAGE: &str = "request body exceeds the 1048576-byte limit";
const BODY_LIMIT_HINT: &str = "Send a request body of 1048576 bytes or fewer";

/// Apply the fixed process-wide request bounds to an application router.
pub fn bounded_router(router: Router, limits: ServerLimits) -> Router {
    router
        .layer(RequestBodyLimitLayer::new(limits.max_request_body_bytes))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            limits.request_processing,
        ))
        .layer(middleware::map_response(normalize_boundary_response))
}

async fn normalize_boundary_response(response: Response) -> Response {
    match response.status() {
        StatusCode::REQUEST_TIMEOUT => err_response(
            StatusCode::REQUEST_TIMEOUT,
            "api.request",
            ErrorCode::RequestTimeout.as_str(),
            TIMEOUT_MESSAGE.to_owned(),
            Some(TIMEOUT_HINT.to_owned()),
        ),
        StatusCode::PAYLOAD_TOO_LARGE => err_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "api.request",
            ErrorCode::PayloadTooLarge.as_str(),
            BODY_LIMIT_MESSAGE.to_owned(),
            Some(BODY_LIMIT_HINT.to_owned()),
        ),
        _ => response,
    }
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
