use time::OffsetDateTime;

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

fn new_library(slug: &str) -> NewLibrary {
    NewLibrary {
        slug: slug.to_owned(),
        display_name: format!("{slug} display"),
        media_kind: LibraryMediaKind::Movie,
        description: Some("desc".to_owned()),
        enabled: true,
    }
}

#[tokio::test]
async fn set_default_scoring_profile_sets_and_clears() {
    let (repo, _tmp) = repo().await;
    let lib = repo
        .create_library(new_library("home"), at(1))
        .await
        .unwrap();
    assert_eq!(lib.default_scoring_profile_name, None);

    let set = repo
        .set_default_scoring_profile(lib.id, Some("balanced-home"), at(2))
        .await
        .unwrap();
    assert_eq!(
        set.default_scoring_profile_name.as_deref(),
        Some("balanced-home")
    );
    assert_eq!(set.updated_at, at(2));

    let cleared = repo
        .set_default_scoring_profile(lib.id, None, at(3))
        .await
        .unwrap();
    assert_eq!(cleared.default_scoring_profile_name, None);
}

#[tokio::test]
async fn set_default_scoring_profile_missing_library_is_not_found() {
    let (repo, _tmp) = repo().await;
    let err = repo
        .set_default_scoring_profile(voom_core::LibraryId(999), Some("x"), at(1))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "NOT_FOUND");
}

#[tokio::test]
async fn create_then_get_round_trips_all_fields() {
    let (repo, _tmp) = repo().await;
    let created = repo
        .create_library(new_library("films"), at(10))
        .await
        .unwrap();
    let fetched = repo.get_library(created.id).await.unwrap().unwrap();
    assert_eq!(created, fetched);
    assert_eq!(fetched.slug, "films");
    assert_eq!(fetched.media_kind, LibraryMediaKind::Movie);
    assert!(fetched.enabled);
    assert_eq!(fetched.created_at, at(10));
    assert_eq!(fetched.updated_at, at(10));
}

#[tokio::test]
async fn duplicate_slug_is_conflict() {
    let (repo, _tmp) = repo().await;
    repo.create_library(new_library("films"), at(0))
        .await
        .unwrap();
    let err = repo
        .create_library(new_library("films"), at(1))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn get_by_slug_and_missing() {
    let (repo, _tmp) = repo().await;
    let created = repo
        .create_library(new_library("shows"), at(0))
        .await
        .unwrap();
    let by_slug = repo.get_library_by_slug("shows").await.unwrap().unwrap();
    assert_eq!(by_slug.id, created.id);
    assert!(repo.get_library_by_slug("nope").await.unwrap().is_none());
    assert!(repo.get_library(LibraryId(9999)).await.unwrap().is_none());
}

#[tokio::test]
async fn list_is_creation_ordered() {
    let (repo, _tmp) = repo().await;
    let a = repo.create_library(new_library("a"), at(1)).await.unwrap();
    let b = repo.create_library(new_library("b"), at(2)).await.unwrap();
    let ids: Vec<_> = repo
        .list_libraries()
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.id)
        .collect();
    assert_eq!(ids, vec![a.id, b.id]);
}

#[tokio::test]
async fn update_mutates_and_bumps_updated_at() {
    let (repo, _tmp) = repo().await;
    let created = repo.create_library(new_library("a"), at(1)).await.unwrap();
    let updated = repo
        .update_library(
            created.id,
            LibraryUpdate {
                display_name: Some("renamed".to_owned()),
                media_kind: Some(LibraryMediaKind::Episode),
                description: None,
            },
            at(5),
        )
        .await
        .unwrap();
    assert_eq!(updated.display_name, "renamed");
    assert_eq!(updated.media_kind, LibraryMediaKind::Episode);
    assert_eq!(updated.description.as_deref(), Some("desc")); // None left it unchanged
    assert_eq!(updated.created_at, at(1));
    assert_eq!(updated.updated_at, at(5));
}

#[tokio::test]
async fn update_missing_is_not_found() {
    let (repo, _tmp) = repo().await;
    let err = repo
        .update_library(LibraryId(1), LibraryUpdate::default(), at(0))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::NotFound(_)), "got {err:?}");
}

#[tokio::test]
async fn set_enabled_toggles() {
    let (repo, _tmp) = repo().await;
    let created = repo.create_library(new_library("a"), at(1)).await.unwrap();
    let disabled = repo
        .set_library_enabled(created.id, false, at(3))
        .await
        .unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.updated_at, at(3));
}

#[tokio::test]
async fn delete_returns_whether_removed() {
    let (repo, _tmp) = repo().await;
    let created = repo.create_library(new_library("a"), at(1)).await.unwrap();
    assert!(repo.delete_library(created.id).await.unwrap());
    assert!(!repo.delete_library(created.id).await.unwrap());
    assert!(repo.get_library(created.id).await.unwrap().is_none());
}
