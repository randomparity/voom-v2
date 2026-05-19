use super::*;
use chrono::{TimeZone, Utc};
use voom_core::LeaseId;

use crate::envelope::{PercentBps, ProgressFrame};

#[test]
fn frame_to_line_bytes_includes_newline() {
    let frame = ProgressFrame::Progress {
        lease_id: LeaseId(1),
        seq: 0,
        emitted_at: Utc.with_ymd_and_hms(2026, 5, 19, 0, 0, 0).unwrap(),
        percent: Some(PercentBps::ZERO),
        message: None,
        payload: None,
    };
    let bytes = frame_to_line_bytes(&frame);
    assert_eq!(bytes.last(), Some(&b'\n'));
}

#[test]
fn raw_post_request_contains_expected_lines() {
    let body = br#"{"hello":"world"}"#.to_vec();
    let raw = raw_post_request(
        "127.0.0.1:8080",
        "/v1/handshake",
        &body,
        &[("X-Test", "ok")],
    );
    let s = String::from_utf8_lossy(&raw);
    assert!(s.starts_with("POST /v1/handshake HTTP/1.1\r\n"));
    assert!(s.contains("Host: 127.0.0.1:8080\r\n"));
    assert!(s.contains("Content-Length: 17\r\n"));
    assert!(s.contains("X-Test: ok\r\n"));
}

#[test]
fn flip_byte_inverts_bits() {
    let mut buf = vec![0x00u8, 0xffu8, 0x55u8];
    flip_byte(&mut buf, 1);
    assert_eq!(buf, vec![0x00, 0x00, 0x55]);
}

#[test]
fn flip_byte_oob_is_noop() {
    let mut buf = vec![0x42u8];
    flip_byte(&mut buf, 10);
    assert_eq!(buf, vec![0x42]);
}

#[test]
fn truncate_returns_prefix() {
    let bytes = b"hello world";
    let t = truncate(bytes, 5);
    assert_eq!(&t[..], b"hello");
}

#[test]
fn truncate_over_len_returns_full() {
    let bytes = b"abc";
    let t = truncate(bytes, 100);
    assert_eq!(&t[..], b"abc");
}
