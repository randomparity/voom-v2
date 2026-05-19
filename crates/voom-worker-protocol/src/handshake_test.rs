use super::*;
use crate::envelope::ProtocolError;

#[test]
fn negotiate_supported_returns_agreed() {
    let resp = negotiate(1).unwrap();
    assert_eq!(resp.agreed, 1);
}

#[test]
fn negotiate_below_min_rejects() {
    let err = negotiate(0).unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::UnsupportedProtocolVersion {
            offered: 0,
            supported_min: 1,
            supported_max: 1,
        }
    ));
}

#[test]
fn negotiate_above_max_rejects() {
    let err = negotiate(2).unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::UnsupportedProtocolVersion {
            offered: 2,
            supported_min: 1,
            supported_max: 1,
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
