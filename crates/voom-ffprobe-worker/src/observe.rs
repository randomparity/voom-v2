use std::fmt::Display;
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
    let mut buffer = [0_u8; 16 * 1024];
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
        return Err(WorkerError::ArtifactChecksumMismatch(format!(
            "artifact path {} changed while ffprobe worker was reading it",
            path.display()
        )));
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
async fn open_regular_file_no_follow(path: &Path) -> Result<tokio::fs::File, WorkerError> {
    tokio::fs::File::open(path)
        .await
        .map_err(|err| artifact_unavailable(path, err))
}

fn artifact_unavailable(path: &Path, err: impl Display) -> WorkerError {
    WorkerError::ArtifactUnavailable(format!("artifact path {}: {err}", path.display()))
}

fn artifact_not_regular(path: &Path) -> WorkerError {
    WorkerError::ArtifactUnavailable(format!(
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
