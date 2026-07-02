use serde_json::json;

use super::*;
use crate::test_support::fresh_initialized_pool_at;

async fn repo() -> (
    SqliteQualityScoringProfileRepo,
    sqlx::SqlitePool,
    tempfile::NamedTempFile,
) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (
        SqliteQualityScoringProfileRepo::new(pool.clone()),
        pool,
        tmp,
    )
}

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
}

fn sample(name: &str) -> NewQualityScoringProfile {
    NewQualityScoringProfile {
        name: name.to_owned(),
        version: 1,
        definition: json!({ "weights": { "resolution": 3, "codec": 2 } }),
    }
}

#[tokio::test]
async fn create_persists_and_round_trips_definition() {
    let (repo, _pool, _tmp) = repo().await;
    let created = repo.create(sample("balanced-home"), now()).await.unwrap();
    assert_eq!(created.version, 1);
    let fetched = repo.get_by_name("balanced-home").await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(fetched.definition["weights"]["resolution"], json!(3));
}

#[tokio::test]
async fn create_rejects_non_object_definition() {
    let (repo, _pool, _tmp) = repo().await;
    let bad = NewQualityScoringProfile {
        name: "scalar".to_owned(),
        version: 1,
        definition: json!("not-an-object"),
    };
    let err = repo.create(bad, now()).await.unwrap_err();
    assert_eq!(err.code(), "CONFIG_INVALID");
}

#[tokio::test]
async fn create_rejects_duplicate_name_as_conflict() {
    let (repo, _pool, _tmp) = repo().await;
    repo.create(sample("dup"), now()).await.unwrap();
    let err = repo.create(sample("dup"), now()).await.unwrap_err();
    assert_eq!(err.code(), "CONFLICT");
}

#[tokio::test]
async fn update_replaces_version_and_definition_missing_is_none() {
    let (repo, _pool, _tmp) = repo().await;
    repo.create(sample("editme"), now()).await.unwrap();
    let changed = NewQualityScoringProfile {
        name: "editme".to_owned(),
        version: 2,
        definition: json!({ "weights": { "hdr": 5 } }),
    };
    let updated = repo.update(changed).await.unwrap().unwrap();
    assert_eq!(updated.version, 2);
    assert_eq!(updated.definition["weights"]["hdr"], json!(5));
    assert!(repo.update(sample("ghost")).await.unwrap().is_none());
}

#[tokio::test]
async fn retire_hides_from_list_keeps_resolvable_and_is_idempotent() {
    let (repo, _pool, _tmp) = repo().await;
    repo.create(sample("gone"), now()).await.unwrap();
    let retired = repo.retire("gone", now()).await.unwrap().unwrap();
    assert!(retired.retired_at.is_some());
    assert!(repo.list().await.unwrap().is_empty());
    assert!(repo.get_by_name("gone").await.unwrap().is_some());

    let later = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    let again = repo.retire("gone", later).await.unwrap().unwrap();
    assert_eq!(again.retired_at, retired.retired_at);
    assert!(repo.retire("never", now()).await.unwrap().is_none());
}

#[tokio::test]
async fn list_orders_by_name_and_excludes_retired() {
    let (repo, _pool, _tmp) = repo().await;
    repo.create(sample("zeta"), now()).await.unwrap();
    repo.create(sample("alpha"), now()).await.unwrap();
    repo.create(sample("mid"), now()).await.unwrap();
    repo.retire("mid", now()).await.unwrap();
    let names: Vec<String> = repo
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, vec!["alpha".to_owned(), "zeta".to_owned()]);
}
