use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use tokio::fs;
use voom_core::ErrorCode;

pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "avi", "m2ts", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "ts", "webm",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileScanStatus {
    FailedContentDrift,
    SkippedSymlink,
    SkippedUnsupportedExtension,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredScan {
    pub root: PathBuf,
    pub mode: ScanMode,
    pub candidates: Vec<ScanCandidate>,
    pub skipped: Vec<SkippedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCandidate {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    pub path: PathBuf,
    pub status: FileScanStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    code: ErrorCode,
    message: String,
}

impl ScanError {
    #[must_use]
    pub fn error_code(&self) -> ErrorCode {
        self.code
    }

    pub(crate) fn internal(message: String) -> Self {
        internal(message)
    }
}

impl Display for ScanError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ScanError {}

#[must_use]
pub fn is_supported_media_path(path: &Path) -> bool {
    extension_key(path)
        .as_deref()
        .is_some_and(|ext| SUPPORTED_EXTENSIONS.contains(&ext))
}

pub async fn discover_path(path: impl AsRef<Path>) -> Result<DiscoveredScan, ScanError> {
    let path = path.as_ref();
    let metadata = fs::symlink_metadata(path).await.map_err(|err| {
        bad_args(format!(
            "cannot inspect scan path {}: {err}",
            path.display()
        ))
    })?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(bad_args(format!(
            "scan path must not be a symlink: {}",
            path.display()
        )));
    }
    if file_type.is_file() {
        return discover_file(path).await;
    }
    if file_type.is_dir() {
        return discover_directory(path).await;
    }
    Err(bad_args(format!(
        "scan path must be a file or directory: {}",
        path.display()
    )))
}

fn extension_key(path: &Path) -> Option<String> {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
}

async fn discover_file(path: &Path) -> Result<DiscoveredScan, ScanError> {
    if !is_supported_media_path(path) {
        return Err(bad_args(format!(
            "unsupported media extension: {}",
            path.display()
        )));
    }
    let root = canonicalize(path).await?;
    Ok(DiscoveredScan {
        root: root.clone(),
        mode: ScanMode::File,
        candidates: vec![ScanCandidate { path: root }],
        skipped: Vec::new(),
    })
}

async fn discover_directory(path: &Path) -> Result<DiscoveredScan, ScanError> {
    let root = canonicalize(path).await?;
    let mut pending = vec![root.clone()];
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();

    while let Some(dir) = pending.pop() {
        let mut entries = fs::read_dir(&dir)
            .await
            .map_err(|err| internal(format!("cannot read directory {}: {err}", dir.display())))?;
        while let Some(entry) = entries.next_entry().await.map_err(|err| {
            internal(format!(
                "cannot read directory entry {}: {err}",
                dir.display()
            ))
        })? {
            let entry_path = entry.path();
            let metadata = fs::symlink_metadata(&entry_path).await.map_err(|err| {
                internal(format!(
                    "cannot inspect directory entry {}: {err}",
                    entry_path.display()
                ))
            })?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                skipped.push(SkippedFile {
                    path: entry_path,
                    status: FileScanStatus::SkippedSymlink,
                });
            } else if file_type.is_dir() {
                pending.push(entry_path);
            } else if file_type.is_file() && is_supported_media_path(&entry_path) {
                candidates.push(ScanCandidate {
                    path: canonicalize(&entry_path).await?,
                });
            } else if file_type.is_file() {
                let skipped_path = canonicalize(&entry_path).await.unwrap_or(entry_path);
                skipped.push(SkippedFile {
                    path: skipped_path,
                    status: FileScanStatus::SkippedUnsupportedExtension,
                });
            }
        }
    }

    candidates.sort_by_key(|candidate| normalized_path(&candidate.path));
    skipped.sort_by_key(|skipped| normalized_path(&skipped.path));

    Ok(DiscoveredScan {
        root,
        mode: ScanMode::Directory,
        candidates,
        skipped,
    })
}

async fn canonicalize(path: &Path) -> Result<PathBuf, ScanError> {
    fs::canonicalize(path)
        .await
        .map_err(|err| internal(format!("cannot canonicalize {}: {err}", path.display())))
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn bad_args(message: String) -> ScanError {
    ScanError {
        code: ErrorCode::BadArgs,
        message,
    }
}

fn internal(message: String) -> ScanError {
    ScanError {
        code: ErrorCode::Internal,
        message,
    }
}

#[cfg(test)]
#[path = "discovery_test.rs"]
mod tests;
