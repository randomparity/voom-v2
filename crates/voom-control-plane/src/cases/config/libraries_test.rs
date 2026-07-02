use voom_core::LibraryId;
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryRootKind, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};

use super::paths_overlap;
use crate::cases::cp;

fn new_library(slug: &str) -> NewLibrary {
    NewLibrary {
        slug: slug.to_owned(),
        display_name: slug.to_owned(),
        media_kind: LibraryMediaKind::Movie,
        description: None,
        enabled: true,
    }
}

fn new_root(library_id: LibraryId, path: &str) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        root_kind: LibraryRootKind::LocalPath,
        canonical_path: path.to_owned(),
        display_path: path.to_owned(),
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extension_allowlist: Vec::new(),
        scan_mode: LibraryScanMode::ManualRecursive,
        symlink_policy: SymlinkPolicy::Reject,
        hidden_file_policy: HiddenFilePolicy::Ignore,
        max_depth: None,
        stability_seconds: 0,
        debounce_seconds: 0,
        default_output_root: None,
        default_staging_root: None,
        default_backup_root: None,
        enabled: true,
    }
}

#[test]
fn paths_overlap_is_component_wise_not_string_prefix() {
    // Sibling sharing a textual prefix must NOT overlap.
    assert!(!paths_overlap("/media/movies", "/media/movies-adult"));
    // Nested and ancestor overlap.
    assert!(paths_overlap("/media", "/media/movies"));
    assert!(paths_overlap("/media/movies", "/media"));
    // Identical overlaps.
    assert!(paths_overlap("/media/movies", "/media/movies"));
    // Disjoint do not.
    assert!(!paths_overlap("/media/movies", "/media/shows"));
}

#[tokio::test]
async fn library_and_root_crud_round_trip() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    assert_eq!(cp.list_libraries().await.unwrap().len(), 1);

    let root = cp
        .create_library_root(new_root(lib.id, "/media/films"))
        .await
        .unwrap();
    let fetched = cp.get_library_root(root.id).await.unwrap().unwrap();
    assert_eq!(fetched.canonical_path, "/media/films");
    assert_eq!(cp.list_library_roots(Some(lib.id)).await.unwrap().len(), 1);
}

#[tokio::test]
async fn overlapping_root_is_rejected() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    cp.create_library_root(new_root(lib.id, "/media/films"))
        .await
        .unwrap();

    // Nested under the existing root.
    let nested = cp
        .create_library_root(new_root(lib.id, "/media/films/2024"))
        .await
        .unwrap_err();
    assert_eq!(nested.code(), "CONFLICT");

    // Sibling sharing a textual prefix is allowed.
    cp.create_library_root(new_root(lib.id, "/media/films-4k"))
        .await
        .unwrap();
}

#[tokio::test]
async fn disable_then_enable_library() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    assert!(!cp.set_library_enabled(lib.id, false).await.unwrap().enabled);
    assert!(cp.set_library_enabled(lib.id, true).await.unwrap().enabled);
}
