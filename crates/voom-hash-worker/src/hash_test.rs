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
        changed_at_ns: Some(1_000),
    };

    assert!(assert_stable(&facts.clone(), &facts.clone()).is_ok());
}

#[test]
fn ctime_only_change_is_hash_drift_without_facts() {
    // A same-size interior edit with mtime restored leaves every other fact
    // equal; only ctime moves, and that alone must fail stability.
    let pre = StatFacts {
        size_bytes: 12,
        modified_at: SystemTime::UNIX_EPOCH,
        dev: Some(1),
        ino: Some(2),
        nlink: Some(1),
        changed_at_ns: Some(1_000),
    };
    let post = StatFacts {
        changed_at_ns: Some(1_001),
        ..pre.clone()
    };

    let err = assert_stable(&pre, &post).unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::ArtifactChecksumMismatch);
    assert!(
        err.to_string().contains("hash_drift"),
        "message must name stage hash_drift: {err}"
    );
    assert_eq!(
        err.payload(),
        &serde_json::json!({ "stage": "hash_drift" }),
        "payload must carry no facts"
    );
}

#[cfg(unix)]
#[test]
fn stat_facts_capture_ctime_in_nanoseconds() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctime.bin");
    std::fs::write(&path, b"ctime").unwrap();
    let metadata = std::fs::metadata(&path).unwrap();

    let expected = i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec());
    assert_eq!(stat_facts(&metadata).changed_at_ns, Some(expected));
}

#[tokio::test]
async fn file_spanning_several_chunks_hashes_across_chunk_boundaries() {
    // Two full chunks plus a partial third, with a per-chunk-varying pattern so a
    // dropped, repeated, or truncated chunk changes the digest.
    let dir = tempfile::tempdir().unwrap();
    let len = HASH_CHUNK_BYTES * 2 + 17;
    let bytes: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
    std::fs::write(dir.path().join("big.bin"), &bytes).unwrap();
    let resolved = resolve_in_root(dir.path(), "big.bin").unwrap();
    let mut file = tokio::fs::File::from(resolved.file);

    let digest = read_hash(&mut file).await.unwrap();

    assert_eq!(digest, blake3::hash(&bytes).to_hex().to_string());
}

#[test]
fn hash_chunk_is_one_mebibyte() {
    // ADR 0120 contract 1: the owner hash reads 1 MiB per read (#789's buf1m).
    assert_eq!(HASH_CHUNK_BYTES, 1024 * 1024);
}

#[cfg(unix)]
#[tokio::test]
async fn hash_result_reports_pre_read_ctime() {
    // ADR 0120 contract 2: the pump copies this value into the probe's reuse
    // guard, so it must be the agreeing pre-read stat's ctime, not absent.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctime.mkv");
    std::fs::write(&path, b"ctime reported").unwrap();
    let expected = stat_facts(&std::fs::metadata(&path).unwrap()).changed_at_ns;

    let result = hash_file_in_root(
        dir.path(),
        &HashFileRequest {
            provider_locator: dir.path().display().to_string(),
            provider_relative_locator: "ctime.mkv".to_owned(),
        },
    )
    .await
    .unwrap();

    assert!(expected.is_some());
    assert_eq!(result.changed_at_ns, expected);
}

/// Rewrite one interior byte in place (same size), then put mtime back, as
/// gate G4's constructions do. The 20 ms wait keeps the edit out of the
/// pre-stat's ctime tick on coarse-clock kernels, as the #798 oracle did.
#[cfg(unix)]
fn same_size_edit_with_mtime_restored(path: &std::path::Path) {
    use std::io::{Seek, SeekFrom, Write};

    let original_mtime = std::fs::metadata(path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(3)).unwrap();
    file.write_all(b"X").unwrap();
    file.set_modified(original_mtime).unwrap();
}

/// G4(b): an edit during the hash read whose mtime is restored before the
/// post-read stat still fails the stability check, because ctime moved.
#[cfg(unix)]
fn assert_restored_mtime_edit_during_hash_is_drift(dir: &std::path::Path) {
    let path = dir.join("edited.mkv");
    std::fs::write(&path, b"interior bytes under edit").unwrap();
    let opened = std::fs::File::open(&path).unwrap();
    let pre = stat_facts(&opened.metadata().unwrap());

    same_size_edit_with_mtime_restored(&path);
    let post = stat_facts(&opened.metadata().unwrap());

    assert_eq!(post.size_bytes, pre.size_bytes);
    assert_eq!(post.modified_at, pre.modified_at, "mtime was restored");
    let err = assert_stable(&pre, &post).unwrap_err();
    assert_eq!(err.failure_class(), FailureClass::ArtifactChecksumMismatch);
    assert_eq!(err.payload(), &serde_json::json!({ "stage": "hash_drift" }));
}

#[cfg(target_os = "linux")]
#[test]
fn restored_mtime_edit_during_hash_is_drift_on_tmpfs() {
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap();
    assert!(
        mounts
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("/dev/shm")
                && line.split_whitespace().nth(2) == Some("tmpfs")),
        "gate G4 needs /dev/shm mounted as tmpfs"
    );
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();
    assert_restored_mtime_edit_during_hash_is_drift(dir.path());
}

/// The pinned test root is btrfs on developer hosts and ext4 on Linux CI;
/// detection does not depend on which.
#[cfg(unix)]
#[test]
fn restored_mtime_edit_during_hash_is_drift_on_the_test_root() {
    let dir = tempfile::tempdir().unwrap();
    assert_restored_mtime_edit_during_hash_is_drift(dir.path());
}
