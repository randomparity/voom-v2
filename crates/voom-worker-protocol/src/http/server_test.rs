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
            offered: 2,
            expected: 1,
        }
    ));
}

#[test]
fn enforce_version_missing_header_is_invalid_payload() {
    let headers = hyper::HeaderMap::new();
    let err = enforce_version(&headers).unwrap_err();
    assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
}
