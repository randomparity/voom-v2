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
