use std::fmt::{Display, Formatter};
use std::path::Path;

use tokio::io::AsyncReadExt;
use voom_worker_protocol::TranscodeVideoObservedFacts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveError {
    ArtifactUnavailable(String),
    ArtifactChecksumMismatch(String),
}

impl Display for ObserveError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ArtifactUnavailable(message) => write!(f, "artifact unavailable: {message}"),
            Self::ArtifactChecksumMismatch(message) => {
                write!(f, "artifact checksum mismatch: {message}")
            }
        }
    }
}

impl std::error::Error for ObserveError {}

pub async fn observe_file_facts(path: &Path) -> Result<TranscodeVideoObservedFacts, ObserveError> {
    let path_metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|err| ObserveError::ArtifactUnavailable(err.to_string()))?;
    if !path_metadata.is_file() {
        return Err(ObserveError::ArtifactUnavailable(
            "artifact path is not a regular file".to_owned(),
        ));
    }

    let mut file = open_regular_file_no_follow(path).await?;
    let metadata = file
        .metadata()
        .await
        .map_err(|err| ObserveError::ArtifactUnavailable(err.to_string()))?;
    if !metadata.is_file() {
        return Err(ObserveError::ArtifactUnavailable(
            "artifact path is not a regular file".to_owned(),
        ));
    }

    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|err| ObserveError::ArtifactUnavailable(err.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let final_metadata = file
        .metadata()
        .await
        .map_err(|err| ObserveError::ArtifactUnavailable(err.to_string()))?;
    if metadata_changed(&metadata, &final_metadata) {
        return Err(ObserveError::ArtifactChecksumMismatch(
            "artifact changed while worker was reading it".to_owned(),
        ));
    }

    Ok(TranscodeVideoObservedFacts {
        size_bytes: metadata.len(),
        content_hash: format!("blake3:{}", hasher.finalize().to_hex()),
        modified_at: modified_at(&metadata),
        local_file_key: None,
    })
}

#[cfg(unix)]
async fn open_regular_file_no_follow(path: &Path) -> Result<tokio::fs::File, ObserveError> {
    use std::os::unix::fs::OpenOptionsExt;

    let path = path.to_owned();
    let std_file = tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    })
    .await
    .map_err(|err| ObserveError::ArtifactUnavailable(err.to_string()))?
    .map_err(|err| ObserveError::ArtifactUnavailable(err.to_string()))?;

    Ok(tokio::fs::File::from_std(std_file))
}

#[cfg(not(unix))]
async fn open_regular_file_no_follow(path: &Path) -> Result<tokio::fs::File, ObserveError> {
    tokio::fs::File::open(path)
        .await
        .map_err(|err| ObserveError::ArtifactUnavailable(err.to_string()))
}

fn metadata_changed(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() != after.len() || before.modified().ok() != after.modified().ok()
}

fn modified_at(metadata: &std::fs::Metadata) -> Option<String> {
    let modified = metadata.modified().ok()?;
    let datetime = chrono::DateTime::<chrono::Utc>::from(modified);
    Some(datetime.to_rfc3339())
}

#[cfg(test)]
#[path = "observe_test.rs"]
mod tests;
