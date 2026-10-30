//! Control-plane / node-agent protocol version gate (ADR 0123).

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use voom_core::{ErrorCode, PROTOCOL_VERSION};

use crate::err_response;

/// Carries the sender's [`PROTOCOL_VERSION`] on node-route requests and responses.
pub(crate) const PROTOCOL_VERSION_HEADER: &str = "x-voom-protocol-version";

const REFUSAL_HINT: &str = "run the control plane and every node agent from one release; \
                            mixed protocol versions are refused (ADR 0123)";

/// Refuse a node-route request from another protocol version before the handler runs, and
/// stamp this control plane's version on every response.
pub(crate) async fn gate(request: Request, next: Next) -> Response {
    let mut response = match check_offered(request.headers()) {
        Ok(()) => next.run(request).await,
        Err(message) => err_response(
            StatusCode::BAD_REQUEST,
            "api.request",
            ErrorCode::BadArgs.as_str(),
            message,
            Some(REFUSAL_HINT.to_owned()),
        ),
    };
    response
        .headers_mut()
        .insert(PROTOCOL_VERSION_HEADER, HeaderValue::from(PROTOCOL_VERSION));
    response
}

fn check_offered(headers: &HeaderMap) -> Result<(), String> {
    let Some(raw) = headers.get(PROTOCOL_VERSION_HEADER) else {
        return Err(format!(
            "missing {PROTOCOL_VERSION_HEADER} (control plane speaks protocol {PROTOCOL_VERSION})"
        ));
    };
    let offered = raw
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| {
            format!("malformed {PROTOCOL_VERSION_HEADER}; expected a decimal version")
        })?;
    if offered != PROTOCOL_VERSION {
        return Err(format!(
            "unsupported protocol version: offered={offered}, expected {PROTOCOL_VERSION}"
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
