use super::*;

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

fn draft(dedupe_key: &str, status: PolicyIssueStatus) -> PolicyIssueDraft {
    PolicyIssueDraft {
        dedupe_key: dedupe_key.to_owned(),
        status,
        title: "Policy compliance: container for synthetic:movie-a".to_owned(),
        body: "Policy version 2 requires {\"container\":\"mkv\"}; observed {\"container\":\"mp4\"}; status planned.".to_owned(),
        priority_reason: "policy compliance report report_1".to_owned(),
    }
}

#[tokio::test]
async fn issue_dedupe_key_column_is_nullable_and_unique_when_present() {
    let (pool, _tmp) = pool().await;

    let nullable: i64 = sqlx::query_scalar(
        "SELECT [notnull] FROM pragma_table_info('issues') WHERE name = 'dedupe_key'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nullable, 0, "dedupe_key must be nullable");

    sqlx::query(
        "INSERT INTO issues \
         (kind, severity, priority, priority_source, status, title, body, created_at, updated_at) \
         VALUES ('policy_noncompliant', 'medium', 'normal', 'policy', 'open', 'a', 'a', ?, ?)",
    )
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issues \
         (kind, severity, priority, priority_source, status, title, body, created_at, updated_at) \
         VALUES ('policy_noncompliant', 'medium', 'normal', 'policy', 'open', 'b', 'b', ?, ?)",
    )
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO issues \
         (kind, severity, priority, priority_source, status, title, body, created_at, updated_at, dedupe_key) \
         VALUES ('policy_noncompliant', 'medium', 'normal', 'policy', 'open', 'c', 'c', ?, ?, 'same')",
    )
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    let err = sqlx::query(
        "INSERT INTO issues \
         (kind, severity, priority, priority_source, status, title, body, created_at, updated_at, dedupe_key) \
         VALUES ('policy_noncompliant', 'medium', 'normal', 'policy', 'open', 'd', 'd', ?, ?, 'same')",
    )
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("UNIQUE"));
}

#[tokio::test]
async fn upsert_policy_issue_creates_then_updates_same_dedupe_key() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteIssueRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();

    let created = repo
        .upsert_policy_noncompliant_in_tx(
            &mut tx,
            draft(
                "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:check=a",
                PolicyIssueStatus::Planned,
            ),
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    assert_eq!(created.kind, PolicyIssueMutationKind::Created);
    assert_eq!(created.row.status, PolicyIssueStatus::Planned);

    let mut changed = draft(
        "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:check=a",
        PolicyIssueStatus::Open,
    );
    changed.title = "Policy compliance: blocked container for synthetic:movie-a".to_owned();
    let updated = repo
        .upsert_policy_noncompliant_in_tx(&mut tx, changed, time::OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    assert_eq!(updated.kind, PolicyIssueMutationKind::Updated);
    assert_eq!(updated.row.id, created.row.id);
    assert_eq!(updated.row.status, PolicyIssueStatus::Open);
    assert_eq!(updated.row.epoch, created.row.epoch + 1);

    tx.commit().await.unwrap();
}

#[tokio::test]
async fn resolve_matching_policy_issue_resolves_only_exact_dedupe_key() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteIssueRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let matching_key = "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:check=matching";
    let other_key = "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:check=other";
    let matching = repo
        .upsert_policy_noncompliant_in_tx(
            &mut tx,
            draft(matching_key, PolicyIssueStatus::Planned),
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    let other = repo
        .upsert_policy_noncompliant_in_tx(
            &mut tx,
            draft(other_key, PolicyIssueStatus::Planned),
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();

    let resolved = repo
        .resolve_policy_noncompliant_by_dedupe_key_in_tx(
            &mut tx,
            matching_key,
            "Policy compliance resolved: container for synthetic:movie-a",
            "Current report marks this check compliant.",
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.kind, PolicyIssueMutationKind::Resolved);
    assert_eq!(resolved.row.id, matching.row.id);
    assert_eq!(resolved.row.status, PolicyIssueStatus::Resolved);

    let live = repo
        .list_live_policy_noncompliant_by_dedupe_prefix_in_tx(
            &mut tx,
            "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:%",
        )
        .await
        .unwrap();
    assert_eq!(live, vec![other.row]);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn list_open_policy_issues_by_document_and_input_prefix_for_no_longer_emitted_resolution() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteIssueRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let prefix = "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:%";
    let matching = repo
        .upsert_policy_noncompliant_in_tx(
            &mut tx,
            draft(
                "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:check=a",
                PolicyIssueStatus::Planned,
            ),
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    let _different_input = repo
        .upsert_policy_noncompliant_in_tx(
            &mut tx,
            draft(
                "policy_noncompliant:v1:policy_document_id=1:input_set_id=3:check=b",
                PolicyIssueStatus::Planned,
            ),
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    let resolved_key = "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:check=resolved";
    repo.upsert_policy_noncompliant_in_tx(
        &mut tx,
        draft(resolved_key, PolicyIssueStatus::Planned),
        time::OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap();
    repo.resolve_policy_noncompliant_by_dedupe_key_in_tx(
        &mut tx,
        resolved_key,
        "resolved",
        "resolved",
        time::OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap();

    let live = repo
        .list_live_policy_noncompliant_by_dedupe_prefix_in_tx(&mut tx, prefix)
        .await
        .unwrap();
    assert_eq!(live, vec![matching.row]);
    tx.commit().await.unwrap();
}
