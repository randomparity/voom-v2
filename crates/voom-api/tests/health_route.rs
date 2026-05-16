#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use voom_api::router;
use voom_control_plane::ControlPlane;

async fn fixture_uninit() -> (tempfile::NamedTempFile, axum::Router) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::connect_or_create(&url).await.unwrap();
    let cp = ControlPlane::open(url).await.unwrap();
    (tmp, router(cp))
}

async fn fixture_initialized() -> (tempfile::NamedTempFile, axum::Router) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let cp = ControlPlane::open(url).await.unwrap();
    (tmp, router(cp))
}

#[tokio::test]
async fn health_on_uninitialized_returns_503_db_uninitialized() {
    let (_keep, app) = fixture_uninit().await;
    let res = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "DB_UNINITIALIZED");
    assert!(json.get("local").is_none(), "API must NEVER include local block");
}

#[tokio::test]
async fn health_on_initialized_returns_200_current() {
    let (_keep, app) = fixture_initialized().await;
    let res = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["db"]["status"], "current");
    assert_eq!(json["data"]["db"]["migration_count"], 1);
    assert!(json.get("local").is_none(), "API must NEVER include local block");
}

#[tokio::test]
async fn health_on_too_new_db_returns_503_db_schema_too_new() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());

    voom_store::init(&url).await.unwrap();
    {
        let pool = voom_store::connect(&url).await.unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    let cp = ControlPlane::open(url).await.unwrap();
    let app = router(cp);
    let res = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "DB_SCHEMA_TOO_NEW");
    assert!(json.get("local").is_none(), "API must NEVER include local block");
}
