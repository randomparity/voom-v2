use super::*;

use voom_policy::{
    BundleTargetState, FixtureName, IssueInputState, TargetKind, TargetRef, load_fixture,
};

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
    let draft = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();

    let created = repo.create_input_set(draft.clone()).await.unwrap();

    assert_eq!(created.slug, draft.slug);
    assert_eq!(created.display_name, draft.display_name);
    assert_eq!(created.schema_version, draft.schema_version);
    assert_eq!(created.source_kind, draft.source_kind);
    assert_eq!(created.created_at, draft.created_at);
    assert_eq!(created.description, draft.description);
    assert_eq!(created.fixture_labels, draft.fixture_labels);

    assert_eq!(created.synthetic_targets.len(), 6);
    assert_eq!(created.synthetic_targets[0].synthetic_key, "asset-1");
    assert_eq!(
        created.synthetic_targets[0].target_kind,
        TargetKind::FileAsset
    );
    assert_eq!(
        created.synthetic_targets[0].display_name.as_deref(),
        Some("Synthetic Asset")
    );
    assert_eq!(created.synthetic_targets[1].synthetic_key, "bundle-1");
    assert_eq!(
        created.synthetic_targets[1].target_kind,
        TargetKind::AssetBundle
    );
    assert_eq!(created.synthetic_targets[2].synthetic_key, "location-1");
    assert_eq!(
        created.synthetic_targets[2].target_kind,
        TargetKind::FileLocation
    );
    assert_eq!(created.synthetic_targets[3].synthetic_key, "variant-1");
    assert_eq!(
        created.synthetic_targets[3].target_kind,
        TargetKind::MediaVariant
    );
    assert_eq!(created.synthetic_targets[4].synthetic_key, "version-1");
    assert_eq!(
        created.synthetic_targets[4].target_kind,
        TargetKind::FileVersion
    );
    assert_eq!(created.synthetic_targets[5].synthetic_key, "work-1");
    assert_eq!(
        created.synthetic_targets[5].target_kind,
        TargetKind::MediaWork
    );

    assert_eq!(created.media_snapshots.len(), 1);
    let snapshot = &created.media_snapshots[0];
    assert_eq!(
        snapshot_target_key(&snapshot.target),
        Some(("variant-1", TargetKind::MediaVariant))
    );
    assert_eq!(snapshot.ordinal, 0);
    assert_eq!(snapshot.container.as_deref(), Some("mp4"));
    assert_eq!(snapshot.video_codec.as_deref(), Some("h264"));
    assert_eq!(snapshot.width, Some(1920));
    assert_eq!(snapshot.height, Some(1080));
    assert_eq!(snapshot.hdr, None);
    assert_eq!(snapshot.bitrate, Some(8_000_000));
    assert_eq!(snapshot.duration_millis, Some(7_200_000));
    assert_eq!(snapshot.audio_languages, ["en"]);
    assert!(snapshot.subtitle_languages.is_empty());
    assert_eq!(snapshot.health_flags, ["missing_english_subtitle"]);
    assert_eq!(
        snapshot.stream_summary,
        draft.media_snapshots[0].stream_summary
    );
    assert_eq!(snapshot.existing_media_snapshot_id, None);

    assert_eq!(created.identity_evidence.len(), 1);
    let evidence = &created.identity_evidence[0];
    assert_eq!(
        snapshot_target_key(&evidence.target),
        Some(("work-1", TargetKind::MediaWork))
    );
    assert_eq!(evidence.ordinal, 0);
    assert_eq!(evidence.assertion_type, "identity_match");
    assert_eq!(evidence.provider, "synthetic-fixture");
    assert_eq!(evidence.provider_version, "1");
    assert!((evidence.confidence - draft.identity_evidence[0].confidence).abs() < f64::EPSILON);
    assert_eq!(evidence.provenance, draft.identity_evidence[0].provenance);
    assert_eq!(evidence.observed_at, draft.identity_evidence[0].observed_at);
    assert_eq!(evidence.existing_evidence_id, None);

    assert_eq!(created.bundle_targets.len(), 1);
    let bundle = &created.bundle_targets[0];
    assert_eq!(
        snapshot_target_key(&bundle.target),
        Some(("bundle-1", TargetKind::AssetBundle))
    );
    assert_eq!(bundle.ordinal, 0);
    assert_eq!(bundle.role, "external_subtitle");
    assert_eq!(bundle.desired_state, BundleTargetState::Required);
    assert_eq!(bundle.language.as_deref(), Some("en"));
    assert_eq!(bundle.label.as_deref(), Some("English subtitles"));
    assert_eq!(bundle.disposition.as_deref(), Some("external"));
    assert_eq!(
        bundle.artifact_expectation,
        draft.bundle_targets[0].artifact_expectation
    );

    assert_eq!(created.quality_profiles.len(), 1);
    let profile = &created.quality_profiles[0];
    assert_eq!(
        snapshot_target_key(&profile.target),
        Some(("variant-1", TargetKind::MediaVariant))
    );
    assert_eq!(profile.ordinal, 0);
    assert_eq!(profile.profile_name, "balanced-home");
    assert_eq!(profile.profile_version, "1");
    assert_eq!(
        profile.dimension_weights,
        draft.quality_profiles[0].dimension_weights
    );

    assert_eq!(created.issues.len(), 1);
    let issue = &created.issues[0];
    assert_eq!(
        snapshot_target_key(&issue.target),
        Some(("variant-1", TargetKind::MediaVariant))
    );
    assert_eq!(issue.ordinal, 0);
    assert_eq!(issue.kind, "policy_noncompliant");
    assert_eq!(issue.severity, voom_core::IssueSeverity::Medium);
    assert_eq!(issue.priority, voom_core::IssuePriority::Normal);
    assert_eq!(issue.state, IssueInputState::Open);
    assert_eq!(issue.reason, "English subtitle is required but missing.");
    assert_eq!(issue.provenance, draft.issues[0].provenance);
    assert_eq!(issue.existing_issue_id, None);
}

fn snapshot_target_key(target: &PolicyInputTargetRef) -> Option<(&str, TargetKind)> {
    match target {
        PolicyInputTargetRef::Synthetic { key, kind, .. } => Some((key.as_str(), *kind)),
        _ => None,
    }
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
