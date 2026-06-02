use super::*;

#[tokio::test]
async fn missing_file_is_artifact_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("missing.mkv");

    let err = observe_file_facts(&path).await.unwrap_err();
    let message = err.to_string();

    assert!(matches!(err, ObserveError::ArtifactUnavailable(_)));
    assert!(message.contains(&path.display().to_string()), "{message}");
}

#[tokio::test]
async fn directory_error_includes_path_context() {
    let temp = tempfile::tempdir().unwrap();

    let err = observe_file_facts(temp.path()).await.unwrap_err();
    let message = err.to_string();

    assert!(matches!(err, ObserveError::ArtifactUnavailable(_)));
    assert!(
        message.contains(&temp.path().display().to_string()),
        "{message}"
    );
    assert!(message.contains("not a regular file"), "{message}");
}
