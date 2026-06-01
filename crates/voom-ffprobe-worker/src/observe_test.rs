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
    let message = error_message(&result);

    assert!(
        matches!(
            result.as_ref().map_err(WorkerError::failure_class),
            Err(voom_core::FailureClass::ArtifactUnavailable)
        ),
        "symlink to a regular file must be rejected, got {result:?}"
    );
    assert!(message.contains(&link.display().to_string()), "{message}");
}

#[tokio::test]
async fn observe_file_facts_rejects_directory() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };

    let result = Box::pin(observe_file_facts(dir.path())).await;
    let message = error_message(&result);

    assert!(matches!(
        result.as_ref().map_err(WorkerError::failure_class),
        Err(voom_core::FailureClass::ArtifactUnavailable)
    ));
    assert!(
        message.contains(&dir.path().display().to_string()),
        "{message}"
    );
    assert!(message.contains("not a regular file"), "{message}");
}

#[tokio::test]
async fn observe_file_facts_rejects_missing_path_with_context() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let path = dir.path().join("missing.mp4");

    let result = Box::pin(observe_file_facts(&path)).await;
    let message = error_message(&result);

    assert!(matches!(
        result.as_ref().map_err(WorkerError::failure_class),
        Err(voom_core::FailureClass::ArtifactUnavailable)
    ));
    assert!(message.contains(&path.display().to_string()), "{message}");
}

#[test]
fn metadata_changed_detects_length_drift() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let before_path = dir.path().join("before.mp4");
    let after_path = dir.path().join("after.mp4");
    let before_write = std::fs::write(&before_path, b"a");
    assert!(before_write.is_ok());
    let after_write = std::fs::write(&after_path, b"ab");
    assert!(after_write.is_ok());
    let before = std::fs::metadata(&before_path);
    assert!(before.is_ok());
    let Ok(before) = before else {
        return;
    };
    let after = std::fs::metadata(&after_path);
    assert!(after.is_ok());
    let Ok(after) = after else {
        return;
    };

    assert!(metadata_changed(&before, &after));
    assert!(!metadata_changed(&before, &before));
}

fn error_message<T>(result: &Result<T, WorkerError>) -> String {
    match result {
        Ok(_) => String::new(),
        Err(err) => err.to_string(),
    }
}
