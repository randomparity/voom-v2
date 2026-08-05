use std::path::Path;
use std::time::SystemTime;

use tokio::fs::File;
use tokio::io::AsyncReadExt;

use super::discovery::ScanError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedFileFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<SystemTime>,
    /// Physical-object identity captured from the same stat: `(dev, ino)`
    /// identifies the underlying file so two hardlinked paths resolve to one
    /// physical file (#249). `nlink` is the link count at scan time. `None`
    /// off Unix or when the platform does not expose them; the hardlink
    /// resolution simply does not apply then.
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

#[cfg(test)]
pub async fn observe_candidate_file(
    path: impl AsRef<Path>,
) -> Result<ObservedFileFacts, ScanError> {
    let path = path.as_ref();
    let mut file = open_regular_file_no_follow(path).await?;
    observe_open_file(path, &mut file).await
}

pub async fn observe_candidate_file_in_root(
    canonical_root: &Path,
    path: &Path,
) -> Result<ObservedFileFacts, ScanError> {
    ensure_candidate_path_in_root(canonical_root, path).await?;
    let mut file = open_regular_file_no_follow(path).await?;
    observe_open_file(path, &mut file).await
}

pub async fn read_candidate_bytes_in_root(
    canonical_root: &Path,
    path: &Path,
) -> Result<Vec<u8>, ScanError> {
    ensure_candidate_path_in_root(canonical_root, path).await?;
    let mut file = open_regular_file_no_follow(path).await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await.map_err(|err| {
        ScanError::internal(format!(
            "cannot read candidate file {}: {err}",
            path.display()
        ))
    })?;
    Ok(bytes)
}

pub async fn ensure_candidate_path_in_root(
    canonical_root: &Path,
    path: &Path,
) -> Result<(), ScanError> {
    let path_metadata = tokio::fs::symlink_metadata(path).await.map_err(|err| {
        ScanError::internal(format!(
            "cannot inspect candidate file {}: {err}",
            path.display()
        ))
    })?;
    if path_metadata.file_type().is_symlink() {
        return Err(ScanError::internal(format!(
            "candidate file is a symlink: {}",
            path.display()
        )));
    }
    if !path_metadata.is_file() {
        return Err(ScanError::internal(format!(
            "candidate path is not a regular file: {}",
            path.display()
        )));
    }
    let resolved = tokio::fs::canonicalize(path).await.map_err(|err| {
        ScanError::internal(format!(
            "cannot canonicalize candidate file {}: {err}",
            path.display()
        ))
    })?;
    if !resolved.starts_with(canonical_root) {
        return Err(ScanError::internal(format!(
            "candidate file escaped storage root {}: {}",
            canonical_root.display(),
            resolved.display()
        )));
    }
    Ok(())
}

async fn observe_open_file(path: &Path, file: &mut File) -> Result<ObservedFileFacts, ScanError> {
    let metadata = file.metadata().await.map_err(|err| {
        ScanError::internal(format!(
            "cannot inspect candidate file {}: {err}",
            path.display()
        ))
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 8192];

    loop {
        let count = file.read(&mut buffer).await.map_err(|err| {
            ScanError::internal(format!(
                "cannot read candidate file {}: {err}",
                path.display()
            ))
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    let (dev, ino, nlink) = inode_facts(&metadata);
    Ok(ObservedFileFacts {
        size_bytes: metadata.len(),
        content_hash: format!("blake3:{}", hasher.finalize().to_hex()),
        modified_at: metadata.modified().ok(),
        dev,
        ino,
        nlink,
    })
}

#[cfg(unix)]
async fn open_regular_file_no_follow(path: &Path) -> Result<File, ScanError> {
    tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await
        .map_err(|err| {
            ScanError::internal(format!(
                "cannot open candidate file without following symlinks {}: {err}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
async fn open_regular_file_no_follow(path: &Path) -> Result<File, ScanError> {
    File::open(path).await.map_err(|err| {
        ScanError::internal(format!(
            "cannot open candidate file {}: {err}",
            path.display()
        ))
    })
}

#[cfg(test)]
#[path = "hash_test.rs"]
mod tests;
