use super::*;

#[test]
fn enforce_version_wrong_version_rejects() {
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        HeaderName::from_static(PROTOCOL_VERSION_HEADER),
        hyper::header::HeaderValue::from_str(&(voom_core::PROTOCOL_VERSION + 1).to_string())
            .unwrap(),
    );
    let err = enforce_version(&headers).unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::UnsupportedProtocolVersion {
            offered,
            expected,
        } if offered == voom_core::PROTOCOL_VERSION + 1
            && expected == voom_core::PROTOCOL_VERSION
    ));
}

#[test]
fn identity_route_is_read_only_and_unauthenticated() {
    assert_eq!(
        route_policy(&Method::POST, "/v1/identity"),
        Some(RoutePolicy {
            version: false,
            auth: false,
        })
    );
}

#[test]
fn enforce_version_missing_header_is_invalid_payload() {
    let headers = hyper::HeaderMap::new();
    let err = enforce_version(&headers).unwrap_err();
    assert!(matches!(
        &err,
        ProtocolError::InvalidPayload { detail } if detail.contains("missing")
    ));
}

#[test]
fn enforce_version_malformed_header_reports_malformed_not_missing() {
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        HeaderName::from_static(PROTOCOL_VERSION_HEADER),
        hyper::header::HeaderValue::from_static("1.0"),
    );
    let err = enforce_version(&headers).unwrap_err();
    assert!(
        matches!(
            &err,
            ProtocolError::InvalidPayload { detail }
                if detail.contains("malformed") && detail.contains("1.0") && !detail.contains("missing")
        ),
        "expected a malformed-value InvalidPayload, got {err:?}"
    );
}
