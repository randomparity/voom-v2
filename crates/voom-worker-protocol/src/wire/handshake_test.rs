use super::*;
use crate::ProtocolError;

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
            offered: 2,
            expected: 1,
        }
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
