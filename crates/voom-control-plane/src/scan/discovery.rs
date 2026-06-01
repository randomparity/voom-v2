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
    Inaccessible,
    Symlink,
    UnsupportedExtension,
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
    pub sidecars: Vec<SidecarCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarCandidate {
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
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| ext.eq_ignore_ascii_case(supported))
        })
}

#[must_use]
pub fn is_supported_sidecar_path(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("srt"))
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
        candidates: vec![ScanCandidate {
            path: root,
            sidecars: Vec::new(),
        }],
        skipped: Vec::new(),
    })
}

async fn discover_directory(path: &Path) -> Result<DiscoveredScan, ScanError> {
    let root = canonicalize(path).await?;
    let mut pending = vec![root.clone()];
    let mut candidates = Vec::new();
    let mut sidecars = Vec::new();
    let mut skipped = Vec::new();

    while let Some(dir) = pending.pop() {
        let Ok(mut entries) = fs::read_dir(&dir).await else {
            skipped.push(SkippedFile {
                path: dir,
                status: FileScanStatus::Inaccessible,
            });
            continue;
        };
        while let Some(entry) = if let Ok(entry) = entries.next_entry().await {
            entry
        } else {
            skipped.push(SkippedFile {
                path: dir.clone(),
                status: FileScanStatus::Inaccessible,
            });
            None
        } {
            let entry_path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&entry_path).await else {
                skipped.push(SkippedFile {
                    path: entry_path,
                    status: FileScanStatus::Inaccessible,
                });
                continue;
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                skipped.push(SkippedFile {
                    path: entry_path,
                    status: FileScanStatus::Symlink,
                });
            } else if file_type.is_dir() {
                pending.push(entry_path);
            } else if file_type.is_file() && is_supported_media_path(&entry_path) {
                candidates.push(ScanCandidate {
                    path: canonicalize(&entry_path).await?,
                    sidecars: Vec::new(),
                });
            } else if file_type.is_file() && is_supported_sidecar_path(&entry_path) {
                sidecars.push(canonicalize(&entry_path).await?);
            } else if file_type.is_file() {
                let skipped_path = canonicalize(&entry_path).await.unwrap_or(entry_path);
                skipped.push(SkippedFile {
                    path: skipped_path,
                    status: FileScanStatus::UnsupportedExtension,
                });
            }
        }
    }

    candidates.sort_by(|left, right| left.path.cmp(&right.path));
    for sidecar in sidecars {
        if let Some(candidate_index) = best_sidecar_candidate(&candidates, &sidecar) {
            candidates[candidate_index]
                .sidecars
                .push(SidecarCandidate { path: sidecar });
        } else {
            skipped.push(SkippedFile {
                path: sidecar,
                status: FileScanStatus::UnsupportedExtension,
            });
        }
    }
    for candidate in &mut candidates {
        candidate
            .sidecars
            .sort_by(|left, right| left.path.cmp(&right.path));
    }
    skipped.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(DiscoveredScan {
        root,
        mode: ScanMode::Directory,
        candidates,
        skipped,
    })
}

fn best_sidecar_candidate(candidates: &[ScanCandidate], sidecar: &Path) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            sidecar_matches_media(&candidate.path, sidecar)
                .map(|stem_len| (index, stem_len, &candidate.path))
        })
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.2.cmp(left.2)))
        .map(|(index, _, _)| index)
}

fn sidecar_matches_media(media: &Path, sidecar: &Path) -> Option<usize> {
    let media_stem = media.file_stem()?.to_str()?;
    let sidecar_stem = sidecar.file_stem()?.to_str()?;
    if sidecar_stem == media_stem {
        return Some(media_stem.len());
    }
    sidecar_stem
        .strip_prefix(media_stem)
        .filter(|suffix| suffix.starts_with('.'))
        .map(|_| media_stem.len())
}

async fn canonicalize(path: &Path) -> Result<PathBuf, ScanError> {
    fs::canonicalize(path)
        .await
        .map_err(|err| internal(format!("cannot canonicalize {}: {err}", path.display())))
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
