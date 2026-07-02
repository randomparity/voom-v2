use time::OffsetDateTime;

use super::super::libraries::{LibraryMediaKind, NewLibrary};
use super::*;

async fn repo() -> (SqliteLibraryRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (SqliteLibraryRepo::new(pool), tmp)
}

fn at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap()
}

async fn library(repo: &SqliteLibraryRepo, slug: &str) -> LibraryId {
    repo.create_library(
        NewLibrary {
            slug: slug.to_owned(),
            display_name: slug.to_owned(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        },
        at(0),
    )
    .await
    .unwrap()
    .id
}

fn new_root(library_id: LibraryId, path: &str) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        root_kind: LibraryRootKind::LocalPath,
        canonical_path: path.to_owned(),
        display_path: path.to_owned(),
        include_globs: vec!["**/*.mkv".to_owned()],
        exclude_globs: vec!["**/sample/**".to_owned()],
        extension_allowlist: vec!["mkv".to_owned(), "mp4".to_owned()],
        scan_mode: LibraryScanMode::ManualRecursive,
        symlink_policy: SymlinkPolicy::Reject,
        hidden_file_policy: HiddenFilePolicy::Ignore,
        max_depth: Some(4),
        stability_seconds: 30,
        debounce_seconds: 5,
        default_output_root: Some("/out".to_owned()),
        default_staging_root: None,
        default_backup_root: None,
        enabled: true,
    }
}

#[tokio::test]
async fn create_then_get_round_trips_including_json_lists() {
    let (repo, _tmp) = repo().await;
    let lib = library(&repo, "films").await;
    let created = repo
        .create_library_root(new_root(lib, "/media/films"), at(1))
        .await
        .unwrap();
    let fetched = repo.get_library_root(created.id).await.unwrap().unwrap();
    assert_eq!(created, fetched);
    assert_eq!(fetched.include_globs, vec!["**/*.mkv".to_owned()]);
    assert_eq!(
        fetched.extension_allowlist,
        vec!["mkv".to_owned(), "mp4".to_owned()]
    );
    assert_eq!(fetched.max_depth, Some(4));
    assert_eq!(fetched.stability_seconds, 30);
    assert_eq!(fetched.default_output_root.as_deref(), Some("/out"));
}

#[tokio::test]
async fn create_under_missing_library_is_not_found() {
    let (repo, _tmp) = repo().await;
    let err = repo
        .create_library_root(new_root(LibraryId(999), "/media/x"), at(1))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::NotFound(_)), "got {err:?}");
}

#[tokio::test]
async fn duplicate_canonical_path_is_conflict() {
    let (repo, _tmp) = repo().await;
    let lib = library(&repo, "films").await;
    repo.create_library_root(new_root(lib, "/media/films"), at(1))
        .await
        .unwrap();
    let err = repo
        .create_library_root(new_root(lib, "/media/films"), at(2))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn list_filters_by_library() {
    let (repo, _tmp) = repo().await;
    let lib_a = library(&repo, "a").await;
    let lib_b = library(&repo, "b").await;
    let ra = repo
        .create_library_root(new_root(lib_a, "/a"), at(1))
        .await
        .unwrap();
    repo.create_library_root(new_root(lib_b, "/b"), at(2))
        .await
        .unwrap();
    let a_roots = repo.list_library_roots(Some(lib_a)).await.unwrap();
    assert_eq!(a_roots.len(), 1);
    assert_eq!(a_roots[0].id, ra.id);
    assert_eq!(repo.list_library_roots(None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn update_mutates_lists_and_settings_and_bumps_updated_at() {
    let (repo, _tmp) = repo().await;
    let lib = library(&repo, "films").await;
    let created = repo
        .create_library_root(new_root(lib, "/media/films"), at(1))
        .await
        .unwrap();
    let updated = repo
        .update_library_root(
            created.id,
            LibraryRootUpdate {
                extension_allowlist: Some(vec!["mkv".to_owned()]),
                scan_mode: Some(LibraryScanMode::WatchEnabled),
                stability_seconds: Some(60),
                ..LibraryRootUpdate::default()
            },
            at(9),
        )
        .await
        .unwrap();
    assert_eq!(updated.extension_allowlist, vec!["mkv".to_owned()]);
    assert_eq!(updated.scan_mode, LibraryScanMode::WatchEnabled);
    assert_eq!(updated.stability_seconds, 60);
    assert_eq!(updated.debounce_seconds, 5); // unchanged
    assert_eq!(updated.canonical_path, "/media/films"); // immutable
    assert_eq!(updated.updated_at, at(9));
}

#[tokio::test]
async fn update_missing_is_not_found() {
    let (repo, _tmp) = repo().await;
    let err = repo
        .update_library_root(LibraryRootId(1), LibraryRootUpdate::default(), at(0))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::NotFound(_)), "got {err:?}");
}

#[tokio::test]
async fn set_enabled_toggles() {
    let (repo, _tmp) = repo().await;
    let lib = library(&repo, "films").await;
    let created = repo
        .create_library_root(new_root(lib, "/media/films"), at(1))
        .await
        .unwrap();
    let disabled = repo
        .set_library_root_enabled(created.id, false, at(4))
        .await
        .unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.updated_at, at(4));
}

#[tokio::test]
async fn deleting_library_cascades_its_roots() {
    let (repo, _tmp) = repo().await;
    let lib = library(&repo, "films").await;
    let root = repo
        .create_library_root(new_root(lib, "/media/films"), at(1))
        .await
        .unwrap();
    assert!(repo.delete_library(lib).await.unwrap());
    assert!(repo.get_library_root(root.id).await.unwrap().is_none());
}
