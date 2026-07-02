use voom_core::{ErrorCode, LibraryId, LibraryRootId};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryRootKind, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};

use super::{RootBlockReason, RootScanOutcome};
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

#[tokio::test]
async fn missing_root_is_not_found() {
    let (cp, _tmp) = cp().await;
    let err = cp.scan_library_root(LibraryRootId(4242)).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn disabled_root_blocks_without_scanning() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    // Point at a path that does NOT exist: if discovery ran, it would error
    // instead of returning Blocked — so a clean Blocked proves nothing was
    // scanned.
    let root = cp
        .create_library_root(new_root(lib.id, "/nonexistent/films"))
        .await
        .unwrap();
    cp.set_library_root_enabled(root.id, false).await.unwrap();

    let outcome = cp.scan_library_root(root.id).await.unwrap();
    match outcome {
        RootScanOutcome::Blocked(blocked) => {
            assert_eq!(blocked.reason, RootBlockReason::RootDisabled);
            assert_eq!(blocked.library_root_id, root.id);
            assert_eq!(blocked.library_id, lib.id);
        }
        RootScanOutcome::Scanned(_) => panic!("disabled root must not scan"),
    }
}

#[tokio::test]
async fn disabled_library_blocks_the_root() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    let root = cp
        .create_library_root(new_root(lib.id, "/nonexistent/films"))
        .await
        .unwrap();
    cp.set_library_enabled(lib.id, false).await.unwrap();

    let outcome = cp.scan_library_root(root.id).await.unwrap();
    match outcome {
        RootScanOutcome::Blocked(blocked) => {
            assert_eq!(blocked.reason, RootBlockReason::LibraryDisabled);
        }
        RootScanOutcome::Scanned(_) => panic!("disabled library must block the root"),
    }
}

#[tokio::test]
async fn enabled_root_over_empty_dir_scans_nothing() {
    let (cp, _tmp) = cp().await;
    let dir = tempfile::tempdir().unwrap();
    let lib = cp.create_library(new_library("films")).await.unwrap();
    let root = cp
        .create_library_root(new_root(lib.id, dir.path().to_str().unwrap()))
        .await
        .unwrap();

    let outcome = cp.scan_library_root(root.id).await.unwrap();
    match outcome {
        RootScanOutcome::Scanned(report) => {
            assert_eq!(report.summary.discovered, 0);
            assert_eq!(report.summary.ingested, 0);
        }
        RootScanOutcome::Blocked(_) => panic!("enabled root must scan"),
    }
}
