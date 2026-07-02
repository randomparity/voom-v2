use super::*;

#[tokio::test]
async fn copies_source_and_reports_size_and_checksum() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mkv");
    let bytes = b"the source bytes";
    tokio::fs::write(&source, bytes).await.unwrap();
    let destination = dir.path().join("backups/42/movie.mkv");

    let outcome = back_up_file(&source, &destination).await.unwrap();

    assert_eq!(outcome.size_bytes, bytes.len() as u64);
    assert_eq!(
        outcome.checksum,
        format!("blake3:{}", blake3::hash(bytes).to_hex())
    );
    assert_eq!(tokio::fs::read(&destination).await.unwrap(), bytes);
}

#[tokio::test]
async fn missing_source_is_artifact_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("missing.mkv");
    let destination = dir.path().join("backups/1/missing.mkv");

    let err = back_up_file(&source, &destination).await.unwrap_err();

    assert!(matches!(err, BackupIoError::ArtifactUnavailable(_)));
}

#[tokio::test]
async fn existing_matching_destination_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mkv");
    let bytes = b"identical bytes";
    tokio::fs::write(&source, bytes).await.unwrap();
    let destination = dir.path().join("backups/7/movie.mkv");

    let first = back_up_file(&source, &destination).await.unwrap();
    let second = back_up_file(&source, &destination).await.unwrap();

    assert_eq!(first, second);
    assert_eq!(tokio::fs::read(&destination).await.unwrap(), bytes);
}

#[tokio::test]
async fn existing_mismatched_destination_is_backup_failure() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mkv");
    tokio::fs::write(&source, b"new bytes").await.unwrap();
    let destination = dir.path().join("backups/9/movie.mkv");
    tokio::fs::create_dir_all(destination.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&destination, b"stale bytes")
        .await
        .unwrap();

    let err = back_up_file(&source, &destination).await.unwrap_err();

    assert!(matches!(err, BackupIoError::BackupFailure(_)));
    // The mismatched destination is left untouched.
    assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"stale bytes");
}

#[tokio::test]
async fn creates_missing_destination_directories() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mkv");
    tokio::fs::write(&source, b"x").await.unwrap();
    let destination = dir.path().join("deeply/nested/backup/movie.mkv");

    back_up_file(&source, &destination).await.unwrap();

    assert!(tokio::fs::try_exists(&destination).await.unwrap());
    assert!(
        !tokio::fs::try_exists(
            dir.path()
                .join("deeply/nested/backup/movie.mkv.voom-backup-partial")
        )
        .await
        .unwrap()
    );
}
