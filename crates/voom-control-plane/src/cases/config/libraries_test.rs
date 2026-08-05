use voom_core::{LibraryId, NodeId, NodeStatus, ProviderLocator, StorageProviderKind};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};

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

async fn node(cp: &crate::ControlPlane, name: &str, status: NodeStatus) -> NodeId {
    let id = sqlx::query(
        "INSERT INTO nodes \
         (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (?, 'local', ?, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                 60, 'hash', 'hint', '{}')",
    )
    .bind(name)
    .bind(status.as_str())
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    NodeId(u64::try_from(id).unwrap())
}

#[tokio::test]
async fn library_and_root_crud_round_trip() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    let owner = node(&cp, "node-a", NodeStatus::Registered).await;
    assert!(
        cp.list_libraries()
            .await
            .unwrap()
            .iter()
            .any(|listed| listed.id == lib.id)
    );

    let root = cp
        .create_library_root(new_root(lib.id, owner, "/media/films"))
        .await
        .unwrap();
    let fetched = cp.get_library_root(root.id).await.unwrap().unwrap();
    assert_eq!(fetched.provider_locator.as_str(), "/media/films");
    assert_eq!(cp.list_library_roots(Some(lib.id)).await.unwrap().len(), 1);
}

#[tokio::test]
async fn owner_scoped_provider_locator_identity_is_enforced() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    let owner_a = node(&cp, "node-a", NodeStatus::Registered).await;
    let owner_b = node(&cp, "node-b", NodeStatus::Registered).await;
    cp.create_library_root(new_root(lib.id, owner_a, "/media/films"))
        .await
        .unwrap();

    let duplicate = cp
        .create_library_root(new_root(lib.id, owner_a, "/media/films"))
        .await
        .unwrap_err();
    assert_eq!(duplicate.code(), "CONFLICT");

    cp.create_library_root(new_root(lib.id, owner_b, "/media/films"))
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

#[tokio::test]
async fn set_default_scoring_profile_validates_existence_and_retire() {
    use voom_store::repo::policy::quality_scoring_profiles::NewQualityScoringProfile;

    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();

    // Unknown profile is refused.
    let err = cp
        .set_library_default_scoring_profile(lib.id, Some("ghost"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "NOT_FOUND");

    cp.create_scoring_profile(NewQualityScoringProfile {
        name: "balanced-home".to_owned(),
        version: 1,
        definition: serde_json::json!({ "weights": {} }),
    })
    .await
    .unwrap();

    let linked = cp
        .set_library_default_scoring_profile(lib.id, Some("balanced-home"))
        .await
        .unwrap();
    assert_eq!(
        linked.default_scoring_profile_name.as_deref(),
        Some("balanced-home")
    );

    // A retired profile cannot become a library default.
    cp.retire_scoring_profile("balanced-home").await.unwrap();
    let err = cp
        .set_library_default_scoring_profile(lib.id, Some("balanced-home"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "NOT_FOUND");

    // Clearing is always allowed.
    let cleared = cp
        .set_library_default_scoring_profile(lib.id, None)
        .await
        .unwrap();
    assert_eq!(cleared.default_scoring_profile_name, None);
}
