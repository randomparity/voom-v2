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
                kind: ExternalSystemKind::Plex,
                display_name: "plex".to_owned(),
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

fn link(system_id: ExternalSystemId, external_ref: &str) -> NewExternalLink {
    NewExternalLink {
        external_system_id: system_id,
        target_type: ExternalLinkTargetType::MediaWork,
        target_id: 7,
        external_ref: external_ref.to_owned(),
    }
}

#[tokio::test]
async fn record_then_list_returns_active_links() {
    let (repo, _tmp) = repo().await;
    let sid = system(&repo).await;
    let mut tx = repo.pool.begin().await.unwrap();
    let a = repo
        .record_link_in_tx(&mut tx, link(sid, "plex://1"), now())
        .await
        .unwrap();
    let b = repo
        .record_link_in_tx(&mut tx, link(sid, "plex://2"), now())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let listed = repo.list_links(sid).await.unwrap();
    assert_eq!(
        listed.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![a.id, b.id]
    );
    assert_eq!(listed[0].target_type, ExternalLinkTargetType::MediaWork);
    assert_eq!(listed[0].external_ref, "plex://1");
}

#[tokio::test]
async fn retire_removes_link_from_active_list_and_is_once() {
    let (repo, _tmp) = repo().await;
    let sid = system(&repo).await;
    let mut tx = repo.pool.begin().await.unwrap();
    let a = repo
        .record_link_in_tx(&mut tx, link(sid, "plex://1"), now())
        .await
        .unwrap();
    let retired = repo.retire_link_in_tx(&mut tx, a.id, now()).await.unwrap();
    assert!(retired.is_some());
    assert!(retired.unwrap().retired_at.is_some());
    // Second retire finds no active row.
    assert!(
        repo.retire_link_in_tx(&mut tx, a.id, now())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.list_links_in_tx(&mut tx, sid)
            .await
            .unwrap()
            .is_empty()
    );
    tx.commit().await.unwrap();
    assert!(repo.list_links(sid).await.unwrap().is_empty());
}

#[test]
fn target_type_rejects_out_of_vocab() {
    assert!(ExternalLinkTargetType::parse("nope").is_err());
    assert_eq!(ExternalLinkTargetType::FileAsset.as_str(), "file_asset");
}
