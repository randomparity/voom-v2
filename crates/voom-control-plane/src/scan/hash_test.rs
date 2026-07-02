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

#[cfg(unix)]
#[tokio::test]
async fn hardlinks_share_dev_ino_but_a_copy_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("a.mkv");
    let hardlink = dir.path().join("b.mkv");
    let copy = dir.path().join("c.mkv");
    std::fs::write(&original, b"movie-bytes").unwrap();
    std::fs::hard_link(&original, &hardlink).unwrap();
    std::fs::copy(&original, &copy).unwrap();

    let a = observe_candidate_file(&original).await.unwrap();
    let b = observe_candidate_file(&hardlink).await.unwrap();
    let c = observe_candidate_file(&copy).await.unwrap();

    // The hardlink is the same physical object: identical (dev, ino).
    assert_eq!((a.dev, a.ino), (b.dev, b.ino));
    assert!(a.dev.is_some() && a.ino.is_some());
    // Two links to the file: nlink >= 2 for the linked pair.
    assert!(a.nlink.unwrap() >= 2);
    // The copy has identical content but a distinct inode — not a hardlink.
    assert_eq!(a.content_hash, c.content_hash);
    assert_ne!(a.ino, c.ino);
}
