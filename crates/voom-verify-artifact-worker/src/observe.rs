use std::fmt::{Display, Formatter};
use std::path::Path;

use time::OffsetDateTime;
use tokio::io::AsyncReadExt;
use voom_core::format_iso8601;
use voom_worker_protocol::VerifyArtifactObservedFacts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveError {
    ArtifactUnavailable(String),
    ArtifactChecksumMismatch(String),
}

impl Display for ObserveError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ArtifactUnavailable(message) => {
                write!(f, "artifact unavailable: {message}")
            }
            Self::ArtifactChecksumMismatch(message) => {
                write!(f, "artifact checksum mismatch: {message}")
            }
        }
    }
}

impl std::error::Error for ObserveError {}

pub async fn observe_file_facts(path: &Path) -> Result<VerifyArtifactObservedFacts, ObserveError> {
    let path_metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|err| artifact_unavailable(path, err))?;
    if !path_metadata.is_file() {
        return Err(artifact_not_regular(path));
    }

    let mut file = open_regular_file_no_follow(path).await?;
    let metadata = file
        .metadata()
        .await
        .map_err(|err| artifact_unavailable(path, err))?;
    if !metadata.is_file() {
        return Err(artifact_not_regular(path));
    }

    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|err| artifact_unavailable(path, err))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let final_metadata = file
        .metadata()
        .await
        .map_err(|err| artifact_unavailable(path, err))?;
    if metadata_changed(&metadata, &final_metadata) {
        return Err(ObserveError::ArtifactChecksumMismatch(
            "artifact changed while verification was reading it".to_owned(),
        ));
    }

    Ok(VerifyArtifactObservedFacts {
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
    let open_path = path.clone();
    let std_file = tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(open_path)
    })
    .await
    .map_err(|err| artifact_unavailable(&path, err))?
    .map_err(|err| artifact_unavailable(&path, err))?;

    Ok(tokio::fs::File::from_std(std_file))
}

#[cfg(not(unix))]
async fn open_regular_file_no_follow(path: &Path) -> Result<tokio::fs::File, ObserveError> {
    tokio::fs::File::open(path)
        .await
        .map_err(|err| artifact_unavailable(path, err))
}

fn artifact_unavailable(path: &Path, err: impl Display) -> ObserveError {
    ObserveError::ArtifactUnavailable(format!("artifact path {}: {err}", path.display()))
}

fn artifact_not_regular(path: &Path) -> ObserveError {
    ObserveError::ArtifactUnavailable(format!(
        "artifact path {} is not a regular file",
        path.display()
    ))
}

fn metadata_changed(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() != after.len() || before.modified().ok() != after.modified().ok()
}

fn modified_at(metadata: &std::fs::Metadata) -> Option<String> {
    let modified = metadata.modified().ok()?;
    Some(format_iso8601(OffsetDateTime::from(modified)))
}

#[cfg(test)]
#[path = "observe_test.rs"]
mod tests;
