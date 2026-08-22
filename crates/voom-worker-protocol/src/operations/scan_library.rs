//! `scan_library` worker contract (ADR 0077).
//!
//! The owner-node scan worker enumerates one storage root metadata-only and
//! streams candidate groups back as progress frames. One progress frame carries
//! at most [`MAX_PROGRESS_CANDIDATES`] candidates and at most
//! [`MAX_PROGRESS_PAYLOAD_BYTES`] of serialized payload — half the protocol's
//! 64 KiB NDJSON frame cap — so a frame can never be rejected by the transport
//! after the worker already built it.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Candidates carried by one progress frame.
pub const MAX_PROGRESS_CANDIDATES: usize = 256;
/// Serialized-payload budget of one progress frame (`{"candidates":[…]}`).
pub const MAX_PROGRESS_PAYLOAD_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanLibraryRequest {
    /// Canonical filesystem path of the storage root this run addresses.
    pub provider_locator: String,
    /// Primary-media extension allowlist; empty means the built-in defaults.
    #[serde(default)]
    pub extension_allowlist: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanCandidate {
    pub primary: ScanCandidateFile,
    #[serde(default)]
    pub sidecars: Vec<ScanCandidateFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanCandidateFile {
    /// Root-relative `/`-joined locator in `ProviderRelativeLocator` shape.
    pub provider_relative_locator: String,
    /// `dev=…;ino=…` stat identity string recorded at discovery time.
    pub provider_object_identity: String,
    pub size_bytes: u64,
    /// RFC 3339 modification time observed at discovery time.
    pub modified_at: String,
    /// Sidecar role (`external_subtitle|nfo|poster|trailer`); `None` for primaries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanLibraryResult {
    pub discovered_count: u64,
    pub skipped_count: u64,
}

/// A candidate progress payload failed its structural decode or bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanProgressDecodeError {
    detail: String,
}

impl ScanProgressDecodeError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ScanProgressDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "scan candidate progress payload: {}",
            self.detail
        )
    }
}

impl std::error::Error for ScanProgressDecodeError {}

/// Strictly decode one candidate progress payload.
///
/// Rejects unknown fields, non-object payloads, and any frame whose bounds
/// exceed [`MAX_PROGRESS_CANDIDATES`] candidates or
/// [`MAX_PROGRESS_PAYLOAD_BYTES`] serialized bytes.
///
/// # Errors
///
/// Returns [`ScanProgressDecodeError`] for any malformed or oversized payload.
pub fn decode_candidate_progress(
    payload: &Value,
) -> Result<Vec<ScanCandidate>, ScanProgressDecodeError> {
    if serde_json::to_vec(payload).is_ok_and(|bytes| bytes.len() > MAX_PROGRESS_PAYLOAD_BYTES) {
        return Err(ScanProgressDecodeError::new(format!(
            "serialized payload exceeds the {MAX_PROGRESS_PAYLOAD_BYTES} byte frame budget"
        )));
    }
    let parsed: ProgressCandidates = serde_json::from_value(payload.clone())
        .map_err(|error| ScanProgressDecodeError::new(format!("decode: {error}")))?;
    Ok(parsed.candidates)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressCandidates {
    candidates: Vec<ScanCandidate>,
}

/// Encode one candidate progress payload under the frame bounds.
///
/// # Errors
///
/// Returns [`ScanProgressDecodeError`] when `candidates` exceeds
/// [`MAX_PROGRESS_CANDIDATES`] — callers split before calling, so overshooting
/// the candidate bound is always a caller defect.
pub fn encode_candidate_progress(
    candidates: &[ScanCandidate],
) -> Result<Value, ScanProgressDecodeError> {
    if candidates.len() > MAX_PROGRESS_CANDIDATES {
        return Err(ScanProgressDecodeError::new(format!(
            "{} candidates exceed the {MAX_PROGRESS_CANDIDATES} candidate frame bound",
            candidates.len()
        )));
    }
    let value = serde_json::json!({ "candidates": candidates });
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| ScanProgressDecodeError::new(format!("encode: {error}")))?;
    if encoded.len() > MAX_PROGRESS_PAYLOAD_BYTES {
        return Err(ScanProgressDecodeError::new(format!(
            "encoded {} bytes exceed the {MAX_PROGRESS_PAYLOAD_BYTES} byte frame budget",
            encoded.len()
        )));
    }
    Ok(value)
}

#[cfg(test)]
#[path = "scan_library_test.rs"]
mod tests;
