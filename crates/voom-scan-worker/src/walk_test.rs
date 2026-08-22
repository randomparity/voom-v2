use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::symlink;
use std::path::Path;

use tempfile::TempDir;

use super::*;

fn write_file(root: &Path, relative: &str, contents: &[u8]) {
    let target = root.join(relative);
    if let Some(parent) = target.parent() {
        assert!(fs::create_dir_all(parent).is_ok(), "create {parent:?}");
    }
    assert!(fs::write(target, contents).is_ok(), "write {relative:?}");
}

fn scan(dir: &Path, allowlist: &[String]) -> Option<WalkOutcome> {
    let walked = scan_root(dir, allowlist);
    assert!(
        walked.is_ok(),
        "walk failed: {:?}",
        walked.as_ref().err().map(ToString::to_string)
    );
    walked.ok()
}

fn primary_locators(outcome: &WalkOutcome) -> Vec<&str> {
    outcome
        .candidates
        .iter()
        .map(|candidate| candidate.primary.locator.as_str())
        .collect()
}

#[test]
fn discovery_order_is_deterministic_across_runs() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    // Deliberately scrambled creation order; read_dir order is unspecified,
    // so the walker must sort per directory itself.
    write_file(dir.path(), "c.mkv", b"x");
    write_file(dir.path(), "a.mkv", b"x");
    write_file(dir.path(), "b.mkv", b"x");
    write_file(dir.path(), "nested/z.mkv", b"x");
    write_file(dir.path(), "nested/y.mkv", b"x");

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    assert_eq!(
        primary_locators(&outcome),
        vec!["a.mkv", "b.mkv", "c.mkv", "nested/y.mkv", "nested/z.mkv"]
    );
    assert_eq!(outcome.skipped_count, 0);
}

#[test]
fn symlinks_inside_root_are_skipped_and_counted_without_being_followed() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    write_file(dir.path(), "real.mkv", b"x");
    write_file(dir.path(), "subdir/inner.mkv", b"x");
    assert!(
        symlink(dir.path().join("real.mkv"), dir.path().join("alias.mkv")).is_ok(),
        "seed file symlink"
    );
    assert!(
        symlink(dir.path().join("subdir"), dir.path().join("dir-link")).is_ok(),
        "seed dir symlink"
    );

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    assert_eq!(
        primary_locators(&outcome),
        vec!["real.mkv", "subdir/inner.mkv"]
    );
    assert_eq!(outcome.skipped_count, 2);
}

#[test]
fn leaf_symlink_pointing_outside_root_is_skipped_without_following() {
    let Ok(outside) = TempDir::new() else {
        return;
    };
    let Ok(dir) = TempDir::new() else {
        return;
    };
    let outside_target = outside.path().join("secret.txt");
    assert!(fs::write(&outside_target, b"must-not-be-scanned").is_ok());
    write_file(dir.path(), "movie.mkv", b"x");
    assert!(
        symlink(outside_target, dir.path().join("link.mkv")).is_ok(),
        "seed escaping symlink"
    );

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    assert_eq!(primary_locators(&outcome), vec!["movie.mkv"]);
    assert_eq!(outcome.skipped_count, 1);
}

#[test]
fn traversal_and_unsafe_components_never_become_locators() {
    assert!(build_relative_locator("../escape").is_err());
    assert!(build_relative_locator("a/../b.mkv").is_err());
    assert!(build_relative_locator("a/./b.mkv").is_err());
    assert!(build_relative_locator("a//b.mkv").is_err());
    assert!(build_relative_locator("").is_err());
    assert!(build_relative_locator("a/b\0c.mkv").is_err());
    // Leading-dash and ordinary names stay untouched.
    assert!(build_relative_locator("-foo.mkv").is_ok());
    assert!(build_relative_locator("dir/sub/file.mkv").is_ok());
    assert!(component_is_safe("plain"));
    assert!(!component_is_safe("."));
    assert!(!component_is_safe(".."));
    assert!(!component_is_safe(""));
    assert!(!component_is_safe("n\0ul"));
}

#[test]
fn escape_guard_rejects_paths_outside_the_canonical_root() {
    let root = Path::new("/srv/media");
    assert!(joined_escapes_root(root, Path::new("/srv/other/x.mkv")));
    assert!(joined_escapes_root(
        root,
        Path::new("/srv/media-sibling/x.mkv")
    ));
    assert!(!joined_escapes_root(root, Path::new("/srv/media/a/x.mkv")));
    assert!(!joined_escapes_root(root, Path::new("/srv/media/x.mkv")));
}

#[test]
fn allowlist_restricts_primaries_while_sidecars_stay_attached() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    write_file(dir.path(), "song.mp3", b"x");
    write_file(dir.path(), "song.srt", b"x");
    write_file(dir.path(), "movie.mkv", b"x");

    let Some(outcome) = scan(dir.path(), &["mp3".to_owned()]) else {
        return;
    };

    assert_eq!(primary_locators(&outcome), vec!["song.mp3"]);
    assert_eq!(outcome.candidates.len(), 1);
    assert_eq!(
        outcome.candidates[0]
            .sidecars
            .iter()
            .map(|sidecar| sidecar.locator.as_str())
            .collect::<Vec<_>>(),
        vec!["song.srt"]
    );
    assert_eq!(outcome.candidates[0].primary.kind, None);
    // movie.mkv fell outside the allowlist and is counted, not emitted.
    assert_eq!(outcome.skipped_count, 1);
}

#[test]
fn empty_root_completes_with_discovered_zero() {
    let Ok(dir) = TempDir::new() else {
        return;
    };

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    assert!(outcome.candidates.is_empty());
    assert_eq!(outcome.skipped_count, 0);
}

#[test]
fn non_utf8_filename_is_skipped_and_counted() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    write_file(dir.path(), "good.mkv", b"x");
    let bad_name = std::ffi::OsStr::from_bytes(b"\xff\xfebad.mkv");
    // APFS rejects non-UTF-8 filenames outright, so macOS runners cannot seed
    // the fixture; the skip-counting contract still holds on ext4-style
    // filesystems, and elsewhere the test degrades to the single good file.
    if fs::write(dir.path().join(bad_name), b"x").is_err() {
        let Some(outcome) = scan(dir.path(), &[]) else {
            return;
        };
        assert_eq!(primary_locators(&outcome), vec!["good.mkv"]);
        return;
    }

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    assert_eq!(primary_locators(&outcome), vec!["good.mkv"]);
    assert_eq!(outcome.skipped_count, 1);
}

#[test]
fn leading_dash_filename_passes_through_untouched() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    write_file(dir.path(), "-foo.mkv", b"x");

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    assert_eq!(primary_locators(&outcome), vec!["-foo.mkv"]);
    assert_eq!(outcome.candidates[0].primary.kind, None);
    assert!(
        outcome.candidates[0]
            .primary
            .locator
            .as_str()
            .starts_with('-')
    );
}

#[test]
fn longest_stem_matching_groups_sidecars_from_the_walk() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    write_file(dir.path(), "movie/movie.mkv", b"x");
    write_file(dir.path(), "movie/movie.srt", b"x");
    write_file(dir.path(), "movie2/movie2.mkv", b"x");
    write_file(dir.path(), "movie2/movie2.srt", b"x");
    write_file(dir.path(), "orphan/note.srt", b"x");

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    assert_eq!(
        primary_locators(&outcome),
        vec!["movie/movie.mkv", "movie2/movie2.mkv"]
    );
    assert_eq!(
        outcome.candidates[0]
            .sidecars
            .iter()
            .map(|sidecar| sidecar.locator.as_str())
            .collect::<Vec<_>>(),
        vec!["movie/movie.srt"]
    );
    // movie2.srt must anchor to movie2.mkv, never to the shorter-stem
    // movie.mkv — the control-plane ancestor-replacement regression.
    assert_eq!(
        outcome.candidates[1]
            .sidecars
            .iter()
            .map(|sidecar| sidecar.locator.as_str())
            .collect::<Vec<_>>(),
        vec!["movie2/movie2.srt"]
    );
    // The orphan subtitle has no primary and degrades to a counted skip.
    assert_eq!(outcome.skipped_count, 1);
    assert_eq!(
        outcome.candidates[0].sidecars[0].kind,
        Some("external_subtitle")
    );
}

#[test]
fn trailer_suffix_files_are_sidecars_with_trailer_role() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    write_file(dir.path(), "film.mkv", b"x");
    write_file(dir.path(), "film-trailer.mkv", b"x");
    write_file(dir.path(), "other.mp4", b"x");
    write_file(dir.path(), "other.trailer.mp4", b"x");

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    assert_eq!(primary_locators(&outcome), vec!["film.mkv", "other.mp4"]);
    let sidecar_roles = |index: usize| -> Vec<(&str, Option<&str>)> {
        outcome.candidates[index]
            .sidecars
            .iter()
            .map(|sidecar| (sidecar.locator.as_str(), sidecar.kind))
            .collect()
    };
    // Both trailers carry a media extension, so they must route to sidecars
    // with the trailer role — never to primaries.
    assert_eq!(
        sidecar_roles(0),
        vec![("film-trailer.mkv", Some("trailer"))]
    );
    assert_eq!(
        sidecar_roles(1),
        vec![("other.trailer.mp4", Some("trailer"))]
    );
    assert_eq!(outcome.skipped_count, 0);
}

#[test]
fn identity_records_dev_and_ino_from_stat_metadata() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    write_file(dir.path(), "movie.mkv", b"x");

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    let metadata = fs::metadata(dir.path().join("movie.mkv"));
    assert!(metadata.is_ok(), "stat seeded file");
    let Some(metadata) = metadata.ok() else {
        return;
    };
    let expected = format!("dev={};ino={}", metadata.dev(), metadata.ino());
    assert_eq!(
        outcome.candidates[0].primary.provider_object_identity,
        expected
    );
    assert!(metadata.len() >= 1);
    assert_eq!(outcome.candidates[0].primary.size_bytes, metadata.len());
    // RFC 3339 shape: date, separator, time.
    let modified_at = &outcome.candidates[0].primary.modified_at;
    assert!(
        modified_at.len() >= 20,
        "timestamp too short: {modified_at}"
    );
    assert!(modified_at.contains('T'), "not RFC 3339: {modified_at}");
}

#[test]
fn missing_symlinked_and_non_directory_roots_are_root_level_failures() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    let missing = dir.path().join("does-not-exist");
    assert!(matches!(
        scan_root(&missing, &[]),
        Err(RootUnavailable { .. })
    ));

    write_file(dir.path(), "plain.txt", b"x");
    assert!(matches!(
        scan_root(&dir.path().join("plain.txt"), &[]),
        Err(RootUnavailable { .. })
    ));

    let Ok(other) = TempDir::new() else {
        return;
    };
    assert!(
        symlink(other.path(), dir.path().join("root-link")).is_ok(),
        "seed symlinked root"
    );
    assert!(matches!(
        scan_root(&dir.path().join("root-link"), &[]),
        Err(RootUnavailable { .. })
    ));
}

#[test]
fn depth_overflow_degrades_to_a_counted_skip() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    let mut deep = dir.path().to_path_buf();
    for level in 0..=MAX_WALK_DEPTH {
        deep.push(format!("level-{level}"));
        assert!(fs::create_dir_all(&deep).is_ok(), "mkdir level {level}");
    }
    write_file(&deep, "bottom.mkv", b"x");

    let Some(outcome) = scan(dir.path(), &[]) else {
        return;
    };

    // The deepest directories exceed MAX_WALK_DEPTH and are counted as skips
    // instead of recursing without bound.
    assert!(outcome.candidates.is_empty());
    assert!(outcome.skipped_count >= 1);
}
