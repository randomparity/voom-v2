use std::path::Path;

use tokio::io::AsyncReadExt;
use voom_worker_protocol::ObservedFileFacts;

use crate::WorkerError;

pub async fn observe_file_facts(path: &Path) -> Result<ObservedFileFacts, WorkerError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|err| WorkerError::ArtifactUnavailable(err.to_string()))?;
    if !metadata.is_file() {
        return Err(WorkerError::ArtifactUnavailable(
            "artifact path is not a regular file".to_owned(),
        ));
    }

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|err| WorkerError::ArtifactUnavailable(err.to_string()))?;
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

fn modified_at(metadata: &std::fs::Metadata) -> Option<String> {
    let modified = metadata.modified().ok()?;
    let datetime = chrono::DateTime::<chrono::Utc>::from(modified);
    Some(datetime.to_rfc3339())
}

#[cfg(test)]
#[path = "observe_test.rs"]
mod tests;
