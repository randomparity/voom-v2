use super::*;

#[test]
fn auth_headers_include_protocol_worker_and_idempotency() {
    let creds = voom_worker_protocol::WorkerCredentials {
        worker_id: voom_core::WorkerId(1),
        worker_epoch: 0,
        secret: secrecy::SecretString::from("secret"),
    };
    let headers = auth_headers(&creds, "abc");
    assert!(
        headers
            .iter()
            .any(|(k, _)| *k == "X-Voom-Protocol-Version")
    );
    assert!(headers.iter().any(|(k, _)| *k == "Authorization"));
    assert!(headers.iter().any(|(k, _)| *k == "X-Voom-Idempotency-Key"));
}

#[test]
fn malformed_json_body_is_not_valid_json() {
    assert!(serde_json::from_slice::<serde_json::Value>(malformed_json_body()).is_err());
}

#[test]
fn raw_response_parser_decodes_protocol_error_body() {
    let body = serde_json::to_vec(&voom_worker_protocol::ProtocolError::UnauthorizedBearer)
        .unwrap();
    let raw = [
        b"HTTP/1.1 401 Unauthorized\r\ncontent-length: ".as_slice(),
        body.len().to_string().as_bytes(),
        b"\r\n\r\n",
        &body,
    ]
    .concat();
    let parsed = RawHttpResponse::parse(&raw).unwrap();
    let err = parsed.protocol_error().unwrap();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::UnauthorizedBearer
    ));
}
