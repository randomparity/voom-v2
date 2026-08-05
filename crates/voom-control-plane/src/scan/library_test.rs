use voom_core::{
    ErrorCode, LibraryId, NodeId, ProviderLocator, StorageProviderKind, StorageRootId,
};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
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

fn new_root(library_id: LibraryId, owner_node_id: NodeId, path: &str) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(path.to_owned()).unwrap(),
        display_locator: path.to_owned(),
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extension_allowlist: Vec::new(),
        scan_mode: LibraryScanMode::ManualRecursive,
        symlink_policy: SymlinkPolicy::Reject,
        hidden_file_policy: HiddenFilePolicy::Ignore,
        max_depth: None,
        stability_seconds: 0,
        debounce_seconds: 0,
        default_output_root_id: None,
        default_staging_root_id: None,
        default_backup_root_id: None,
        enabled: true,
    }
}

async fn owner(cp: &crate::ControlPlane, name: &str) -> NodeId {
    let id = sqlx::query(
        "INSERT INTO nodes \
         (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (?, 'local', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .bind(name)
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    NodeId(u64::try_from(id).unwrap())
}

#[tokio::test]
async fn missing_root_is_not_found() {
    let (cp, _tmp) = cp().await;
    let err = cp.scan_library_root(StorageRootId(4242)).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn disabled_root_blocks_without_scanning() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    let owner = owner(&cp, "disabled-root-owner").await;
    // Point at a path that does NOT exist: if discovery ran, it would error
    // instead of returning Blocked — so a clean Blocked proves nothing was
    // scanned.
    let root = cp
        .create_library_root(new_root(lib.id, owner, "/nonexistent/films"))
        .await
        .unwrap();
    cp.set_library_root_enabled(root.id, false).await.unwrap();

    let outcome = cp.scan_library_root(root.id).await.unwrap();
    match outcome {
        RootScanOutcome::Blocked(blocked) => {
            assert_eq!(blocked.reason, RootBlockReason::RootDisabled);
            assert_eq!(blocked.storage_root_id, root.id);
            assert_eq!(blocked.library_id, lib.id);
        }
        RootScanOutcome::Scanned(_) => panic!("disabled root must not scan"),
    }
}

#[tokio::test]
async fn disabled_library_blocks_the_root() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    let owner = owner(&cp, "disabled-library-owner").await;
    let root = cp
        .create_library_root(new_root(lib.id, owner, "/nonexistent/films"))
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
async fn root_owned_by_another_node_blocks_before_filesystem_access() {
    let (cp, _tmp) = cp().await;
    let lib = cp
        .create_library(new_library("remote-films"))
        .await
        .unwrap();
    let remote_owner = owner(&cp, "remote-root-owner").await;
    let root = cp
        .create_library_root(new_root(lib.id, remote_owner, "/nonexistent/remote-films"))
        .await
        .unwrap();
    cp.activate_library_root(root.id, "remote-fixture".to_owned())
        .await
        .unwrap();

    let outcome = cp.scan_library_root(root.id).await.unwrap();

    match outcome {
        RootScanOutcome::Blocked(blocked) => {
            assert_eq!(blocked.reason, RootBlockReason::OwnerNotLocal);
        }
        RootScanOutcome::Scanned(_) => panic!("remote root must not touch local bytes"),
    }
}

#[tokio::test]
async fn enabled_root_over_empty_dir_scans_nothing() {
    let (cp, _tmp) = cp().await;
    let dir = tempfile::tempdir().unwrap();
    let lib = cp.create_library(new_library("films")).await.unwrap();
    let root = cp
        .create_library_root(new_root(
            lib.id,
            NodeId(9_000_001),
            dir.path().to_str().unwrap(),
        ))
        .await
        .unwrap();
    cp.activate_library_root(root.id, "empty-dir-fixture".to_owned())
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
