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

pub async fn observe_candidate_file(
    path: impl AsRef<Path>,
) -> Result<ObservedFileFacts, ScanError> {
    let path = path.as_ref();
    let mut file = File::open(path).await.map_err(|err| {
        ScanError::internal(format!(
            "cannot open candidate file {}: {err}",
            path.display()
        ))
    })?;
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

#[cfg(test)]
#[path = "hash_test.rs"]
mod tests;
