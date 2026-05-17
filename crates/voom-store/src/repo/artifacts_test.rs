use super::*;

use serde_json::json;
use time::OffsetDateTime;

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let p = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (p, tmp)
}

fn sample_new_handle() -> NewArtifactHandle {
    NewArtifactHandle {
        size_bytes: Some(1024),
        checksum: Some("abc".to_owned()),
        privacy_class: "internal".to_owned(),
        durability_class: "durable".to_owned(),
        allowed_access_modes: vec!["read".to_owned(), "write".to_owned()],
        mutability: "immutable".to_owned(),
        source_lineage: Some(json!({"src": "test"})),
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn artifact_handles_has_no_identity_link_columns() {
    let (pool, _tmp) = pool().await;
    let cols: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM pragma_table_info('artifact_handles') ORDER BY cid")
            .fetch_all(&pool)
            .await
            .unwrap();
    let names: Vec<&str> = cols.iter().map(|c| c.0.as_str()).collect();
    for forbidden in [
        "media_work_id",
        "media_variant_id",
        "asset_bundle_id",
        "file_asset_id",
        "file_version_id",
    ] {
        assert!(
            !names.contains(&forbidden),
            "M1 artifact_handles must NOT carry identity-layer columns; M2 adds them with FKs"
        );
    }
}

#[tokio::test]
async fn create_handle_returns_id() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let h = repo.create_handle(sample_new_handle()).await.unwrap();
    assert!(h.id.0 > 0);
}

#[tokio::test]
async fn record_location_attaches_to_handle() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let h = repo.create_handle(sample_new_handle()).await.unwrap();
    let loc = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: h.id,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    assert!(loc.id.0 > 0);
    let locs = repo.list_locations_for_handle(h.id).await.unwrap();
    assert_eq!(locs.len(), 1);
}

#[tokio::test]
async fn retire_location_sets_retired_at() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let h = repo.create_handle(sample_new_handle()).await.unwrap();
    let loc = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: h.id,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let when = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
    repo.retire_location(loc.id, when).await.unwrap();
    let live = repo.list_locations_for_handle(h.id).await.unwrap();
    assert_eq!(
        live.len(),
        0,
        "retired locations excluded from live listing"
    );
}

#[tokio::test]
async fn record_lineage_links_two_handles() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let parent = repo.create_handle(sample_new_handle()).await.unwrap();
    let child = repo.create_handle(sample_new_handle()).await.unwrap();
    let edge = repo
        .record_lineage(NewArtifactLineage {
            parent_artifact_id: parent.id,
            child_artifact_id: child.id,
            operation: "transcode".to_owned(),
            recorded_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    assert!(edge.id > 0);
}

#[tokio::test]
async fn record_lineage_rejects_self_edge() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let h = repo.create_handle(sample_new_handle()).await.unwrap();
    let err = repo
        .record_lineage(NewArtifactLineage {
            parent_artifact_id: h.id,
            child_artifact_id: h.id,
            operation: "noop".to_owned(),
            recorded_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap_err();
    // CHECK constraint rejects self-references; surfaces as Database.
    assert!(matches!(err, voom_core::VoomError::Database(_)));
}
