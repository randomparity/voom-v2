use super::*;

use voom_core::{LibraryId, NodeId, ProviderLocator, StorageProviderKind};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryRootUpdate, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
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

async fn active_root(
    cp: &ControlPlane,
    library_id: LibraryId,
    path: &std::path::Path,
    identity: &str,
) -> StorageRootId {
    let root = cp
        .create_library_root(root_input(library_id, path))
        .await
        .unwrap();
    cp.activate_library_root(root.id, identity.to_owned())
        .await
        .unwrap();
    root.id
}

async fn set_defaults(
    cp: &ControlPlane,
    root: StorageRootId,
    staging: Option<StorageRootId>,
    output: Option<StorageRootId>,
) {
    cp.update_library_root(
        root,
        LibraryRootUpdate {
            default_staging_root_id: Some(staging),
            default_output_root_id: Some(output),
            ..LibraryRootUpdate::default()
        },
    )
    .await
    .unwrap();
}

/// Canonical sibling directories `names` under one temp dir, so containment
/// compares canonical paths (macOS `/var` is a symlink to `/private/var`).
fn sibling_dirs(names: &[&str]) -> (tempfile::TempDir, Vec<std::path::PathBuf>) {
    let parent = tempfile::tempdir().unwrap();
    let base = parent.path().canonicalize().unwrap();
    let dirs = names
        .iter()
        .map(|name| {
            let dir = base.join(name);
            std::fs::create_dir(&dir).unwrap();
            dir
        })
        .collect();
    (parent, dirs)
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
async fn output_target_rejects_cross_library_default_root_as_corrupt_storage() {
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

    let error = resolve_output_target(
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

/// `require_contained` is the guard that rejects a commit target outside its
/// storage root. Nothing else in the workspace asserts its rejection branch, so
/// a change that let it pass unconditionally would surface only in a run that
/// misconfigures a root. Issue #491 showed that such runs happen only in the
/// weekly `chaos-e2e` job, which is `#[ignore]`-gated out of `just ci`.
///
/// The escape target is a textual-prefix sibling of the root, not an unrelated
/// directory, so this also fails if the component-wise `starts_with` check is
/// ever replaced by a string prefix comparison.
#[tokio::test]
async fn output_target_rejects_a_target_outside_the_resolved_root() {
    let (cp, _db) = crate::cases::cp().await;
    let parent = tempfile::tempdir().unwrap();
    // Canonicalize before joining: the symlink guard walks the target's
    // ancestors, and on macOS the temp directory sits under `/var`, which is a
    // symlink to `/private/var`. Without this the guard rejects the path for
    // traversing a symlink before the containment check ever runs.
    let base = parent.path().canonicalize().unwrap();
    let root_dir = base.join("root");
    let outside_dir = base.join("root-outside");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();
    let library_id = library(&cp, "contained-target").await;
    let root = cp
        .create_library_root(root_input(library_id, &root_dir))
        .await
        .unwrap();
    cp.activate_library_root(root.id, "contained-target".to_owned())
        .await
        .unwrap();
    set_defaults(&cp, root.id, None, Some(root.id)).await;

    let target = outside_dir.join("output.mkv");
    let error = resolve_output_target(&cp, "test artifact", root.id, &target)
        .await
        .unwrap_err();

    // Discriminate on "path escaped", not on "escaped storage root":
    // `rooted_target_address` rejects an escape a second time when
    // `strip_prefix` fails, with "target escaped ...", so the looser substring
    // would not notice `require_contained` being disabled. Match a substring
    // rather than the whole rendered string, so the message can later name the
    // rejected path without editing this test.
    assert_eq!(error.code(), "CONFIG_INVALID");
    assert!(
        error.to_string().contains("path escaped storage root"),
        "got: {error}"
    );

    // The rejection has to tell an operator which path was rejected and what it
    // was measured against; a storage root id alone leaves the constraint to be
    // reconstructed from source (issue #617). Assert the two paths as one
    // rendered phrase rather than two `contains` calls: `outside_dir` is a
    // textual-prefix sibling of `root_dir`, so `contains(root_dir)` alone is
    // satisfied by the rejected path and would still pass if the root were
    // dropped from the message.
    assert!(
        error.to_string().contains(&format!(
            "{} is not inside {}",
            target.display(),
            root_dir.display()
        )),
        "got: {error}"
    );
}

/// ADR 0097: a commit (pre-promotion) address is contained by the staging root,
/// not the output root. Staging and output are siblings here — the layout ADR
/// 0097 declares correct — so measuring against the output root, as the old
/// resolver did, would accept the output-side target and reject the staging one.
#[tokio::test]
async fn pre_promotion_target_is_contained_by_the_staging_root() {
    let (cp, _db) = crate::cases::cp().await;
    let (_parent, dirs) = sibling_dirs(&["source", "staging", "output"]);
    let library_id = library(&cp, "staging-containment").await;
    let source = active_root(&cp, library_id, &dirs[0], "source").await;
    let staging = active_root(&cp, library_id, &dirs[1], "staging").await;
    let output = active_root(&cp, library_id, &dirs[2], "output").await;
    set_defaults(&cp, source, Some(staging), Some(output)).await;

    let (root, locator, path) =
        resolve_pre_promotion_target(&cp, "test commit", source, &dirs[1].join("t1-out.mkv"))
            .await
            .unwrap();
    assert_eq!(root, staging);
    assert_eq!(locator.as_str(), "t1-out.mkv");
    assert_eq!(path, dirs[1].join("t1-out.mkv"));

    let error =
        resolve_pre_promotion_target(&cp, "test commit", source, &dirs[2].join("t1-out.mkv"))
            .await
            .unwrap_err();
    assert_eq!(error.code(), "CONFIG_INVALID");
    assert!(
        error
            .to_string()
            .contains(&format!("path escaped storage root {staging}")),
        "got: {error}"
    );
}

/// A source root with no staging default — and named as no root's staging
/// default — has no containment root for a commit: fail closed and name the
/// fix, rather than measuring against the source or output root.
#[tokio::test]
async fn pre_promotion_target_without_staging_default_fails_closed() {
    let (cp, _db) = crate::cases::cp().await;
    let (_parent, dirs) = sibling_dirs(&["source"]);
    let library_id = library(&cp, "no-staging-default").await;
    let source = active_root(&cp, library_id, &dirs[0], "source").await;
    set_defaults(&cp, source, None, Some(source)).await;

    let error =
        resolve_pre_promotion_target(&cp, "test commit", source, &dirs[0].join("t1-out.mkv"))
            .await
            .unwrap_err();

    assert_eq!(error.code(), "CONFIG_INVALID");
    let message = error.to_string();
    assert!(
        message.contains(&format!("storage root {source}")),
        "got: {message}"
    );
    assert!(
        message.contains(&format!(
            "voom library root update --root-id {source} --staging-root <id>"
        )),
        "got: {message}"
    );
}

/// A chained phase commits with the phase-1 artifact's root — the staging root
/// T — as its source. T resolves to itself because another root names it as
/// its staging default (`destination_root`'s leaf lookup), even though T has no
/// staging default of its own.
#[tokio::test]
async fn chained_phase_staging_root_resolves_to_itself() {
    let (cp, _db) = crate::cases::cp().await;
    let (_parent, dirs) = sibling_dirs(&["source", "staging"]);
    let library_id = library(&cp, "chained-staging").await;
    let source = active_root(&cp, library_id, &dirs[0], "source").await;
    let staging = active_root(&cp, library_id, &dirs[1], "staging").await;
    set_defaults(&cp, source, Some(staging), None).await;

    let (root, _, _) =
        resolve_pre_promotion_target(&cp, "test commit", staging, &dirs[1].join("t2-out.mkv"))
            .await
            .unwrap();

    assert_eq!(root, staging);
}

/// ADR 0097 removed the fallback to the passed root: with no output default,
/// a durable output target fails closed even when it lies inside that root,
/// and the error names the root and the command that configures one.
#[tokio::test]
async fn output_target_without_output_default_fails_closed() {
    let (cp, _db) = crate::cases::cp().await;
    let (_parent, dirs) = sibling_dirs(&["root"]);
    let library_id = library(&cp, "no-output-default").await;
    let root = active_root(&cp, library_id, &dirs[0], "root").await;

    let error = resolve_output_target(&cp, "test promotion", root, &dirs[0].join("out.mkv"))
        .await
        .unwrap_err();

    assert_eq!(error.code(), "CONFIG_INVALID");
    let message = error.to_string();
    assert!(
        message.contains(&format!("storage root {root} has no default output root")),
        "got: {message}"
    );
    assert!(
        message.contains(&format!(
            "voom library root update --root-id {root} --output-root <id>"
        )),
        "got: {message}"
    );
}

/// The staging route keeps the library check the output route has: a staging
/// default in another library is corrupt storage (configuration rejects it, so
/// the row is written directly here).
#[tokio::test]
async fn pre_promotion_target_rejects_cross_library_staging_root() {
    let (cp, _db) = crate::cases::cp().await;
    let (_parent, dirs) = sibling_dirs(&["source", "staging"]);
    let source_library = library(&cp, "staging-source-library").await;
    let staging_library = library(&cp, "staging-other-library").await;
    let source = active_root(&cp, source_library, &dirs[0], "source").await;
    let staging = active_root(&cp, staging_library, &dirs[1], "staging").await;
    sqlx::query("UPDATE library_roots SET default_staging_root_id = ? WHERE id = ?")
        .bind(i64::try_from(staging.0).unwrap())
        .bind(i64::try_from(source.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let error =
        resolve_pre_promotion_target(&cp, "test commit", source, &dirs[1].join("t1-out.mkv"))
            .await
            .unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }), "got: {error}");
    assert!(
        error.to_string().contains("default staging root"),
        "got: {error}"
    );
}
