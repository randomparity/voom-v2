use super::*;

#[tokio::test]
async fn observe_file_facts_reports_blake3_size_and_modified_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.bin");
    tokio::fs::write(&path, b"video bytes").await.unwrap();

    let facts = observe_file_facts(&path).await.unwrap();

    assert_eq!(facts.size_bytes, 11);
    assert_eq!(
        facts.content_hash,
        format!("blake3:{}", blake3::hash(b"video bytes").to_hex())
    );
    assert!(facts.modified_at.is_some());
}
