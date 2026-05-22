use super::*;

#[test]
fn operation_body_uses_manifest_operation_and_payload() {
    let body = operation_body(
        9,
        voom_worker_protocol::OperationKind::Remux,
        serde_json::json!({"path": "/library/example.mkv", "container": "mkv"}),
    )
    .unwrap();
    let req: voom_worker_protocol::OperationRequest = serde_json::from_slice(&body).unwrap();
    assert_eq!(req.operation, voom_worker_protocol::OperationKind::Remux);
    assert_eq!(
        req.payload,
        serde_json::json!({"path": "/library/example.mkv", "container": "mkv"})
    );
}

#[test]
fn auth_headers_include_protocol_worker_and_idempotency() {
    let creds = voom_worker_protocol::WorkerCredentials {
        worker_id: voom_core::WorkerId(1),
        worker_epoch: 0,
        secret: secrecy::SecretString::from("secret"),
    };
    let headers = auth_headers(&creds, "abc");
    assert!(headers.iter().any(|(k, _)| *k == "X-Voom-Protocol-Version"));
    assert!(headers.iter().any(|(k, _)| *k == "Authorization"));
    assert!(headers.iter().any(|(k, _)| *k == "X-Voom-Idempotency-Key"));
}

#[test]
fn malformed_json_body_is_not_valid_json() {
    assert!(serde_json::from_slice::<serde_json::Value>(malformed_json_body()).is_err());
}

#[test]
fn raw_response_parser_decodes_protocol_error_body() {
    let body =
        serde_json::to_vec(&voom_worker_protocol::ProtocolError::UnauthorizedBearer).unwrap();
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

#[test]
fn wrong_content_length_classifier_rejects_success_response() {
    let raw = b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec();
    let err = classify_wrong_content_length_response(Ok(raw)).unwrap_err();
    assert!(err.contains("accepted"));
}

#[test]
fn wrong_content_length_classifier_accepts_non_success_response() {
    let raw = b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n".to_vec();
    classify_wrong_content_length_response(Ok(raw)).unwrap();
}

#[test]
fn wrong_content_length_classifier_accepts_clean_close() {
    classify_wrong_content_length_response(Ok(Vec::new())).unwrap();
}

#[test]
fn wrong_content_length_classifier_rejects_timeout() {
    let err = classify_wrong_content_length_response(Err(
        "wrong content-length hung waiting for response/close".to_owned(),
    ))
    .unwrap_err();
    assert!(err.contains("hung"));
}
