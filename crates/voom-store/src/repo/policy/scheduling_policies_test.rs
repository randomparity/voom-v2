use time::OffsetDateTime;
use voom_core::VoomError;

use super::*;

async fn repo() -> (SqliteSchedulingPolicyRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (SqliteSchedulingPolicyRepo::new(pool), tmp)
}

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
}

fn later() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_900).unwrap()
}

fn sample(slug: &str) -> NewSchedulingPolicy {
    NewSchedulingPolicy {
        slug: slug.to_owned(),
        display_name: "Home library default".to_owned(),
        priority: SchedulePriority::NewestFirst,
        copy_window: Some("00:00-08:00".to_owned()),
        large_jobs_night_only: true,
        pause_on_degraded_node: false,
    }
}

#[tokio::test]
async fn create_then_get_round_trips_every_field() {
    let (repo, _tmp) = repo().await;
    let created = repo.create(sample("home"), now()).await.unwrap();
    assert!(created.id > 0);
    assert_eq!(created.schema_version, SCHEDULING_POLICY_SCHEMA_VERSION);

    let fetched = repo.get_by_slug("home").await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(fetched.priority, SchedulePriority::NewestFirst);
    assert_eq!(fetched.copy_window.as_deref(), Some("00:00-08:00"));
    assert!(fetched.large_jobs_night_only);
    assert!(!fetched.pause_on_degraded_node);
    assert_eq!(fetched.created_at, now());
    assert_eq!(fetched.updated_at, now());
}

#[tokio::test]
async fn get_unknown_slug_is_none() {
    let (repo, _tmp) = repo().await;
    assert!(repo.get_by_slug("missing").await.unwrap().is_none());
}

#[tokio::test]
async fn duplicate_slug_is_conflict() {
    let (repo, _tmp) = repo().await;
    repo.create(sample("home"), now()).await.unwrap();
    let err = repo.create(sample("home"), now()).await.unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn list_orders_by_slug() {
    let (repo, _tmp) = repo().await;
    repo.create(sample("zeta"), now()).await.unwrap();
    repo.create(sample("alpha"), now()).await.unwrap();
    let slugs: Vec<String> = repo
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.slug)
        .collect();
    assert_eq!(slugs, vec!["alpha".to_owned(), "zeta".to_owned()]);
}

#[tokio::test]
async fn update_replaces_all_mutable_fields_and_bumps_updated_at() {
    let (repo, _tmp) = repo().await;
    let created = repo.create(sample("home"), now()).await.unwrap();

    let replacement = NewSchedulingPolicy {
        slug: "home".to_owned(),
        display_name: "Renamed".to_owned(),
        priority: SchedulePriority::LargestFirst,
        copy_window: None,
        large_jobs_night_only: false,
        pause_on_degraded_node: true,
    };
    let updated = repo.update(replacement, later()).await.unwrap().unwrap();

    assert_eq!(updated.id, created.id, "id preserved");
    assert_eq!(updated.created_at, now(), "created_at preserved");
    assert_eq!(updated.updated_at, later(), "updated_at bumped");
    assert_eq!(updated.display_name, "Renamed");
    assert_eq!(updated.priority, SchedulePriority::LargestFirst);
    assert_eq!(updated.copy_window, None);
    assert!(!updated.large_jobs_night_only);
    assert!(updated.pause_on_degraded_node);
}

#[tokio::test]
async fn update_unknown_slug_is_none() {
    let (repo, _tmp) = repo().await;
    let out = repo.update(sample("missing"), now()).await.unwrap();
    assert!(out.is_none());
}

#[tokio::test]
async fn delete_reports_whether_a_row_was_removed() {
    let (repo, _tmp) = repo().await;
    repo.create(sample("home"), now()).await.unwrap();
    assert!(repo.delete("home").await.unwrap());
    assert!(repo.get_by_slug("home").await.unwrap().is_none());
    assert!(!repo.delete("home").await.unwrap(), "second delete is false");
}

#[tokio::test]
async fn invalid_copy_window_is_rejected_on_create_and_update() {
    let (repo, _tmp) = repo().await;
    let mut bad = sample("home");
    bad.copy_window = Some("8am-4pm".to_owned());
    let err = repo.create(bad, now()).await.unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");

    repo.create(sample("home"), now()).await.unwrap();
    let mut bad_update = sample("home");
    bad_update.copy_window = Some("24:00-08:00".to_owned());
    let err = repo.update(bad_update, later()).await.unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
}

#[tokio::test]
async fn every_priority_variant_round_trips() {
    let (repo, _tmp) = repo().await;
    for (i, priority) in SchedulePriority::ALL.iter().copied().enumerate() {
        let mut input = sample(&format!("p{i}"));
        input.priority = priority;
        repo.create(input, now()).await.unwrap();
        let fetched = repo.get_by_slug(&format!("p{i}")).await.unwrap().unwrap();
        assert_eq!(fetched.priority, priority);
    }
}
