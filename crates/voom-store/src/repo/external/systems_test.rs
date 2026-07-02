use serde_json::json;
use time::OffsetDateTime;

use super::*;
use crate::repo::external::SqliteExternalSystemRepo;

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

fn sample() -> NewExternalSystem {
    NewExternalSystem {
        kind: ExternalSystemKind::Filesystem,
        display_name: "local media".to_owned(),
        connection_profile: json!({ "root": "/srv/media" }),
        auth_ref: "keyring://voom/local".to_owned(),
        rate_limit_config: json!({}),
    }
}

#[tokio::test]
async fn register_then_get_round_trips_every_field() {
    let (repo, _tmp) = repo().await;
    let mut tx = repo.pool.begin().await.unwrap();
    let created = repo.register_in_tx(&mut tx, sample(), now()).await.unwrap();
    tx.commit().await.unwrap();
    assert!(created.id.0 > 0);
    assert_eq!(created.health_status, ExternalSystemHealth::Unknown);
    assert_eq!(created.epoch, 0);
    assert!(created.retired_at.is_none());

    let fetched = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(fetched.kind, ExternalSystemKind::Filesystem);
    assert_eq!(fetched.connection_profile, json!({ "root": "/srv/media" }));
    assert_eq!(fetched.created_at, now());
}

#[tokio::test]
async fn get_missing_system_is_none() {
    let (repo, _tmp) = repo().await;
    assert!(
        repo.get(voom_core::ExternalSystemId(999))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn list_returns_active_systems_in_id_order() {
    let (repo, _tmp) = repo().await;
    let mut tx = repo.pool.begin().await.unwrap();
    let a = repo.register_in_tx(&mut tx, sample(), now()).await.unwrap();
    let mut second = sample();
    second.display_name = "remote".to_owned();
    let b = repo.register_in_tx(&mut tx, second, now()).await.unwrap();
    tx.commit().await.unwrap();

    let listed = repo.list().await.unwrap();
    assert_eq!(
        listed.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![a.id, b.id]
    );
}

#[tokio::test]
async fn set_health_updates_status_and_returns_row() {
    let (repo, _tmp) = repo().await;
    let mut tx = repo.pool.begin().await.unwrap();
    let created = repo.register_in_tx(&mut tx, sample(), now()).await.unwrap();
    let updated = repo
        .set_health_in_tx(&mut tx, created.id, ExternalSystemHealth::Healthy)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(updated.health_status, ExternalSystemHealth::Healthy);
    let fetched = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(fetched.health_status, ExternalSystemHealth::Healthy);
}

#[tokio::test]
async fn set_health_on_missing_system_is_none() {
    let (repo, _tmp) = repo().await;
    let mut tx = repo.pool.begin().await.unwrap();
    let outcome = repo
        .set_health_in_tx(
            &mut tx,
            voom_core::ExternalSystemId(999),
            ExternalSystemHealth::Degraded,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(outcome.is_none());
}

#[test]
fn kind_and_health_reject_out_of_vocab_tokens() {
    assert!(ExternalSystemKind::parse("nope").is_err());
    assert!(ExternalSystemHealth::parse("nope").is_err());
    assert_eq!(ExternalSystemKind::Plex.as_str(), "plex");
    assert_eq!(ExternalSystemHealth::Unreachable.as_str(), "unreachable");
}
