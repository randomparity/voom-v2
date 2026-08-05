use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::header::CONTENT_TYPE;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use http_body::{Body as HttpBody, Frame};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::bounded_router;
use crate::config::ServerLimits;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn test_limits() -> Result<ServerLimits, voom_core::VoomError> {
    ServerLimits::new_for_test(
        1024 * 1024,
        Duration::from_secs(30),
        Duration::from_secs(30),
        Duration::from_secs(30),
        Duration::from_secs(90),
        Duration::from_secs(30),
    )
}

fn boundary_router() -> Result<Router, voom_core::VoomError> {
    let router = Router::new()
        .route(
            "/body",
            post(|body: Bytes| async move { body.len().to_string() }),
        )
        .route(
            "/slow",
            get(|| async { std::future::pending::<()>().await }),
        )
        .route("/ok", get(|| async { "ok" }));
    Ok(bounded_router(router, test_limits()?))
}

async fn response_json(response: axum::response::Response) -> TestResult {
    let status = response.status();
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let body = response.into_body().collect().await?.to_bytes();
    let actual: Value = serde_json::from_slice(&body)?;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        content_type.as_ref().and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        actual,
        json!({
            "schema_version": "0",
            "command": "api.request",
            "status": "error",
            "data": null,
            "warnings": [],
            "error": {
                "code": "PAYLOAD_TOO_LARGE",
                "message": "request body exceeds the 1048576-byte limit",
                "hint": "Send a request body of 1048576 bytes or fewer"
            }
        })
    );
    Ok(())
}

async fn timeout_response_json(response: axum::response::Response) -> TestResult {
    let status = response.status();
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let body = response.into_body().collect().await?.to_bytes();
    let actual: Value = serde_json::from_slice(&body)?;

    assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        content_type.as_ref().and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        actual,
        json!({
            "schema_version": "0",
            "command": "api.request",
            "status": "error",
            "data": null,
            "warnings": [],
            "error": {
                "code": "REQUEST_TIMEOUT",
                "message": "request processing exceeded the 30-second deadline",
                "hint": "Retry a mutation with the same idempotency key if its outcome is unknown"
            }
        })
    );
    Ok(())
}

#[tokio::test]
async fn boundary_body_limit_uses_exact_json_envelope() -> TestResult {
    let request = Request::post("/body").body(Body::from(vec![0_u8; 1024 * 1024 + 1]))?;
    let response = boundary_router()?.oneshot(request).await?;

    response_json(response).await
}

#[tokio::test]
async fn boundary_pending_handler_uses_exact_timeout_envelope() -> TestResult {
    tokio::time::pause();
    let task = tokio::spawn(boundary_router()?.oneshot(Request::get("/slow").body(Body::empty())?));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;

    timeout_response_json(task.await??).await
}

#[tokio::test]
async fn boundary_pending_body_is_inside_processing_timeout() -> TestResult {
    tokio::time::pause();
    let task = tokio::spawn(
        boundary_router()?
            .oneshot(Request::post("/body").body(Body::new(OneFrameThenPending::new()))?),
    );
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;

    timeout_response_json(task.await??).await
}

#[tokio::test]
async fn boundary_normal_response_is_unchanged() -> TestResult {
    let response = boundary_router()?
        .oneshot(Request::get("/ok").body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = response.into_body().collect().await?.to_bytes();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"ok");
    Ok(())
}

struct OneFrameThenPending {
    yielded: bool,
}

impl OneFrameThenPending {
    const fn new() -> Self {
        Self { yielded: false }
    }
}

impl HttpBody for OneFrameThenPending {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.yielded {
            Poll::Pending
        } else {
            self.yielded = true;
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"partial")))))
        }
    }
}
