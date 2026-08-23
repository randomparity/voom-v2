use super::*;
use crate::ProtocolError;

#[test]
fn protocol_version_is_three() {
    assert_eq!(voom_core::PROTOCOL_VERSION, 3);
}

#[test]
fn negotiate_exact_match_returns_agreed() {
    let resp = negotiate(voom_core::PROTOCOL_VERSION).unwrap();
    assert_eq!(resp.agreed, voom_core::PROTOCOL_VERSION);
}

#[test]
fn negotiate_other_version_rejects() {
    let err = negotiate(voom_core::PROTOCOL_VERSION + 1).unwrap_err();
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
fn handshake_request_round_trips() {
    let req = HandshakeRequest { offered: 1 };
    let json = serde_json::to_string(&req).unwrap();
    let back: HandshakeRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
}

#[test]
fn handshake_response_round_trips() {
    let resp = HandshakeResponse { agreed: 1 };
    let json = serde_json::to_string(&resp).unwrap();
    let back: HandshakeResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(resp, back);
}

#[test]
fn handshake_request_rejects_unknown_field() {
    let raw = r#"{"offered": 1, "extra": true}"#;
    let res: Result<HandshakeRequest, _> = serde_json::from_str(raw);
    assert!(res.is_err());
}
