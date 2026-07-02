use time::OffsetDateTime;
use voom_core::{OperationKind, VoomError};

use super::*;

async fn repo() -> (
    SqliteSafetyPolicyRepo,
    sqlx::SqlitePool,
    tempfile::NamedTempFile,
) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (SqliteSafetyPolicyRepo::new(pool.clone()), pool, tmp)
}

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
}

fn later() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_900).unwrap()
}

fn sample(slug: &str) -> NewSafetyPolicy {
    NewSafetyPolicy {
        slug: slug.to_owned(),
        display_name: "Conservative default".to_owned(),
        auto_execute_operations: vec![OperationKind::Remux, OperationKind::TranscodeVideo],
        backup_required: true,
        approval_required: false,
        allowed_commit_modes: vec![CommitMode::AddOnly],
        verification_level: VerificationLevel::QuickDecode,
        block_on_failed_records: true,
        block_on_recovery_required_records: true,
    }
}

#[tokio::test]
async fn create_then_get_round_trips_every_field() {
    let (repo, _pool, _tmp) = repo().await;
    let created = repo.create(sample("safe"), now()).await.unwrap();
    assert!(created.id > 0);
    assert_eq!(created.schema_version, SAFETY_POLICY_SCHEMA_VERSION);
    assert!(created.is_current_schema());

    let fetched = repo.get_by_slug("safe").await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(
        fetched.auto_execute_operations,
        vec![OperationKind::Remux, OperationKind::TranscodeVideo]
    );
    assert_eq!(fetched.allowed_commit_modes, vec![CommitMode::AddOnly]);
    assert_eq!(fetched.verification_level, VerificationLevel::QuickDecode);
    assert!(fetched.backup_required);
    assert!(!fetched.approval_required);
    assert!(fetched.block_on_failed_records);
    assert!(fetched.block_on_recovery_required_records);
    assert_eq!(fetched.created_at, now());
}

#[tokio::test]
async fn empty_array_columns_round_trip() {
    let (repo, _pool, _tmp) = repo().await;
    let mut input = sample("locked");
    input.auto_execute_operations = vec![];
    input.allowed_commit_modes = vec![];
    repo.create(input, now()).await.unwrap();
    let fetched = repo.get_by_slug("locked").await.unwrap().unwrap();
    assert!(fetched.auto_execute_operations.is_empty());
    assert!(fetched.allowed_commit_modes.is_empty());
    assert!(!fetched.allows_auto_execute(OperationKind::Remux));
    assert!(!fetched.allows_commit_mode(CommitMode::AddOnly));
}

#[tokio::test]
async fn duplicate_slug_is_conflict() {
    let (repo, _pool, _tmp) = repo().await;
    repo.create(sample("safe"), now()).await.unwrap();
    let err = repo.create(sample("safe"), now()).await.unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn list_orders_by_slug() {
    let (repo, _pool, _tmp) = repo().await;
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
    let (repo, _pool, _tmp) = repo().await;
    let created = repo.create(sample("safe"), now()).await.unwrap();

    let replacement = NewSafetyPolicy {
        slug: "safe".to_owned(),
        display_name: "Permissive".to_owned(),
        auto_execute_operations: vec![OperationKind::ExtractAudio],
        backup_required: false,
        approval_required: true,
        allowed_commit_modes: vec![CommitMode::AddOnly, CommitMode::Replace],
        verification_level: VerificationLevel::Full,
        block_on_failed_records: false,
        block_on_recovery_required_records: false,
    };
    let updated = repo.update(replacement, later()).await.unwrap().unwrap();

    assert_eq!(updated.id, created.id, "id preserved");
    assert_eq!(updated.created_at, now(), "created_at preserved");
    assert_eq!(updated.updated_at, later(), "updated_at bumped");
    assert_eq!(updated.display_name, "Permissive");
    assert_eq!(
        updated.auto_execute_operations,
        vec![OperationKind::ExtractAudio]
    );
    assert_eq!(
        updated.allowed_commit_modes,
        vec![CommitMode::AddOnly, CommitMode::Replace]
    );
    assert_eq!(updated.verification_level, VerificationLevel::Full);
    assert!(!updated.backup_required);
    assert!(updated.approval_required);
    assert!(!updated.block_on_failed_records);
    assert!(!updated.block_on_recovery_required_records);
}

#[tokio::test]
async fn update_unknown_slug_is_none() {
    let (repo, _pool, _tmp) = repo().await;
    assert!(
        repo.update(sample("missing"), now())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn delete_reports_whether_a_row_was_removed() {
    let (repo, _pool, _tmp) = repo().await;
    repo.create(sample("safe"), now()).await.unwrap();
    assert!(repo.delete("safe").await.unwrap());
    assert!(repo.get_by_slug("safe").await.unwrap().is_none());
    assert!(!repo.delete("safe").await.unwrap());
}

#[tokio::test]
async fn bad_verification_level_rejected_at_db_boundary() {
    // The CHECK constraint is the last line of defense; the typed enum makes an
    // invalid level unrepresentable through the repo, so we assert the DB itself
    // rejects a hand-written bad value.
    let (_repo, pool, _tmp) = repo().await;
    let res = sqlx::query(
        "INSERT INTO safety_policies \
         (slug, display_name, schema_version, auto_execute_operations, backup_required, \
          approval_required, allowed_commit_modes, verification_level, block_on_failed_records, \
          block_on_recovery_required_records, created_at, updated_at) \
         VALUES ('x', 'x', 1, '[]', 0, 0, '[]', 'bogus', 0, 0, ?, ?)",
    )
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .execute(&pool)
    .await;
    assert!(
        res.is_err(),
        "DB CHECK must reject an invalid verification_level"
    );
}

#[tokio::test]
async fn unknown_operation_token_in_row_fails_loud_on_read() {
    // A row whose JSON array carries an out-of-vocabulary operation must fail
    // the decode rather than silently drop the token.
    let (repo, pool, _tmp) = repo().await;
    sqlx::query(
        "INSERT INTO safety_policies \
         (slug, display_name, schema_version, auto_execute_operations, backup_required, \
          approval_required, allowed_commit_modes, verification_level, block_on_failed_records, \
          block_on_recovery_required_records, created_at, updated_at) \
         VALUES ('x', 'x', 1, '[\"not_an_op\"]', 0, 0, '[]', 'none', 0, 0, ?, ?)",
    )
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    let err = repo.get_by_slug("x").await.unwrap_err();
    assert!(matches!(err, VoomError::Database { .. }), "got {err:?}");
}

#[tokio::test]
async fn stale_schema_version_row_reports_not_current() {
    let (repo, pool, _tmp) = repo().await;
    let stale = i64::from(SAFETY_POLICY_SCHEMA_VERSION) + 1;
    sqlx::query(
        "INSERT INTO safety_policies \
         (slug, display_name, schema_version, auto_execute_operations, backup_required, \
          approval_required, allowed_commit_modes, verification_level, block_on_failed_records, \
          block_on_recovery_required_records, created_at, updated_at) \
         VALUES ('future', 'x', ?, '[]', 0, 0, '[]', 'none', 0, 0, ?, ?)",
    )
    .bind(stale)
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    let fetched = repo.get_by_slug("future").await.unwrap().unwrap();
    assert!(!fetched.is_current_schema());
}
