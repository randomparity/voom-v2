use super::*;

#[tokio::test]
async fn hashes_fixture_bytes_with_independently_computed_blake3() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("movie")).unwrap();
    let bytes = b"voom fixture bytes for hashing 0x01 0x02".repeat(1000);
    std::fs::write(dir.path().join("movie/file.mkv"), &bytes).unwrap();

    let result = hash_file_in_root(
        dir.path(),
        &HashFileRequest {
            provider_locator: dir.path().display().to_string(),
            provider_relative_locator: "movie/file.mkv".to_owned(),
        },
    )
    .await
    .unwrap();

    // The published identity must equal an independent BLAKE3 of the bytes:
    // any other value would poison scan-session agreement.
    let expected_hash = blake3::hash(&bytes).to_hex().to_string();
    assert_eq!(result.content_hash, format!("blake3:{expected_hash}"));
    assert_eq!(result.size_bytes, bytes.len() as u64);
    assert!(!result.modified_at.is_empty(), "mtime must be populated");
    assert!(
        !result.stability_started_at.is_empty() && !result.stability_confirmed_at.is_empty(),
        "stability timestamps must be populated"
    );
    // One file per dispatch: the pump owns sidecar correlation.
    assert!(result.sidecars.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn reports_file_key_from_pre_read_stat() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("clip.mkv"), b"keyed").unwrap();

    let result = hash_file_in_root(
        dir.path(),
        &HashFileRequest {
            provider_locator: dir.path().display().to_string(),
            provider_relative_locator: "clip.mkv".to_owned(),
        },
    )
    .await
    .unwrap();

    let key = result.file_key.expect("unix hashing populates file_key");
    assert!(key.dev > 0, "dev must be observed");
    assert!(key.ino > 0, "ino must be observed");
    assert_eq!(key.nlink, 1, "single link count");
}

#[cfg(unix)]
#[tokio::test]
async fn hardlink_pair_reports_same_dev_ino_and_nlink_two() {
    let dir = tempfile::tempdir().unwrap();
    let primary = dir.path().join("a.mkv");
    std::fs::write(&primary, b"shared inode").unwrap();
    std::fs::hard_link(&primary, dir.path().join("b.mkv")).unwrap();

    let request = |locator: &str| HashFileRequest {
        provider_locator: dir.path().display().to_string(),
        provider_relative_locator: locator.to_owned(),
    };
    let first = hash_file_in_root(dir.path(), &request("a.mkv"))
        .await
        .unwrap();
    let second = hash_file_in_root(dir.path(), &request("b.mkv"))
        .await
        .unwrap();

    // (dev, ino) is the physical-object identity: two names for one file
    // must collapse onto one key with the true link count.
    let a = first.file_key.expect("file key populated");
    let b = second.file_key.expect("file key populated");
    assert_eq!(a.dev, b.dev, "hardlinks share a device");
    assert_eq!(a.ino, b.ino, "hardlinks share an inode");
    assert_eq!(a.nlink, 2, "two names for one physical file");
}

#[tokio::test]
async fn missing_file_maps_to_artifact_unavailable_with_not_found_code() {
    let dir = tempfile::tempdir().unwrap();

    let err = hash_file_in_root(
        dir.path(),
        &HashFileRequest {
            provider_locator: dir.path().display().to_string(),
            provider_relative_locator: "ghosts/x.mkv".to_owned(),
        },
    )
    .await
    .unwrap_err();

    // Absence is real: NOT_FOUND lets the pump skip observation instead of
    // retrying or failing the session.
    assert_eq!(err.failure_class(), FailureClass::ArtifactUnavailable);
    assert_eq!(err.error_code(), ErrorCode::NotFound);
}

#[cfg(unix)]
#[tokio::test]
async fn drift_between_stats_is_terminal_checksum_mismatch_without_facts() {
    // Drives the stability-protocol steps manually so the mutation lands
    // deterministically between the two stats (no sleeps).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drift.bin");
    std::fs::write(&path, b"original-bytes").unwrap();
    let resolved = resolve_in_root(dir.path(), "drift.bin").unwrap();

    let mut file = tokio::fs::File::from(resolved.file);
    let pre = stat_facts(&file.metadata().await.unwrap());
    let _hash = read_hash(&mut file).await.unwrap();

    // Same-inode truncate + rewrite with a different length so even coarse
    // mtime granularity detects drift.
    std::fs::write(&path, b"mutated").unwrap();

    let post = stat_facts(&file.metadata().await.unwrap());

    let err = assert_stable(&pre, &post).unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::ArtifactChecksumMismatch);
    assert!(
        err.to_string().contains("hash_drift"),
        "message must name stage hash_drift: {err}"
    );
    // Fact-free payload: facts observed on a mutating file must never reach
    // the record; only the stage marker rides along.
    assert_eq!(
        err.payload(),
        &serde_json::json!({ "stage": "hash_drift" }),
        "payload must carry no facts"
    );
}

#[test]
fn identical_facts_pass_stability_check() {
    let facts = StatFacts {
        size_bytes: 12,
        modified_at: SystemTime::UNIX_EPOCH,
        dev: Some(1),
        ino: Some(2),
        nlink: Some(1),
    };

    assert!(assert_stable(&facts.clone(), &facts.clone()).is_ok());
}
