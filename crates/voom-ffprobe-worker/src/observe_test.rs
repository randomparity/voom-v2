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

const MIB: usize = 1024 * 1024;
/// BLAKE3 of `patterned(2 * MIB + 1)`. Content hashes are a persisted contract, so the
/// read buffer size must never change them.
const PINNED_MULTI_CHUNK_HASH: &str =
    "blake3:52dc212cb4cc61cb94d25bd7b1d47b256e4c3a6d68956df50c235c37a2aeacd7";

fn patterned(len: usize) -> Vec<u8> {
    (0..=250_u8).cycle().take(len).collect()
}

async fn observe_written(dir: &Path, bytes: &[u8]) -> Option<ObservedFileFacts> {
    let path = dir.join(format!("{}.bin", bytes.len()));
    let write_result = std::fs::write(&path, bytes);
    assert!(write_result.is_ok());
    let observed_result = Box::pin(observe_file_facts(&path)).await;
    assert!(observed_result.is_ok());
    observed_result.ok()
}

#[tokio::test]
async fn observe_file_facts_hash_matches_one_shot_blake3_at_buffer_boundaries() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    for len in [MIB, 2 * MIB, 2 * MIB + 1] {
        let bytes = patterned(len);

        let observed = Box::pin(observe_written(dir.path(), &bytes)).await;

        assert_eq!(
            observed.map(|facts| (facts.size_bytes, facts.content_hash)),
            Some((
                u64::try_from(len).unwrap_or(u64::MAX),
                format!("blake3:{}", blake3::hash(&bytes).to_hex())
            )),
            "length {len}"
        );
    }
}

#[tokio::test]
async fn observe_file_facts_pins_the_multi_chunk_content_hash() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };

    let observed = Box::pin(observe_written(dir.path(), &patterned(2 * MIB + 1))).await;

    assert_eq!(
        observed.map(|facts| facts.content_hash).as_deref(),
        Some(PINNED_MULTI_CHUNK_HASH)
    );
}
