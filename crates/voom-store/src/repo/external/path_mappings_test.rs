use serde_json::json;
use time::OffsetDateTime;
use voom_core::ExternalSystemId;

use super::*;
use crate::repo::external::SqliteExternalSystemRepo;
use crate::repo::external::systems::{ExternalSystemKind, NewExternalSystem};

async fn repo() -> (SqliteExternalSystemRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (SqliteExternalSystemRepo::new(pool), tmp)
}

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
}

async fn system(repo: &SqliteExternalSystemRepo) -> ExternalSystemId {
    let mut tx = repo.pool.begin().await.unwrap();
    let created = repo
        .register_in_tx(
            &mut tx,
            NewExternalSystem {
                kind: ExternalSystemKind::Filesystem,
                display_name: "fs".to_owned(),
                connection_profile: json!({}),
                auth_ref: "none".to_owned(),
                rate_limit_config: json!({}),
            },
            now(),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    created.id
}

fn mapping(system_id: ExternalSystemId) -> NewExternalPathMapping {
    NewExternalPathMapping {
        external_system_id: system_id,
        internal_prefix: "/srv/media".to_owned(),
        external_prefix: "/data".to_owned(),
        visibility: PathVisibility::ReadOnly,
    }
}

#[tokio::test]
async fn create_then_get_round_trips() {
    let (repo, _tmp) = repo().await;
    let sid = system(&repo).await;
    let created = repo.create_path_mapping(mapping(sid), now()).await.unwrap();
    assert!(created.id.0 > 0);
    let fetched = repo.get_path_mapping(created.id).await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(fetched.visibility, PathVisibility::ReadOnly);
}

#[tokio::test]
async fn create_with_unknown_system_is_not_found() {
    let (repo, _tmp) = repo().await;
    let err = repo
        .create_path_mapping(mapping(ExternalSystemId(999)), now())
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::NotFound(_)), "got: {err:?}");
}

#[tokio::test]
async fn list_returns_only_active_mappings_for_the_system() {
    let (repo, _tmp) = repo().await;
    let sid = system(&repo).await;
    let a = repo.create_path_mapping(mapping(sid), now()).await.unwrap();
    let b = repo.create_path_mapping(mapping(sid), now()).await.unwrap();
    assert!(repo.retire_path_mapping(b.id, now()).await.unwrap());

    let listed = repo.list_path_mappings(sid).await.unwrap();
    assert_eq!(listed.iter().map(|m| m.id).collect::<Vec<_>>(), vec![a.id]);
}

#[tokio::test]
async fn update_applies_partial_change() {
    let (repo, _tmp) = repo().await;
    let sid = system(&repo).await;
    let created = repo.create_path_mapping(mapping(sid), now()).await.unwrap();
    let updated = repo
        .update_path_mapping(
            created.id,
            PathMappingUpdate {
                external_prefix: Some("/mnt/data".to_owned()),
                visibility: Some(PathVisibility::ReadWrite),
                ..PathMappingUpdate::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.internal_prefix, "/srv/media");
    assert_eq!(updated.external_prefix, "/mnt/data");
    assert_eq!(updated.visibility, PathVisibility::ReadWrite);
}

#[tokio::test]
async fn update_missing_mapping_is_none() {
    let (repo, _tmp) = repo().await;
    let outcome = repo
        .update_path_mapping(
            voom_core::ExternalPathMappingId(999),
            PathMappingUpdate::default(),
        )
        .await
        .unwrap();
    assert!(outcome.is_none());
}

#[tokio::test]
async fn retire_is_idempotent_only_once() {
    let (repo, _tmp) = repo().await;
    let sid = system(&repo).await;
    let created = repo.create_path_mapping(mapping(sid), now()).await.unwrap();
    assert!(repo.retire_path_mapping(created.id, now()).await.unwrap());
    assert!(!repo.retire_path_mapping(created.id, now()).await.unwrap());
    // A retired mapping cannot be updated.
    assert!(
        repo.update_path_mapping(created.id, PathMappingUpdate::default())
            .await
            .unwrap()
            .is_none()
    );
}
