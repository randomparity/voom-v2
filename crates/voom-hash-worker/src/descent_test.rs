use std::io::Read;

use super::*;

#[test]
fn resolves_nested_locator_and_returns_root_joined_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("films/1989")).unwrap();
    std::fs::write(dir.path().join("films/1989/movie.mkv"), b"voom-bytes").unwrap();

    let mut resolved = resolve_in_root(dir.path(), "films/1989/movie.mkv").unwrap();

    assert_eq!(resolved.path, dir.path().join("films/1989/movie.mkv"));
    let mut contents = Vec::new();
    let read = resolved.file.read_to_end(&mut contents);
    assert!(read.is_ok(), "read failed: {read:?}");
    assert_eq!(&contents, b"voom-bytes");
}

#[test]
fn rejects_escaping_locators_structurally_before_any_syscall() {
    // The root directory does not exist, so a locator that reached the
    // filesystem could only fail with NotFound; classification as
    // InvalidLocator proves the structural pre-check rejected these first.
    let missing_root = std::path::Path::new("/definitely/not/a/voom/root");

    for locator in [
        "../escape.mkv",
        "a/../escape.mkv",
        "",
        ".",
        "a/./b",
        "/abs/movie.mkv",
        "trailing/",
    ] {
        let err = resolve_in_root(missing_root, locator).unwrap_err();
        assert!(
            matches!(&err, DescentError::InvalidLocator(_)),
            "locator {locator:?} must be rejected structurally, got: {err}"
        );
    }
}

#[test]
fn rejects_locator_with_nul_component_structurally() {
    let dir = tempfile::tempdir().unwrap();

    let err = resolve_in_root(dir.path(), "a\0b").unwrap_err();

    assert!(matches!(&err, DescentError::InvalidLocator(_)), "{err}");
}

#[cfg(unix)]
#[test]
fn rejects_symlink_component_in_the_middle_of_the_locator() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real_dir");
    std::fs::create_dir_all(real_dir.join("sub")).unwrap();
    std::fs::write(real_dir.join("sub/f.mkv"), b"outside").unwrap();
    std::os::unix::fs::symlink(&real_dir, dir.path().join("a_dir")).unwrap();

    // A symlinked directory component must not redirect the descent outside
    // the root; otherwise `a_dir` would alias an arbitrary directory.
    let err = resolve_in_root(dir.path(), "a_dir/sub/f.mkv").unwrap_err();

    assert!(
        matches!(&err, DescentError::Rejected(message) if message.contains("symlink")),
        "expected symlink rejection, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlink_leaf_component() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("real.mkv"), b"payload").unwrap();
    std::os::unix::fs::symlink(dir.path().join("real.mkv"), dir.path().join("link.mkv")).unwrap();

    let err = resolve_in_root(dir.path(), "link.mkv").unwrap_err();

    assert!(
        matches!(&err, DescentError::Rejected(message) if message.contains("symlink")),
        "expected symlink rejection, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn reports_missing_file_as_not_found() {
    let dir = tempfile::tempdir().unwrap();

    let err = resolve_in_root(dir.path(), "ghosts/movie.mkv").unwrap_err();

    assert!(matches!(&err, DescentError::NotFound(_)), "{err}");
}
