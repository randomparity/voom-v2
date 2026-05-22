use super::*;

use voom_policy::{FixtureName, TargetKind, TargetRef, load_fixture};

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let p = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (p, tmp)
}

fn compliant_fixture() -> voom_policy::PolicyInputSetDraft {
    load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap()
}

#[tokio::test]
async fn create_get_and_list_policy_input_set() {
    let (pool, _tmp) = pool().await;
    let repo = SqlitePolicyInputRepo::new(pool);
    let draft = compliant_fixture();

    let created = repo.create_input_set(draft.clone()).await.unwrap();
    let fetched = repo.get_input_set(created.id).await.unwrap().unwrap();
    let fetched_by_slug = repo
        .get_input_set_by_slug(&draft.slug)
        .await
        .unwrap()
        .unwrap();
    let listed = repo.list_input_sets().await.unwrap();

    assert_eq!(created, fetched);
    assert_eq!(created, fetched_by_slug);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);
    assert_eq!(listed[0].slug, draft.slug);
    assert_eq!(listed[0].fixture_labels, draft.fixture_labels);
}

#[tokio::test]
async fn duplicate_slug_is_rejected() {
    let (pool, _tmp) = pool().await;
    let repo = SqlitePolicyInputRepo::new(pool);
    let first = compliant_fixture();
    let mut duplicate = first.clone();
    duplicate.fixture_labels = vec!["duplicate_slug_label".to_owned()];

    repo.create_input_set(first).await.unwrap();
    let err = repo.create_input_set(duplicate).await.unwrap_err();

    assert_eq!(err.code(), "DB_UNREACHABLE");
}

#[tokio::test]
async fn fixture_labels_are_globally_unique() {
    let (pool, _tmp) = pool().await;
    let repo = SqlitePolicyInputRepo::new(pool);
    let first = compliant_fixture();
    let mut duplicate = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();
    duplicate.fixture_labels = first.fixture_labels.clone();

    repo.create_input_set(first).await.unwrap();
    let err = repo.create_input_set(duplicate).await.unwrap_err();

    assert_eq!(err.code(), "DB_UNREACHABLE");
}

#[tokio::test]
async fn create_rolls_back_when_child_insert_fails() {
    let (pool, _tmp) = pool().await;
    let repo = SqlitePolicyInputRepo::new(pool);
    let mut draft = compliant_fixture();
    draft.media_snapshots[0].target = TargetRef::MediaWork {
        id: voom_core::MediaWorkId(9_999),
    };

    let err = repo.create_input_set(draft.clone()).await.unwrap_err();
    let listed = repo.list_input_sets().await.unwrap();

    assert_eq!(err.code(), "DB_UNREACHABLE");
    assert!(listed.is_empty());
}

#[tokio::test]
async fn raw_sql_rejects_undeclared_synthetic_target() {
    let (pool, _tmp) = pool().await;
    let set_id = insert_raw_input_set(&pool, "raw-undeclared").await;

    let err = sqlx::query(
        "INSERT INTO policy_media_snapshot_inputs \
         (policy_input_set_id, ordinal, synthetic_target_id, stream_summary) \
         VALUES (?, 0, 404, '{}')",
    )
    .bind(set_id)
    .execute(&pool)
    .await
    .unwrap_err();

    assert!(err.to_string().contains("FOREIGN KEY"));
}

#[tokio::test]
async fn raw_sql_rejects_mixed_durable_and_synthetic_target_shape() {
    let (pool, _tmp) = pool().await;
    let set_id = insert_raw_input_set(&pool, "raw-mixed").await;
    let target_id = insert_raw_synthetic_target(&pool, set_id, "variant-1", "media_variant").await;

    let err = sqlx::query(
        "INSERT INTO policy_media_snapshot_inputs \
         (policy_input_set_id, ordinal, media_work_id, synthetic_target_id, stream_summary) \
         VALUES (?, 0, 1, ?, '{}')",
    )
    .bind(set_id)
    .bind(target_id)
    .execute(&pool)
    .await
    .unwrap_err();

    assert!(err.to_string().contains("CHECK"));
}

#[tokio::test]
async fn raw_sql_rejects_cross_input_set_synthetic_target() {
    let (pool, _tmp) = pool().await;
    let set_a = insert_raw_input_set(&pool, "raw-cross-a").await;
    let set_b = insert_raw_input_set(&pool, "raw-cross-b").await;
    let target_id = insert_raw_synthetic_target(&pool, set_a, "variant-1", "media_variant").await;

    let err = sqlx::query(
        "INSERT INTO policy_media_snapshot_inputs \
         (policy_input_set_id, ordinal, synthetic_target_id, stream_summary) \
         VALUES (?, 0, ?, '{}')",
    )
    .bind(set_b)
    .bind(target_id)
    .execute(&pool)
    .await
    .unwrap_err();

    assert!(err.to_string().contains("FOREIGN KEY"));
}

#[tokio::test]
async fn sqlite_round_trip_matches_fixture_projection() {
    let (pool, _tmp) = pool().await;
    let repo = SqlitePolicyInputRepo::new(pool);
    let draft = compliant_fixture();

    let created = repo.create_input_set(draft.clone()).await.unwrap();

    assert_eq!(created.slug, draft.slug);
    assert_eq!(created.display_name, draft.display_name);
    assert_eq!(created.schema_version, draft.schema_version);
    assert_eq!(created.source_kind, draft.source_kind);
    assert_eq!(created.created_at, draft.created_at);
    assert_eq!(created.description, draft.description);
    assert_eq!(created.fixture_labels, draft.fixture_labels);
    assert_eq!(created.media_snapshots.len(), draft.media_snapshots.len());
    assert_eq!(
        created.media_snapshots[0].ordinal,
        draft.media_snapshots[0].ordinal
    );
    assert_eq!(
        created.media_snapshots[0].container,
        draft.media_snapshots[0].container
    );
    assert_eq!(
        created.media_snapshots[0].stream_summary,
        draft.media_snapshots[0].stream_summary
    );
    assert_eq!(
        created.identity_evidence.len(),
        draft.identity_evidence.len()
    );
    assert_eq!(created.bundle_targets.len(), draft.bundle_targets.len());
    assert_eq!(created.quality_profiles.len(), draft.quality_profiles.len());
    assert_eq!(created.issues.len(), draft.issues.len());
    assert!(matches!(
        created.media_snapshots[0].target,
        PolicyInputTargetRef::Synthetic {
            kind: TargetKind::MediaVariant,
            ..
        }
    ));
}

async fn insert_raw_input_set(pool: &sqlx::SqlitePool, slug: &str) -> i64 {
    sqlx::query(
        "INSERT INTO policy_input_sets \
         (slug, display_name, schema_version, source_kind, created_at) \
         VALUES (?, 'raw', 1, 'test', '1970-01-01T00:00:00Z')",
    )
    .bind(slug)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
}

async fn insert_raw_synthetic_target(
    pool: &sqlx::SqlitePool,
    set_id: i64,
    key: &str,
    kind: &str,
) -> i64 {
    sqlx::query(
        "INSERT INTO policy_input_synthetic_targets \
         (policy_input_set_id, synthetic_key, target_kind) \
         VALUES (?, ?, ?)",
    )
    .bind(set_id)
    .bind(key)
    .bind(kind)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
}
