//! Raw HTTP / NDJSON primitives that bypass the typed encoder.
//!
//! Conformance and chaos workers use this module to construct
//! deliberately malformed wire bytes so a bug in the typed encoder
//! cannot mask the fault.

use bytes::Bytes;

use crate::ProgressFrame;

/// Serialize a frame to its canonical NDJSON line bytes (JSON object +
/// `\n` terminator). This is the byte sequence the wire carries.
#[must_use]
pub fn frame_to_line_bytes(frame: &ProgressFrame) -> Bytes {
    let mut v = serde_json::to_vec(frame).unwrap_or_default();
    v.push(b'\n');
    Bytes::from(v)
}

/// Build a `Content-Length`-correct raw HTTP/1.1 POST request body in
/// memory. Used by the raw-wire conformance suite to construct
/// requests below the typed `hyper` layer (so a bug in the typed
/// encoder cannot fake correctness).
#[must_use]
pub fn raw_post_request(host: &str, path: &str, body: &[u8], headers: &[(&str, &str)]) -> Bytes {
    let mut out = Vec::with_capacity(256 + body.len());
    out.extend_from_slice(format!("POST {path} HTTP/1.1\r\n").as_bytes());
    out.extend_from_slice(format!("Host: {host}\r\n").as_bytes());
    out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    for (k, v) in headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    Bytes::from(out)
}

/// Flip one byte at `index` (XOR 0xff). Caller-supplied mutation for
/// the conformance suite's `flip_one_byte` test.
pub fn flip_byte(bytes: &mut [u8], index: usize) {
    if let Some(b) = bytes.get_mut(index) {
        *b ^= 0xff;
    }
}

/// Truncate `bytes` to `len`, returning the truncated copy. Used by
/// the conformance suite's `truncate_at_byte` test.
#[must_use]
pub fn truncate(bytes: &[u8], len: usize) -> Bytes {
    Bytes::copy_from_slice(&bytes[..len.min(bytes.len())])
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
