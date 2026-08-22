//! Single-file BLAKE3 hashing over a symlink-free descent from the storage
//! root.
//!
//! One dispatch hashes exactly one file: primaries and sidecars arrive as
//! separate `HashFile` dispatches, and the agent pump correlates sidecar
//! roles itself (`HashFileRequest` carries no sidecar list).
//!
//! Stability protocol: record `stability_started_at`, stat the opened file,
//! hash the bytes in fixed-size chunks, re-stat, then record
//! `stability_confirmed_at`. Any fact difference between the two stats is
//! terminal drift reported with the stage marker `hash_drift` and **no**
//! observed facts, so a file mutated mid-hash can never publish stale
//! identity evidence (the pump discriminates drift by class + code).

use std::path::Path;
use std::time::SystemTime;

use time::OffsetDateTime;
use tokio::io::AsyncReadExt;
use voom_core::{ErrorCode, FailureClass, FileKeyFacts, format_iso8601};
use voom_worker_protocol::{HashFileRequest, HashFileResult};

use crate::descent::{DescentError, resolve_in_root};

/// Streaming chunk size for BLAKE3 over the candidate bytes.
pub const HASH_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HashWorkerError {
    /// The artifact does not exist: absence is real. Surfaces as failure
    /// class [`FailureClass::ArtifactUnavailable`] with code
    /// [`ErrorCode::NotFound`] so the pump records absence without an
    /// observation instead of retrying forever.
    #[error("artifact unavailable: {message}")]
    ArtifactNotFound {
        message: String,
        payload: serde_json::Value,
    },
    /// The artifact exists but cannot be read or its locator was refused
    /// (symlink descent rejection, permission denied). Class
    /// [`FailureClass::ArtifactUnavailable`] with a non-`NOT_FOUND` code so
    /// it never masquerades as real absence.
    #[error("artifact unavailable: {message}")]
    ArtifactUnreadable {
        message: String,
        payload: serde_json::Value,
    },
    /// The file changed between the pre-hash and post-hash stats
    /// (`hash_drift`); carries no facts by design.
    #[error("artifact checksum mismatch: {message}")]
    ArtifactChecksumMismatch {
        message: String,
        payload: serde_json::Value,
    },
    /// The worker's own output would violate the result contract.
    #[error("malformed worker result: {message}")]
    MalformedWorkerResult {
        message: String,
        payload: serde_json::Value,
    },
}

impl HashWorkerError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        match self {
            Self::ArtifactNotFound { .. } | Self::ArtifactUnreadable { .. } => {
                FailureClass::ArtifactUnavailable
            }
            Self::ArtifactChecksumMismatch { .. } => FailureClass::ArtifactChecksumMismatch,
            Self::MalformedWorkerResult { .. } => FailureClass::MalformedWorkerResult,
        }
    }

    #[must_use]
    pub const fn error_code(&self) -> ErrorCode {
        match self {
            Self::ArtifactNotFound { .. } => ErrorCode::NotFound,
            // "Unreadable ⇒ ArtifactUnavailable with any other code" per the
            // spec's outcome classification; INTERNAL is the honest bucket
            // for symlink refusals and permission failures.
            Self::ArtifactUnreadable { .. }
            | Self::ArtifactChecksumMismatch { .. }
            | Self::MalformedWorkerResult { .. } => self.failure_class().into_error_code(),
        }
    }

    #[must_use]
    pub const fn payload(&self) -> &serde_json::Value {
        match self {
            Self::ArtifactNotFound { payload, .. }
            | Self::ArtifactUnreadable { payload, .. }
            | Self::ArtifactChecksumMismatch { payload, .. }
            | Self::MalformedWorkerResult { payload, .. } => payload,
        }
    }
}

/// Terminal worker-result error for request payloads that decode but violate
/// the contract (and for undecodable payloads at the handler boundary).
pub(crate) fn malformed_worker_result(stage: &str, message: String) -> HashWorkerError {
    HashWorkerError::MalformedWorkerResult {
        payload: serde_json::json!({ "stage": stage }),
        message,
    }
}

impl From<DescentError> for HashWorkerError {
    fn from(value: DescentError) -> Self {
        match value {
            DescentError::InvalidLocator(reason) => Self::ArtifactUnreadable {
                message: format!("locator rejected before descent: {reason}"),
                payload: serde_json::json!({ "stage": "descent", "reason": reason }),
            },
            DescentError::NotFound(path) => Self::ArtifactNotFound {
                message: format!("candidate not found in storage root: {path}"),
                payload: serde_json::json!({ "stage": "descent" }),
            },
            DescentError::Rejected(reason) => Self::ArtifactUnreadable {
                payload: serde_json::json!({ "stage": "descent", "reason": reason }),
                message: reason,
            },
        }
    }
}

/// Snapshot of the stat fields that must agree before and after hashing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatFacts {
    pub size_bytes: u64,
    pub modified_at: SystemTime,
    pub dev: Option<u64>,
    pub ino: Option<u64>,
    pub nlink: Option<u64>,
}

#[cfg(unix)]
fn inode_facts(metadata: &std::fs::Metadata) -> (Option<u64>, Option<u64>, Option<u64>) {
    use std::os::unix::fs::MetadataExt;
    (
        Some(metadata.dev()),
        Some(metadata.ino()),
        Some(metadata.nlink()),
    )
}

#[cfg(not(unix))]
fn inode_facts(_metadata: &std::fs::Metadata) -> (Option<u64>, Option<u64>, Option<u64>) {
    (None, None, None)
}

#[must_use]
pub fn stat_facts(metadata: &std::fs::Metadata) -> StatFacts {
    let (dev, ino, nlink) = inode_facts(metadata);
    StatFacts {
        size_bytes: metadata.len(),
        modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        dev,
        ino,
        nlink,
    }
}

/// Reject any difference between the pre-hash and post-hash stats as
/// terminal `hash_drift`.
///
/// # Errors
///
/// Returns [`HashWorkerError::ArtifactChecksumMismatch`] naming stage
/// `hash_drift`; the payload is deliberately fact-free so facts observed on
/// a mutating file can never reach the record.
pub fn assert_stable(pre: &StatFacts, post: &StatFacts) -> Result<(), HashWorkerError> {
    if pre == post {
        return Ok(());
    }
    Err(HashWorkerError::ArtifactChecksumMismatch {
        message:
            "hash_drift: file facts changed between the pre-hash and post-hash stats; no facts \
             published"
                .to_owned(),
        payload: serde_json::json!({ "stage": "hash_drift" }),
    })
}

/// Stream the open file through BLAKE3 in [`HASH_CHUNK_BYTES`] chunks.
///
/// # Errors
///
/// Returns [`HashWorkerError::ArtifactUnreadable`] when a read fails
/// mid-stream.
pub async fn read_hash(file: &mut tokio::fs::File) -> Result<String, HashWorkerError> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; HASH_CHUNK_BYTES];
    loop {
        let count =
            file.read(&mut buffer)
                .await
                .map_err(|err| HashWorkerError::ArtifactUnreadable {
                    message: format!("cannot read candidate file while hashing: {err}"),
                    payload: serde_json::json!({ "stage": "read" }),
                })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Physical-object identity captured from the pre-read stat.
#[must_use]
pub fn file_key(facts: &StatFacts) -> Option<FileKeyFacts> {
    Some(FileKeyFacts {
        dev: facts.dev?,
        ino: facts.ino?,
        nlink: facts.nlink?,
    })
}

fn stat_failed(stage: &str, err: &std::io::Error) -> HashWorkerError {
    HashWorkerError::ArtifactUnreadable {
        message: format!("cannot stat candidate file during {stage} stat: {err}"),
        payload: serde_json::json!({ "stage": stage }),
    }
}

/// Hash one file under `canonical_root` per the stability protocol.
///
/// # Errors
///
/// Descent, stat, read, and drift failures surface as
/// [`HashWorkerError`]; drift is [`FailureClass::ArtifactChecksumMismatch`]
/// with the fact-free `hash_drift` payload.
pub async fn hash_file_in_root(
    canonical_root: &Path,
    request: &HashFileRequest,
) -> Result<HashFileResult, HashWorkerError> {
    let stability_started_at = OffsetDateTime::now_utc();
    let resolved = resolve_in_root(canonical_root, &request.provider_relative_locator)?;
    let mut file = tokio::fs::File::from(resolved.file);

    let pre_metadata = file
        .metadata()
        .await
        .map_err(|err| stat_failed("pre-hash", &err))?;
    let pre = stat_facts(&pre_metadata);

    let content_hash = read_hash(&mut file).await?;

    let post_metadata = file
        .metadata()
        .await
        .map_err(|err| stat_failed("post-hash", &err))?;
    let stability_confirmed_at = OffsetDateTime::now_utc();
    assert_stable(&pre, &stat_facts(&post_metadata))?;

    Ok(HashFileResult {
        content_hash: format!("blake3:{content_hash}"),
        size_bytes: pre.size_bytes,
        modified_at: format_iso8601(OffsetDateTime::from(pre.modified_at)),
        file_key: file_key(&pre),
        stability_started_at: format_iso8601(stability_started_at),
        stability_confirmed_at: format_iso8601(stability_confirmed_at),
        // Sidecars are separate HashFile dispatches; the pump fills this in.
        sidecars: Vec::new(),
    })
}

#[cfg(test)]
#[path = "hash_test.rs"]
mod tests;
