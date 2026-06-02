//! Version-negotiation handshake. Phase 1 design §3.6.

use serde::{Deserialize, Serialize};

use crate::ProtocolError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandshakeRequest {
    pub offered: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandshakeResponse {
    pub agreed: u32,
}

/// Decide whether the offered protocol version is acceptable.
///
/// Returns `Ok(HandshakeResponse { agreed })` when `offered` falls in
/// `[voom_core::PROTOCOL_VERSION_SUPPORTED_MIN, voom_core::PROTOCOL_VERSION_SUPPORTED_MAX]`,
/// or `Err(ProtocolError::UnsupportedProtocolVersion)` with the
/// supported range populated so the caller can negotiate.
pub fn negotiate(offered: u32) -> Result<HandshakeResponse, ProtocolError> {
    let min = voom_core::PROTOCOL_VERSION_SUPPORTED_MIN;
    let max = voom_core::PROTOCOL_VERSION_SUPPORTED_MAX;
    if offered < min || offered > max {
        return Err(ProtocolError::UnsupportedProtocolVersion {
            offered,
            supported_min: min,
            supported_max: max,
        });
    }
    Ok(HandshakeResponse { agreed: offered })
}

#[cfg(test)]
#[path = "handshake_test.rs"]
mod tests;
