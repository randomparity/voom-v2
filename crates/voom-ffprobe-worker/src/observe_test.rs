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
