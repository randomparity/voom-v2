use super::*;

use std::os::unix::fs::PermissionsExt;

use voom_core::ErrorCode;

#[tokio::test]
async fn explicit_supported_file_is_single_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.MP4");
    std::fs::write(&path, b"clip").unwrap();

    let discovered = discover_path_filtered(&path, &[]).await.unwrap();

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

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

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

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

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

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

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

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

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

#[test]
fn classify_sidecar_maps_extensions_and_trailer_suffix() {
    use std::path::Path;

    assert_eq!(
        classify_sidecar(Path::new("Movie.srt")),
        Some(SidecarKind::Subtitle)
    );
    assert_eq!(
        classify_sidecar(Path::new("Movie.SRT")),
        Some(SidecarKind::Subtitle)
    );
    assert_eq!(
        classify_sidecar(Path::new("Movie.nfo")),
        Some(SidecarKind::Nfo)
    );
    for image in [
        "Movie-poster.jpg",
        "Movie.jpeg",
        "Movie-fanart.PNG",
        "art.webp",
        "Movie.tbn",
    ] {
        assert_eq!(
            classify_sidecar(Path::new(image)),
            Some(SidecarKind::Poster),
            "{image}"
        );
    }
    assert_eq!(
        classify_sidecar(Path::new("Movie-trailer.mkv")),
        Some(SidecarKind::Trailer)
    );
    assert_eq!(
        classify_sidecar(Path::new("Movie.trailer.mp4")),
        Some(SidecarKind::Trailer)
    );
    // Plain media, unsupported extensions, and extensionless files are not sidecars.
    assert_eq!(classify_sidecar(Path::new("Movie.mkv")), None);
    assert_eq!(classify_sidecar(Path::new("notes.txt")), None);
    assert_eq!(classify_sidecar(Path::new("art.bmp")), None);
    assert_eq!(classify_sidecar(Path::new("README")), None);
}

#[tokio::test]
async fn directory_discovery_attaches_v1_sidecar_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let media = write_file(dir.path(), "Movie.mkv", b"media");
    let nfo = write_file(dir.path(), "Movie.nfo", b"nfo");
    let poster = write_file(dir.path(), "Movie-poster.jpg", b"poster");
    let fanart = write_file(dir.path(), "Movie-fanart.png", b"fanart");
    let trailer = write_file(dir.path(), "Movie-trailer.mkv", b"trailer");
    let srt = write_file(dir.path(), "Movie.srt", b"subtitle");

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

    assert_eq!(
        discovered.candidates.len(),
        1,
        "trailer must not be a candidate"
    );
    let candidate = &discovered.candidates[0];
    assert_eq!(candidate.path, media);
    let by_path: std::collections::BTreeMap<_, _> = candidate
        .sidecars
        .iter()
        .map(|sidecar| (sidecar.path.clone(), sidecar.kind))
        .collect();
    assert_eq!(by_path.get(&nfo), Some(&SidecarKind::Nfo));
    assert_eq!(by_path.get(&poster), Some(&SidecarKind::Poster));
    assert_eq!(by_path.get(&fanart), Some(&SidecarKind::Poster));
    assert_eq!(by_path.get(&trailer), Some(&SidecarKind::Trailer));
    assert_eq!(by_path.get(&srt), Some(&SidecarKind::Subtitle));
    assert_eq!(candidate.sidecars.len(), 5);
    assert!(discovered.skipped.is_empty());
}

#[tokio::test]
async fn orphan_trailer_without_base_media_is_skipped_not_ingested_as_primary() {
    let dir = tempfile::tempdir().unwrap();
    let trailer = write_file(dir.path(), "Lonely-trailer.mkv", b"trailer");

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

    assert!(discovered.candidates.is_empty());
    assert_eq!(discovered.skipped.len(), 1);
    assert_eq!(discovered.skipped[0].path, trailer);
    assert_eq!(
        discovered.skipped[0].status,
        FileScanStatus::UnsupportedExtension
    );
}

#[tokio::test]
async fn unsupported_image_extension_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let _media = write_file(dir.path(), "Movie.mkv", b"media");
    let bmp = write_file(dir.path(), "Movie.bmp", b"bitmap");

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

    assert_eq!(discovered.candidates.len(), 1);
    assert!(discovered.candidates[0].sidecars.is_empty());
    assert_eq!(discovered.skipped.len(), 1);
    assert_eq!(discovered.skipped[0].path, bmp);
}

#[tokio::test]
async fn sidecar_attaches_to_longest_matching_stem_across_hyphen_separator() {
    let dir = tempfile::tempdir().unwrap();
    let shorter = write_file(dir.path(), "Movie.mkv", b"short");
    let longer = write_file(dir.path(), "Movie.Part1.mkv", b"long");
    let poster = write_file(dir.path(), "Movie.Part1-poster.jpg", b"poster");

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

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
    assert_eq!(longer.sidecars.len(), 1);
    assert_eq!(longer.sidecars[0].path, poster);
    assert_eq!(longer.sidecars[0].kind, SidecarKind::Poster);
}

#[tokio::test]
async fn directory_skipped_entries_are_returned_in_lexicographic_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("z.txt"), b"z").unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("clip.mp4"), b"clip").unwrap();

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

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

    let err = discover_path_filtered(&path, &[]).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::BadArgs);
}

#[tokio::test]
async fn explicit_symlink_is_rejected_before_canonicalization() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clip.mp4");
    let link = dir.path().join("link.mp4");
    std::fs::write(&target, b"clip").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = discover_path_filtered(&link, &[]).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::BadArgs);
}

#[tokio::test]
async fn directory_walk_does_not_traverse_symlinked_directory() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside.mp4"), b"outside").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();

    let discovered = discover_path_filtered(root.path(), &[]).await.unwrap();

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

    let discovered = discover_path_filtered(root.path(), &[]).await.unwrap();

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

#[tokio::test]
async fn extension_allowlist_restricts_primary_media() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keep.mkv"), b"a").unwrap();
    std::fs::write(dir.path().join("drop.mp4"), b"b").unwrap();

    let discovered = discover_path_filtered(dir.path(), &["mkv".to_owned()])
        .await
        .unwrap();

    let names: Vec<_> = discovered
        .candidates
        .iter()
        .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["keep.mkv"]);
    // The mp4 is now unsupported under the allowlist, so it is skipped.
    assert!(
        discovered
            .skipped
            .iter()
            .any(|s| s.path.file_name().unwrap() == "drop.mp4")
    );
}

#[tokio::test]
async fn empty_allowlist_uses_builtin_extensions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.mkv"), b"a").unwrap();
    std::fs::write(dir.path().join("b.mp4"), b"b").unwrap();

    let discovered = discover_path_filtered(dir.path(), &[]).await.unwrap();

    assert_eq!(discovered.candidates.len(), 2);
}

#[tokio::test]
async fn allowlist_is_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.MKV");
    std::fs::write(&path, b"a").unwrap();

    let discovered = discover_path_filtered(&path, &["mkv".to_owned()])
        .await
        .unwrap();

    assert_eq!(discovered.candidates.len(), 1);
}
