//! Unit tests for the shared HTTP error mappers.

use axum::http::StatusCode;
use voom_core::VoomError;

use super::voom_route_error_response;

const CMD: &str = "execution.acquire";

#[test]
fn route_error_maps_db_down_family_to_service_unavailable() {
    // H4 regression: a database outage during an execution route is a
    // dependency failure (503 — retry the dependency), not a client error
    // or an unclassified internal fault. All four DB-down codes must map to
    // Service Unavailable so callers don't read "your request was wrong".
    let db_down = [
        VoomError::database("disk I/O error"),
        VoomError::Migration("schema_meta missing".to_owned()),
        VoomError::DirtyMigration("failed migration row present".to_owned()),
        VoomError::SchemaTooNew("db newer than this binary".to_owned()),
    ];
    for err in &db_down {
        let resp = voom_route_error_response(CMD, err);
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "expected 503 for {err:?}"
        );
    }
}

#[test]
fn route_error_keeps_internal_failures_at_500() {
    // The fix must not over-broaden: a genuine internal fault still maps to
    // 500, not 503.
    let resp = voom_route_error_response(CMD, &VoomError::Internal("boom".to_owned()));
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn route_error_keeps_not_found_at_404() {
    let resp = voom_route_error_response(CMD, &VoomError::NotFound("lease 7".to_owned()));
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
