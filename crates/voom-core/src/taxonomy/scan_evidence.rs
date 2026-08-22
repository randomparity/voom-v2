//! Typed evidence reported by owner-node scan/hash workers.
//!
//! An observation carries evidence if and only if current hash and probe
//! results agreed on stable facts (ADR 0077); an evidence-less observation
//! records existence without publishing identity. The payload is durable
//! (`scan_observations.evidence_json`), so it participates in ADR 0013's
//! strict payload contract: `deny_unknown_fields`, additive-only evolution.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::VoomError;

/// Inode facts that let the control plane resolve hardlinks without touching
/// bytes (Unix only; absent on other platforms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileKeyFacts {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
}

impl FileKeyFacts {
    /// Validates checked conversions from untrusted persisted scalars.
    pub fn parse_database(dev: i64, ino: i64, nlink: i64) -> Result<Self, VoomError> {
        let positive = |value: i64, field: &str| {
            u64::try_from(value).map_err(|_| {
                VoomError::database(format!(
                    "file key {field} {value} is not a valid non-negative device/inode scalar"
                ))
            })
        };
        Ok(Self {
            dev: positive(dev, "dev")?,
            ino: positive(ino, "ino")?,
            nlink: positive(nlink, "nlink")?,
        })
    }
}

/// A sidecar hashed on the owner node while its primary was hashed and probed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanSidecarEvidence {
    pub provider_relative_locator: String,
    /// Sidecar role vocabulary: `external_subtitle`, `nfo`, `poster`, `trailer`.
    pub role: String,
    pub blake3_hex: String,
    pub size_bytes: u64,
}

impl ScanSidecarEvidence {
    /// Structural validation applied at every trust boundary (wire, database).
    pub fn validate(&self) -> Result<(), VoomError> {
        validate_locator(&self.provider_relative_locator)?;
        match self.role.as_str() {
            "external_subtitle" | "nfo" | "poster" | "trailer" => {}
            other => {
                return Err(VoomError::Config(format!(
                    "sidecar evidence role {other:?} not in sidecar role vocab"
                )));
            }
        }
        validate_blake3_hex(&self.blake3_hex)?;
        Ok(())
    }
}

fn validate_locator(value: &str) -> Result<(), VoomError> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
        return Err(VoomError::Config(
            "sidecar evidence locator must be 1..=4096 bytes without NUL".to_owned(),
        ));
    }
    Ok(())
}

fn validate_blake3_hex(value: &str) -> Result<(), VoomError> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(VoomError::Config(
            "sidecar evidence digest must be 64 hex characters".to_owned(),
        ));
    }
    Ok(())
}

fn validate_blake3_hash(value: &str) -> Result<(), VoomError> {
    let Some(hex) = value.strip_prefix("blake3:") else {
        return Err(VoomError::Config(
            "evidence content hash must carry the blake3: prefix".to_owned(),
        ));
    };
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(VoomError::Config(
            "blake3 content hash must be blake3:<64 hex>".to_owned(),
        ));
    }
    Ok(())
}

/// Evidence attached to a scan observation when hash and probe agree (ADR 0077).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanObservationEvidence {
    /// `blake3:<64 hex>` as computed by the owner-node hash worker.
    pub content_hash: String,
    pub size_bytes: u64,
    /// RFC 3339 modification time observed by the agreeing hash and probe legs.
    pub modified_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_key: Option<FileKeyFacts>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<ScanSidecarEvidence>,
    /// Normalized ffprobe snapshot, verbatim from the probe worker result.
    pub probe_snapshot: Value,
}

/// Upper bounds mirroring ADR 0077's per-observation evidence budget.
pub const MAX_EVIDENCE_SIDECARS: usize = 64;
/// Serialized-evidence ceiling per observation, in bytes.
pub const MAX_EVIDENCE_BYTES: usize = 64 * 1024;

impl ScanObservationEvidence {
    /// Structural validation applied at every trust boundary (wire, database):
    /// fact shapes, role vocabulary, digest forms, and the per-observation
    /// size/sidecar bounds of ADR 0077. Snapshot content is validated by the
    /// media-snapshot reader at publication time.
    pub fn validate(&self) -> Result<(), VoomError> {
        validate_blake3_hash(&self.content_hash)?;
        if self.sidecars.len() > MAX_EVIDENCE_SIDECARS {
            return Err(VoomError::Config(format!(
                "evidence carries {} sidecars, above the {MAX_EVIDENCE_SIDECARS} bound",
                self.sidecars.len()
            )));
        }
        for sidecar in &self.sidecars {
            sidecar.validate()?;
        }
        let serialized = serde_json::to_vec(self)
            .map_err(|error| VoomError::Config(format!("evidence encode: {error}")))?;
        if serialized.len() > MAX_EVIDENCE_BYTES {
            return Err(VoomError::Config(format!(
                "serialized evidence is {} bytes, above the {MAX_EVIDENCE_BYTES}-byte bound",
                serialized.len()
            )));
        }
        Ok(())
    }

    /// Decodes persisted evidence JSON, rejecting unknown fields and anything
    /// failing structural validation — corrupt storage is a database error.
    pub fn parse_database(value: &str) -> Result<Self, VoomError> {
        let parsed: Self = serde_json::from_str(value)
            .map_err(|error| VoomError::database(format!("scan observation evidence: {error}")))?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Encodes evidence for persistence after structural validation.
    pub fn to_database_json(&self) -> Result<String, VoomError> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|error| VoomError::database(format!("scan observation evidence: {error}")))
    }
}
