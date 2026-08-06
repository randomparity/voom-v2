use super::*;

use voom_core::{LibraryId, NodeId, ProviderLocator, StorageProviderKind};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};

fn root_input(library_id: LibraryId, path: &std::path::Path) -> NewLibraryRoot {
    let locator = path.to_string_lossy().into_owned();
    NewLibraryRoot {
        library_id,
        owner_node_id: NodeId(9_000_001),
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(locator.clone()).unwrap(),
        display_locator: locator,
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

async fn library(cp: &ControlPlane, slug: &str) -> LibraryId {
    cp.create_library(NewLibrary {
        slug: slug.to_owned(),
        display_name: slug.to_owned(),
        media_kind: LibraryMediaKind::Movie,
        description: None,
        enabled: true,
    })
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn artifact_target_rejects_cross_library_default_root_as_corrupt_storage() {
    let (cp, _db) = crate::cases::cp().await;
    let source_dir = tempfile::tempdir().unwrap();
    let target_dir = tempfile::tempdir().unwrap();
    let source_library_id = library(&cp, "source-library").await;
    let target_library_id = library(&cp, "target-library").await;
    let source_root = cp
        .create_library_root(root_input(source_library_id, source_dir.path()))
        .await
        .unwrap();
    let target_root = cp
        .create_library_root(root_input(target_library_id, target_dir.path()))
        .await
        .unwrap();
    cp.activate_library_root(source_root.id, "source-root".to_owned())
        .await
        .unwrap();
    cp.activate_library_root(target_root.id, "target-root".to_owned())
        .await
        .unwrap();
    sqlx::query("UPDATE library_roots SET default_output_root_id = ? WHERE id = ?")
        .bind(i64::try_from(target_root.id.0).unwrap())
        .bind(i64::try_from(source_root.id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let error = resolve_artifact_target(
        &cp,
        "test artifact",
        source_root.id,
        &target_dir.path().join("output.mkv"),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("belongs to library"));
}

#[cfg(unix)]
#[tokio::test]
async fn configured_root_alias_resolves_but_descendant_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let (cp, _db) = crate::cases::cp().await;
    let temp = tempfile::tempdir().unwrap();
    let real_root = temp.path().join("real-root");
    let alias_root = temp.path().join("root-alias");
    std::fs::create_dir(&real_root).unwrap();
    symlink(&real_root, &alias_root).unwrap();
    let safe_path = real_root.join("safe.mkv");
    std::fs::write(&safe_path, b"safe").unwrap();
    let library_id = library(&cp, "root-alias").await;
    let root = cp
        .create_library_root(root_input(library_id, &alias_root))
        .await
        .unwrap();
    cp.activate_library_root(root.id, "root-alias".to_owned())
        .await
        .unwrap();

    let safe = resolve_root_relative_existing_path(
        &cp,
        "test artifact",
        root.id,
        &ProviderRelativeLocator::new("safe.mkv".to_owned()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(safe, safe_path.canonicalize().unwrap());

    let real_nested = real_root.join("real-nested");
    std::fs::create_dir(&real_nested).unwrap();
    std::fs::write(real_nested.join("unsafe.mkv"), b"unsafe").unwrap();
    symlink(&real_nested, real_root.join("nested-alias")).unwrap();
    let error = resolve_root_relative_existing_path(
        &cp,
        "test artifact",
        root.id,
        &ProviderRelativeLocator::new("nested-alias/unsafe.mkv".to_owned()).unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "CONFIG_INVALID");
    assert!(error.to_string().contains("must not traverse a symlink"));
}
