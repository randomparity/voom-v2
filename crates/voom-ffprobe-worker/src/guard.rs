//! Guarded reuse of the owner's content hash (ADR 0120 contracts 4 and 5).
//!
//! With a guard, the probe reads no file bytes on btrfs and tmpfs: it compares
//! size, mtime ns, ctime ns and the file key against the hash leg's facts
//! before and after ffprobe, then echoes the expected hash. On any other
//! filesystem it returns `None` and the caller recomputes the hash as before.
//! The guard is compared, never stored or treated as a content hash.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use time::OffsetDateTime;
use voom_core::{FileKeyFacts, HashSource, format_iso8601};
use voom_worker_protocol::{
    ObservedFileFacts, ProbeFileRequest, ProbeFileResult, ProbeFileStatus, ProbeGuard,
};

use crate::ffprobe::{FfprobeConfig, FfprobeError, run_ffprobe_json};
use crate::normalize_ffprobe_json;
use crate::observe::{modified_at, open_regular_file};

/// Filesystems whose ctime detected every constructed same-size edit (#798).
fn reuse_listed(filesystem_type: i64) -> bool {
    filesystem_type == libc::BTRFS_SUPER_MAGIC || filesystem_type == libc::TMPFS_MAGIC
}

/// Whether the open file lives on a reuse-listed filesystem. A failed
/// `fstatfs` counts as unlisted, so the probe falls back to re-hashing.
pub(crate) fn on_reuse_listed_filesystem(file: impl std::os::fd::AsFd) -> bool {
    rustix::fs::fstatfs(file).is_ok_and(|stat| reuse_listed(stat.f_type))
}

/// The stat facts the guard compares.
struct GuardFacts {
    size_bytes: u64,
    modified_at: Option<String>,
    changed_at_ns: i128,
    file_key: FileKeyFacts,
}

impl GuardFacts {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            size_bytes: metadata.len(),
            modified_at: modified_at(metadata),
            changed_at_ns: i128::from(metadata.ctime()) * 1_000_000_000
                + i128::from(metadata.ctime_nsec()),
            file_key: FileKeyFacts {
                dev: metadata.dev(),
                ino: metadata.ino(),
                nlink: metadata.nlink(),
            },
        }
    }

    fn matches(&self, request: &ProbeFileRequest, guard: &ProbeGuard) -> bool {
        request.expected.modified_at.is_some()
            && self.size_bytes == request.expected.size_bytes
            && self.modified_at == request.expected.modified_at
            && self.changed_at_ns == guard.changed_at_ns
            && self.file_key == guard.file_key
    }

    fn observed(self, request: &ProbeFileRequest) -> ObservedFileFacts {
        ObservedFileFacts {
            size_bytes: self.size_bytes,
            content_hash: request.expected.content_hash.clone(),
            modified_at: self.modified_at,
            local_file_key: None,
        }
    }
}

/// Probe under the reuse guard, or return `None` when the file is not on a
/// reuse-listed filesystem.
///
/// # Errors
///
/// A guard difference before or after ffprobe is `ArtifactChecksumMismatch`
/// with a stage-only payload; open and ffprobe failures are as for an
/// unguarded probe.
pub(crate) async fn probe_reused(
    request: &ProbeFileRequest,
    guard: &ProbeGuard,
    config: &FfprobeConfig,
) -> Result<Option<ProbeFileResult>, FfprobeError> {
    let path = Path::new(&request.path);
    let (file, metadata) = open_regular_file(path).await?;
    if !on_reuse_listed_filesystem(&file) {
        return Ok(None);
    }
    let pre_probe = GuardFacts::from_metadata(&metadata);
    if !pre_probe.matches(request, guard) {
        return Err(guard_mismatch("pre_probe_guard"));
    }
    drop(file);

    let raw = run_ffprobe_json(path, config).await?;
    let probed_at = format_iso8601(OffsetDateTime::now_utc());
    let snapshot = normalize_ffprobe_json(raw, config.provider_version(), &probed_at)
        .map_err(FfprobeError::from)?;

    // Re-open by path so a file renamed over it during ffprobe fails the
    // file-key comparison instead of passing on the old descriptor.
    let (_file, metadata) = open_regular_file(path).await?;
    let post_probe = GuardFacts::from_metadata(&metadata);
    if !post_probe.matches(request, guard) {
        return Err(guard_mismatch("post_probe_guard"));
    }

    Ok(Some(ProbeFileResult {
        status: ProbeFileStatus::Probed,
        provider: "ffprobe".to_owned(),
        provider_version: config.provider_version().to_owned(),
        pre_probe: pre_probe.observed(request),
        post_probe: post_probe.observed(request),
        snapshot,
        hash_source: Some(HashSource::Reused),
    }))
}

/// Fact-free by contract: no stat value, filesystem type or path.
fn guard_mismatch(stage: &str) -> FfprobeError {
    FfprobeError::ArtifactChecksumMismatch {
        payload: serde_json::json!({ "stage": stage }),
        message: format!("{stage}: file facts differ from the reuse guard; no facts published"),
    }
}

#[cfg(test)]
#[path = "guard_test.rs"]
mod tests;
