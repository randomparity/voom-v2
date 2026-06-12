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
/// The contract is an **exact match** (ADR-0016): workers are bundled and
/// version-locked with the control-plane build, so the only acceptable
/// offer is `voom_core::PROTOCOL_VERSION`. Returns
/// `Ok(HandshakeResponse { agreed })` (where `agreed == offered`) on a
/// match, or `Err(ProtocolError::UnsupportedProtocolVersion)` carrying the
/// single `expected` version on any mismatch. This is the sole definition
/// of the version check; the operations-path middleware delegates to it.
pub fn negotiate(offered: u32) -> Result<HandshakeResponse, ProtocolError> {
    if offered != voom_core::PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedProtocolVersion {
            offered,
            expected: voom_core::PROTOCOL_VERSION,
        });
    }
    Ok(HandshakeResponse { agreed: offered })
}

#[cfg(test)]
#[path = "handshake_test.rs"]
mod tests;
