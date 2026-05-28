use super::*;

#[tokio::test]
async fn observe_file_facts_returns_regular_file_hash_and_size() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let path = dir.path().join("clip.mp4");

    let write_result = std::fs::write(&path, b"voom");
    assert!(write_result.is_ok());

    let observed_result = Box::pin(observe_file_facts(&path)).await;
    assert!(observed_result.is_ok());
    let Ok(observed) = observed_result else {
        return;
    };

    assert_eq!(observed.size_bytes, 4);
    assert!(observed.content_hash.starts_with("blake3:"));
    assert_eq!(observed.content_hash.len(), "blake3:".len() + 64);
    assert!(observed.modified_at.is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn observe_file_facts_rejects_symlink_to_regular_file() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let target = dir.path().join("real.mp4");
    let write_result = std::fs::write(&target, b"voom");
    assert!(write_result.is_ok());
    let link = dir.path().join("link.mp4");
    let symlink_result = std::os::unix::fs::symlink(&target, &link);
    assert!(symlink_result.is_ok());

    // Other workers refuse to follow symlinks (symlink_metadata + O_NOFOLLOW);
    // ffprobe must match so a symlinked path cannot redirect the probe to a
    // different file than the scanner hashed.
    let result = Box::pin(observe_file_facts(&link)).await;

    assert!(
        matches!(
            result.as_ref().map_err(WorkerError::failure_class),
            Err(voom_core::FailureClass::ArtifactUnavailable)
        ),
        "symlink to a regular file must be rejected, got {result:?}"
    );
}

#[tokio::test]
async fn observe_file_facts_rejects_directory() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };

    let result = Box::pin(observe_file_facts(dir.path())).await;

    assert!(matches!(
        result.as_ref().map_err(WorkerError::failure_class),
        Err(voom_core::FailureClass::ArtifactUnavailable)
    ));
}
