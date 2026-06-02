use super::*;

use std::os::unix::fs::PermissionsExt;

use voom_core::ErrorCode;

#[tokio::test]
async fn explicit_supported_file_is_single_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.MP4");
    std::fs::write(&path, b"clip").unwrap();

    let discovered = discover_path(&path).await.unwrap();

    assert_eq!(discovered.mode, ScanMode::File);
    assert_eq!(discovered.candidates.len(), 1);
    assert!(discovered.skipped.is_empty());
    assert_eq!(discovered.root, path.canonicalize().unwrap());
    assert_eq!(discovered.candidates[0].path, path.canonicalize().unwrap());
    assert!(discovered.candidates[0].path.is_absolute());
}

#[tokio::test]
async fn directory_discovery_returns_supported_media_in_lexicographic_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("b")).unwrap();
    std::fs::create_dir(dir.path().join("a")).unwrap();
    std::fs::write(dir.path().join("b").join("z.mkv"), b"z").unwrap();
    std::fs::write(dir.path().join("a").join("a.mp4"), b"a").unwrap();
    std::fs::write(dir.path().join("a").join("notes.txt"), b"skip").unwrap();

    let discovered = discover_path(dir.path()).await.unwrap();

    assert_eq!(discovered.mode, ScanMode::Directory);
    assert_eq!(discovered.root, dir.path().canonicalize().unwrap());
    let names: Vec<_> = discovered
        .candidates
        .iter()
        .map(|candidate| {
            candidate
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, vec!["a.mp4", "z.mkv"]);
    assert_eq!(discovered.skipped.len(), 1);
    assert_eq!(
        discovered.skipped[0].status,
        FileScanStatus::UnsupportedExtension
    );
}

#[tokio::test]
async fn directory_discovery_attaches_matching_srt_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let media = write_file(dir.path(), "Movie.Name.mkv", b"media");
    let exact = write_file(dir.path(), "Movie.Name.srt", b"subtitle");
    let sidecar = write_file(dir.path(), "Movie.Name.eng.srt", b"subtitle");
    let other = write_file(dir.path(), "Other.eng.srt", b"subtitle");

    let discovered = discover_path(dir.path()).await.unwrap();

    assert_eq!(discovered.candidates.len(), 1);
    assert_eq!(discovered.candidates[0].path, media);
    assert_eq!(
        discovered.candidates[0]
            .sidecars
            .iter()
            .map(|sidecar| sidecar.path.as_path())
            .collect::<Vec<_>>(),
        vec![sidecar.as_path(), exact.as_path()]
    );
    assert_eq!(
        discovered
            .skipped
            .iter()
            .map(|file| file.path.as_path())
            .collect::<Vec<_>>(),
        vec![other.as_path()]
    );
}

#[tokio::test]
async fn directory_discovery_assigns_sidecar_to_longest_matching_media_stem() {
    let dir = tempfile::tempdir().unwrap();
    let shorter = write_file(dir.path(), "Movie.mkv", b"short");
    let longer = write_file(dir.path(), "Movie.Part1.mkv", b"long");
    let sidecar = write_file(dir.path(), "Movie.Part1.eng.srt", b"subtitle");

    let discovered = discover_path(dir.path()).await.unwrap();

    let shorter = discovered
        .candidates
        .iter()
        .find(|candidate| candidate.path == shorter)
        .unwrap();
    let longer = discovered
        .candidates
        .iter()
        .find(|candidate| candidate.path == longer)
        .unwrap();
    assert!(shorter.sidecars.is_empty());
    assert_eq!(longer.sidecars[0].path, sidecar);
}

#[tokio::test]
async fn unsupported_file_inside_directory_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("clip.mp4"), b"clip").unwrap();
    std::fs::write(dir.path().join("notes.txt"), b"notes").unwrap();

    let discovered = discover_path(dir.path()).await.unwrap();

    assert_eq!(discovered.candidates.len(), 1);
    assert_eq!(discovered.skipped.len(), 1);
    assert_eq!(
        discovered.skipped[0].status,
        FileScanStatus::UnsupportedExtension
    );
}

fn write_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    std::fs::canonicalize(path).unwrap()
}

#[tokio::test]
async fn directory_skipped_entries_are_returned_in_lexicographic_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("z.txt"), b"z").unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("clip.mp4"), b"clip").unwrap();

    let discovered = discover_path(dir.path()).await.unwrap();

    let names: Vec<_> = discovered
        .skipped
        .iter()
        .map(|skipped| {
            skipped
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, vec!["a.txt", "z.txt"]);
}

#[tokio::test]
async fn unsupported_explicit_file_is_bad_args() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, b"notes").unwrap();

    let err = discover_path(&path).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::BadArgs);
}

#[tokio::test]
async fn explicit_symlink_is_rejected_before_canonicalization() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clip.mp4");
    let link = dir.path().join("link.mp4");
    std::fs::write(&target, b"clip").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = discover_path(&link).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::BadArgs);
}

#[tokio::test]
async fn directory_walk_does_not_traverse_symlinked_directory() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside.mp4"), b"outside").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();

    let discovered = discover_path(root.path()).await.unwrap();

    assert!(
        discovered
            .candidates
            .iter()
            .all(|candidate| !candidate.path.ends_with("outside.mp4"))
    );
    assert_eq!(discovered.skipped.len(), 1);
    assert_eq!(discovered.skipped[0].status, FileScanStatus::Symlink);
}

#[tokio::test]
async fn unreadable_child_directory_is_skipped_without_aborting_scan() {
    let root = tempfile::tempdir().unwrap();
    let readable = root.path().join("readable.mp4");
    std::fs::write(&readable, b"media").unwrap();
    let unreadable = root.path().join("unreadable");
    std::fs::create_dir(&unreadable).unwrap();
    let mut permissions = std::fs::metadata(&unreadable).unwrap().permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&unreadable, permissions).unwrap();

    let discovered = discover_path(root.path()).await.unwrap();

    let mut restore = std::fs::metadata(&unreadable).unwrap().permissions();
    restore.set_mode(0o700);
    std::fs::set_permissions(&unreadable, restore).unwrap();
    assert_eq!(discovered.candidates.len(), 1);
    assert_eq!(
        discovered.candidates[0].path,
        readable.canonicalize().unwrap()
    );
    assert_eq!(discovered.skipped.len(), 1);
    assert_eq!(discovered.skipped[0].status, FileScanStatus::Inaccessible);
}
