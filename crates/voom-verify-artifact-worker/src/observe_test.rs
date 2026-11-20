use std::path::Path;

use super::*;

#[tokio::test]
async fn observe_file_facts_reports_size_hash_and_mtime_for_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("artifact.bin");
    tokio::fs::write(&path, b"verified bytes").await.unwrap();

    let observed = observe_file_facts(&path).await.unwrap();

    assert_eq!(observed.size_bytes, 14);
    assert_eq!(
        observed.content_hash,
        format!("blake3:{}", blake3::hash(b"verified bytes").to_hex())
    );
    assert!(observed.modified_at.is_some());
    assert_eq!(observed.local_file_key, None);
}

#[tokio::test]
async fn observe_file_facts_rejects_missing_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.bin");

    let err = observe_file_facts(&path).await.unwrap_err();
    let message = err.to_string();

    assert!(message.contains("artifact unavailable"), "{message}");
    assert!(message.contains(&path.display().to_string()), "{message}");
}

#[tokio::test]
async fn observe_file_facts_rejects_non_regular_files_without_following_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.bin");
    tokio::fs::write(&target, b"target").await.unwrap();
    let link = dir.path().join("link.bin");
    make_symlink(&target, &link);

    let err = observe_file_facts(&link).await.unwrap_err();
    let message = err.to_string();

    assert!(message.contains("not a regular file"), "{message}");
    assert!(message.contains(&link.display().to_string()), "{message}");
}

#[cfg(unix)]
#[tokio::test]
async fn no_follow_open_rejects_leaf_symlink_after_path_check() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.bin");
    tokio::fs::write(&target, b"target").await.unwrap();
    let link = dir.path().join("link.bin");
    make_symlink(&target, &link);

    let err = open_regular_file_no_follow(&link).await.unwrap_err();
    let message = err.to_string();

    assert!(message.contains("artifact unavailable"), "{message}");
    assert!(message.contains(&link.display().to_string()), "{message}");
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn make_symlink(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_file(target, link).unwrap();
}

const MIB: usize = 1024 * 1024;
/// BLAKE3 of `patterned(2 * MIB + 1)`. Content hashes are a persisted contract, so the
/// read buffer size must never change them.
const PINNED_MULTI_CHUNK_HASH: &str =
    "blake3:52dc212cb4cc61cb94d25bd7b1d47b256e4c3a6d68956df50c235c37a2aeacd7";

fn patterned(len: usize) -> Vec<u8> {
    (0..=250_u8).cycle().take(len).collect()
}

#[tokio::test]
async fn observe_file_facts_hash_matches_one_shot_blake3_at_buffer_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    for len in [MIB, 2 * MIB, 2 * MIB + 1] {
        let bytes = patterned(len);
        let path = dir.path().join(format!("{len}.bin"));
        tokio::fs::write(&path, &bytes).await.unwrap();

        let facts = Box::pin(observe_file_facts(&path)).await.unwrap();

        assert_eq!(usize::try_from(facts.size_bytes).ok(), Some(len));
        assert_eq!(
            facts.content_hash,
            format!("blake3:{}", blake3::hash(&bytes).to_hex()),
            "length {len}"
        );
    }
}

#[tokio::test]
async fn observe_file_facts_pins_the_multi_chunk_content_hash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("multi-chunk.bin");
    tokio::fs::write(&path, patterned(2 * MIB + 1))
        .await
        .unwrap();

    let facts = Box::pin(observe_file_facts(&path)).await.unwrap();

    assert_eq!(facts.content_hash, PINNED_MULTI_CHUNK_HASH);
}
