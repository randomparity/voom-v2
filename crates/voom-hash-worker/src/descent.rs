//! Component-wise `O_NOFOLLOW` resolution from a canonical storage root to a
//! root-relative `/`-joined locator.
//!
//! Every intermediate directory component is opened with
//! `O_NOFOLLOW | O_DIRECTORY` and the final component with plain `O_NOFOLLOW`,
//! so a symlink anywhere along the locator fails the descent instead of
//! redirecting the read to a different file (ADR 0077 / spec C3). Locators
//! that could escape the root — absolute paths, empty or `.` or `..`
//! components, NUL bytes — are rejected structurally, before any syscall,
//! through [`voom_core::ProviderRelativeLocator`].

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use voom_core::ProviderRelativeLocator;

/// A file opened without following symlinks, plus its path within the root.
///
/// The open went through the per-component descent; `path` is
/// `root.join(locator)` and exists for diagnostics only.
#[derive(Debug)]
pub struct ResolvedFile {
    pub file: std::fs::File,
    pub path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum DescentError {
    /// The locator fails structural escape-checking (empty, `..`, absolute,
    /// NUL). Rejected before any syscall.
    #[error("invalid provider-relative locator: {0}")]
    InvalidLocator(String),
    /// The locator names no existing object: absence is real, not an error
    /// to retry around.
    #[error("candidate not found in storage root: {0}")]
    NotFound(String),
    /// A component was a symlink or the path was otherwise unopenable.
    #[error("candidate path rejected during descent: {0}")]
    Rejected(String),
}

/// Resolve `locator` beneath `canonical_root` without ever following a
/// symlink component.
///
/// # Errors
///
/// Returns [`DescentError::InvalidLocator`] for structurally invalid
/// locators (checked before any syscall), [`DescentError::NotFound`] when the
/// final component does not exist, and [`DescentError::Rejected`] when a
/// component is a symlink or cannot be opened.
pub fn resolve_in_root(root: &Path, locator: &str) -> Result<ResolvedFile, DescentError> {
    // Structural rejection first: empty locators, `..` escapes, absolute
    // paths, and NUL bytes must never reach the filesystem.
    let relative = ProviderRelativeLocator::new(locator.to_owned())
        .map_err(|err| DescentError::InvalidLocator(err.to_string()))?;
    descend(root, relative.as_str())
}

#[cfg(unix)]
fn descend(root: &Path, locator: &str) -> Result<ResolvedFile, DescentError> {
    use std::os::unix::fs::OpenOptionsExt;

    let components: Vec<&str> = locator.split('/').collect();
    let last = components.len() - 1;
    for depth in 0..last {
        let prefix = components[..=depth].join("/");
        let prefix_path = root.join(&prefix);
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(&prefix_path)
            .map_err(|err| classify_open_error(&prefix_path.display().to_string(), &err))?;
    }

    let full = root.join(locator);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&full)
        .map_err(|err| classify_open_error(&full.display().to_string(), &err))?;
    Ok(ResolvedFile { file, path: full })
}

#[cfg(unix)]
fn classify_open_error(path: &str, err: &std::io::Error) -> DescentError {
    if err.kind() == std::io::ErrorKind::NotFound {
        return DescentError::NotFound(path.to_owned());
    }
    // Under O_NOFOLLOW a symlink component surfaces as ELOOP; under
    // O_DIRECTORY it may surface as ENOTDIR instead. Confirm against
    // symlink_metadata so both shapes report a symlink refusal.
    let is_symlink =
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink());
    if err.raw_os_error() == Some(libc::ELOOP) || is_symlink {
        return DescentError::Rejected(format!("symlink component refused at {path}: {err}"));
    }
    DescentError::Rejected(format!("cannot open {path}: {err}"))
}

#[cfg(not(unix))]
fn descend(_root: &Path, _locator: &str) -> Result<ResolvedFile, DescentError> {
    Err(DescentError::Rejected(
        "component-wise O_NOFOLLOW descent requires a unix platform".to_owned(),
    ))
}

#[cfg(test)]
#[path = "descent_test.rs"]
mod tests;
