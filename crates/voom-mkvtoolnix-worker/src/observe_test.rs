use super::*;

#[tokio::test]
async fn missing_file_is_artifact_unavailable() {
    let temp = tempfile::tempdir().unwrap();

    let err = observe_file_facts(&temp.path().join("missing.mkv"))
        .await
        .unwrap_err();

    assert!(matches!(err, ObserveError::ArtifactUnavailable(_)));
}
