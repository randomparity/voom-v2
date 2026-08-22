//! `hash_file` worker contract (ADR 0077).
//!
//! The owner-node hash worker resolves one root-relative locator through a
//! component-wise `O_NOFOLLOW` descent from the canonical root, streams BLAKE3
//! over the bytes, and re-stats after reading. Any fact difference between the
//! pre- and post-read stats fails closed as a terminal error frame — the
//! supervisor classifies it, never this payload.

use serde::{Deserialize, Serialize};

use voom_core::FileKeyFacts;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HashFileRequest {
    /// Canonical filesystem path of the storage root this run addresses.
    pub provider_locator: String,
    /// Root-relative `/`-joined locator of the file to hash.
    pub provider_relative_locator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HashedSidecar {
    pub provider_relative_locator: String,
    /// Sidecar role (`external_subtitle|nfo|poster|trailer`).
    pub role: String,
    pub sha256_hex: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HashFileResult {
    /// `blake3:<64 hex>` over the file bytes.
    pub content_hash: String,
    pub size_bytes: u64,
    /// RFC 3339 modification time from the agreeing pre-read stat.
    pub modified_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_key: Option<FileKeyFacts>,
    /// RFC 3339 timestamp of the stat taken before the first byte was read.
    pub stability_started_at: String,
    /// RFC 3339 timestamp of the stat taken after the last byte was read.
    pub stability_confirmed_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<HashedSidecar>,
}

#[cfg(test)]
#[path = "hash_file_test.rs"]
mod tests;
