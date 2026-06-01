use std::path::Path;

use time::OffsetDateTime;
use tokio::io::AsyncReadExt;
use voom_core::format_iso8601;
use voom_worker_protocol::ObservedFileFacts;

use crate::WorkerError;

pub async fn observe_file_facts(path: &Path) -> Result<ObservedFileFacts, WorkerError> {
    // Do not follow symlinks: symlink_metadata rejects a symlink up front, and
    // the open uses O_NOFOLLOW so a symlink swapped in after the stat is also
    // refused. Matches the ffmpeg / mkvtoolnix / verify-artifact workers so a
    // symlinked path cannot redirect the probe to a different file.
    let path_metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|err| WorkerError::ArtifactUnavailable(err.to_string()))?;
    if !path_metadata.is_file() {
        return Err(WorkerError::ArtifactUnavailable(
            "artifact path is not a regular file".to_owned(),
        ));
    }

    let mut file = open_regular_file_no_follow(path).await?;
    let metadata = file
        .metadata()
        .await
        .map_err(|err| WorkerError::ArtifactUnavailable(err.to_string()))?;
    if !metadata.is_file() {
        return Err(WorkerError::ArtifactUnavailable(
            "artifact path is not a regular file".to_owned(),
        ));
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|err| WorkerError::ArtifactUnavailable(err.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(ObservedFileFacts {
        size_bytes: metadata.len(),
        content_hash: format!("blake3:{}", hasher.finalize().to_hex()),
        modified_at: modified_at(&metadata),
        local_file_key: None,
    })
}

#[cfg(unix)]
async fn open_regular_file_no_follow(path: &Path) -> Result<tokio::fs::File, WorkerError> {
    use std::os::unix::fs::OpenOptionsExt;

    let path = path.to_owned();
    let std_file = tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    })
    .await
    .map_err(|err| WorkerError::ArtifactUnavailable(err.to_string()))?
    .map_err(|err| WorkerError::ArtifactUnavailable(err.to_string()))?;

    Ok(tokio::fs::File::from_std(std_file))
}

#[cfg(not(unix))]
async fn open_regular_file_no_follow(path: &Path) -> Result<tokio::fs::File, WorkerError> {
    tokio::fs::File::open(path)
        .await
        .map_err(|err| WorkerError::ArtifactUnavailable(err.to_string()))
}

fn modified_at(metadata: &std::fs::Metadata) -> Option<String> {
    let modified = metadata.modified().ok()?;
    Some(format_iso8601(OffsetDateTime::from(modified)))
}

#[cfg(test)]
#[path = "observe_test.rs"]
mod tests;
