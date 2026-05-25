use super::*;

#[tokio::test]
async fn observed_file_facts_use_blake3_prefix_and_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mp4");
    std::fs::write(&path, b"voom").unwrap();

    let observed = observe_candidate_file(&path).await.unwrap();

    assert_eq!(observed.size_bytes, 4);
    assert_eq!(
        observed.content_hash,
        format!("blake3:{}", blake3::hash(b"voom").to_hex())
    );
    assert!(observed.modified_at.is_some());
}
