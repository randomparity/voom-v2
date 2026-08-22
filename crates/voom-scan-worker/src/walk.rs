//! Metadata-only recursive walk of one canonical storage root (ADR 0077).
//!
//! The walker never follows symlinks: a symlinked root is rejected up front,
//! symlinks encountered during descent are skipped and counted, and every
//! accepted file is re-canonicalized and required to remain beneath the
//! canonical root. Locators are `/`-joined validated components, so
//! leading-dash filenames pass through untouched and traversal components
//! can never be smuggled into a locator.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use time::OffsetDateTime;
use voom_core::{ProviderRelativeLocator, format_iso8601};

use crate::discover::{best_sidecar_candidate, classify_sidecar, matches_media_extension};
/// Maximum directory depth the walk descends before counting deeper entries
/// as skipped. Bounds recursion on adversarially deep trees.
const MAX_WALK_DEPTH: usize = 64;

/// Outcome of one completed root walk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WalkOutcome {
    /// One entry per discovered primary media file, in deterministic
    /// discovery order (sorted `read_dir` per directory).
    pub candidates: Vec<WalkCandidate>,
    /// Symlinks, non-UTF-8 names, invalid locators, unclassifiable files,
    /// unreadable entries, and depth overflows encountered along the way.
    pub skipped_count: u64,
}

/// One primary media file plus the sidecars grouped onto it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkCandidate {
    pub primary: WalkFile,
    pub sidecars: Vec<WalkFile>,
}

/// Facts recorded about one file at discovery time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkFile {
    pub locator: ProviderRelativeLocator,
    /// `"dev={dev};ino={ino}"` stat identity string (Unix).
    pub provider_object_identity: String,
    pub size_bytes: u64,
    /// RFC 3339 modification timestamp.
    pub modified_at: String,
    /// Sidecar role (`external_subtitle` | `nfo` | `poster` | `trailer`);
    /// [`None`] for primaries.
    pub kind: Option<&'static str>,
}

/// Root-level walk failure. Missing, unreadable, symlinked, and non-directory
/// roots abort the whole scan; everything beneath the root degrades to a
/// counted skip instead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RootUnavailable {
    pub message: String,
}

/// Walk `root` metadata-only, classifying primaries against
/// `extension_allowlist` (empty allowlist = the built-in default set).
///
/// # Errors
///
/// Returns [`RootUnavailable`] when the root cannot be scanned at all: it is
/// missing, unreadable, a symlink, or not a directory.
pub fn scan_root(
    root: &Path,
    extension_allowlist: &[String],
) -> Result<WalkOutcome, RootUnavailable> {
    // Reject a symlinked root before canonicalization: canonicalize would
    // silently resolve the link and re-point the entire scan.
    let root_metadata = fs::symlink_metadata(root).map_err(|err| {
        root_unavailable(format!("cannot stat scan root {}: {err}", root.display()))
    })?;
    if root_metadata.file_type().is_symlink() {
        return Err(root_unavailable(format!(
            "scan root {} is a symlink",
            root.display()
        )));
    }
    if !root_metadata.is_dir() {
        return Err(root_unavailable(format!(
            "scan root {} is not a directory",
            root.display()
        )));
    }
    let canonical_root = fs::canonicalize(root).map_err(|err| {
        root_unavailable(format!(
            "cannot canonicalize scan root {}: {err}",
            root.display()
        ))
    })?;
    let mut walker = Walker {
        canonical_root,
        extension_allowlist,
        outcome: WalkOutcome::default(),
        primary_locators: Vec::new(),
        pending_sidecars: Vec::new(),
    };
    let root_dir = walker.canonical_root.clone();
    walker.walk_directory(&root_dir, Path::new(""), 0);
    walker.finish();
    Ok(walker.outcome)
}

fn root_unavailable(message: String) -> RootUnavailable {
    RootUnavailable { message }
}

struct Walker<'a> {
    canonical_root: PathBuf,
    extension_allowlist: &'a [String],
    outcome: WalkOutcome,
    /// Locator of every accepted primary, parallel to
    /// `outcome.candidates`; drives longest-stem sidecar matching.
    primary_locators: Vec<String>,
    /// Sidecars found before their primary could exist (sorted `read_dir`
    /// order is not stem order); attached in one post-walk pass, mirroring
    /// the control-plane discovery this logic replaces.
    pending_sidecars: Vec<(String, WalkFile)>,
}

impl Walker<'_> {
    fn skip(&mut self) {
        self.outcome.skipped_count += 1;
    }

    fn walk_directory(&mut self, dir: &Path, relative_dir: &Path, depth: usize) {
        if depth >= MAX_WALK_DEPTH {
            self.skip();
            return;
        }
        // An unreadable subdirectory degrades to a counted skip; only the
        // pre-validated root itself can produce a root-level failure.
        let Ok(read) = fs::read_dir(dir) else {
            self.skip();
            return;
        };
        let mut entries = Vec::new();
        for entry in read {
            match entry {
                Ok(entry) => entries.push(entry),
                Err(_) => self.skip(),
            }
        }
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in &entries {
            self.walk_entry(entry, relative_dir, depth);
        }
    }

    fn walk_entry(&mut self, entry: &fs::DirEntry, relative_dir: &Path, depth: usize) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            // Locators are Strings: a non-UTF-8 filename cannot be named.
            self.skip();
            return;
        };
        let Ok(file_type) = entry.file_type() else {
            self.skip();
            return;
        };
        if file_type.is_symlink() {
            // Never followed, whether the target lies inside or outside the
            // root: the walk descends real directories only.
            self.skip();
            return;
        }
        if file_type.is_dir() {
            let child_relative = relative_dir.join(name);
            self.walk_directory(&entry.path(), &child_relative, depth + 1);
            return;
        }
        if !file_type.is_file() {
            // FIFOs, sockets, and devices are not scan candidates.
            self.skip();
            return;
        }
        // Defense-in-depth: re-resolve the file and require it to still sit
        // beneath the canonical root, so a concurrent rename or symlink swap
        // cannot redirect the scan outside the tree being scanned.
        let Ok(canonical_child) = fs::canonicalize(entry.path()) else {
            self.skip();
            return;
        };
        if joined_escapes_root(&self.canonical_root, &canonical_child) {
            self.skip();
            return;
        }
        let Ok(metadata) = entry.metadata() else {
            self.skip();
            return;
        };
        let child_relative = relative_dir.join(name);
        let Some(relative) = child_relative.to_str() else {
            self.skip();
            return;
        };
        let Ok(locator) = build_relative_locator(relative) else {
            self.skip();
            return;
        };
        let Some(modified_at) = modified_at(&metadata) else {
            self.skip();
            return;
        };
        let Some(provider_object_identity) = object_identity(&metadata) else {
            self.skip();
            return;
        };
        let size_bytes = metadata.len();
        if let Some(kind) = classify_sidecar(Path::new(relative)) {
            self.pending_sidecars.push((
                relative.to_owned(),
                WalkFile {
                    locator,
                    provider_object_identity,
                    size_bytes,
                    modified_at,
                    kind: Some(kind.role()),
                },
            ));
        } else if matches_media_extension(Path::new(relative), self.extension_allowlist) {
            self.primary_locators.push(relative.to_owned());
            self.outcome.candidates.push(WalkCandidate {
                primary: WalkFile {
                    locator,
                    provider_object_identity,
                    size_bytes,
                    modified_at,
                    kind: None,
                },
                sidecars: Vec::new(),
            });
        } else {
            // Neither primary media nor a known sidecar role.
            self.skip();
        }
    }

    /// Attach every queued sidecar now that the full primary set is known,
    /// then impose deterministic ordering: candidates and their sidecars are
    /// sorted by locator, as the control-plane discovery sorted them.
    fn finish(&mut self) {
        for (relative, sidecar) in std::mem::take(&mut self.pending_sidecars) {
            match best_sidecar_candidate(&self.primary_locators, &relative) {
                Some(index) => self.outcome.candidates[index].sidecars.push(sidecar),
                // A sidecar with no primary media to attach to is a leftover.
                None => self.skip(),
            }
        }
        self.outcome.candidates.sort_by(|left, right| {
            left.primary
                .locator
                .as_str()
                .cmp(right.primary.locator.as_str())
        });
        for candidate in &mut self.outcome.candidates {
            candidate
                .sidecars
                .sort_by(|left, right| left.locator.as_str().cmp(right.locator.as_str()));
        }
    }
}

/// True when `candidate` (already canonicalized, so free of `.`/`..`) does
/// not sit beneath the canonical root.
#[must_use]
pub fn joined_escapes_root(canonical_root: &Path, candidate: &Path) -> bool {
    !candidate.starts_with(canonical_root)
}

/// Marker for a locator that failed component or shape validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocatorRejected;

/// Build and validate a provider-relative locator from a `/`-joined relative
/// path. Every component is checked against `.` / `..` / empty / NUL as
/// defense-in-depth beneath `ProviderRelativeLocator::new`'s own validation,
/// so a hostile filename can never smuggle traversal into a locator.
///
/// # Errors
///
/// Returns [`LocatorRejected`] when any component is unsafe or the joined
/// string fails `ProviderRelativeLocator::new`.
pub fn build_relative_locator(relative: &str) -> Result<ProviderRelativeLocator, LocatorRejected> {
    if !relative.split('/').all(component_is_safe) {
        return Err(LocatorRejected);
    }
    ProviderRelativeLocator::new(relative.to_owned()).map_err(|_| LocatorRejected)
}

/// Defense-in-depth check for one locator path component: non-empty, NUL-free,
/// and never `.` or `..`.
#[must_use]
pub fn component_is_safe(component: &str) -> bool {
    !component.is_empty() && component != "." && component != ".." && !component.contains('\0')
}

// The shared call site treats identity as fallible, so the Unix
// implementation keeps the `Option` shape even though it always succeeds.
#[cfg(unix)]
#[expect(clippy::unnecessary_wraps)]
fn object_identity(metadata: &fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("dev={};ino={}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn object_identity(_metadata: &fs::Metadata) -> Option<String> {
    // Without Unix stat facts the worker cannot build a truthful identity
    // string; the file degrades to a counted skip rather than fabricating one.
    None
}

fn modified_at(metadata: &fs::Metadata) -> Option<String> {
    let modified = metadata.modified().ok()?;
    let since_epoch = modified.duration_since(UNIX_EPOCH).ok()?;
    let secs = i64::try_from(since_epoch.as_secs()).ok()?;
    let timestamp = OffsetDateTime::from_unix_timestamp(secs).ok()?;
    let stamped = timestamp
        .replace_nanosecond(since_epoch.subsec_nanos())
        .ok()?;
    Some(format_iso8601(stamped))
}

#[cfg(test)]
#[path = "walk_test.rs"]
mod tests;
