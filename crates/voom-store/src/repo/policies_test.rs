use super::*;

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

async fn repo() -> (SqlitePolicyRepo, tempfile::NamedTempFile) {
    let (pool, tmp) = pool().await;
    (SqlitePolicyRepo::new(pool), tmp)
}

fn draft(slug: &str, source_text: &str) -> NewPolicyDocumentVersion {
    NewPolicyDocumentVersion {
        slug: slug.to_owned(),
        display_name: None,
        source_text: source_text.to_owned(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn create_document_with_first_version_round_trips() {
    let (repo, _tmp) = repo().await;
    let draft = draft(
        "production-normalize",
        "policy \"production-normalize\" { phase a {} }",
    );

    let created = repo.create_document_with_version(draft).await.unwrap();
    let fetched = repo
        .get_document(created.document.id)
        .await
        .unwrap()
        .unwrap();
    let versions = repo.list_versions(created.document.id).await.unwrap();

    assert_eq!(created.document.slug, "production-normalize");
    assert_eq!(created.version.version_number, 1);
    assert_eq!(
        created.document.current_accepted_version_id,
        Some(created.version.id)
    );
    assert_eq!(fetched, created.document);
    assert_eq!(versions, [created.version]);
}

#[tokio::test]
async fn list_documents_orders_by_slug() {
    let (repo, _tmp) = repo().await;
    let b = repo
        .create_document_with_version(draft("b-policy", "policy \"b-policy\" { phase a {} }"))
        .await
        .unwrap();
    let a = repo
        .create_document_with_version(draft("a-policy", "policy \"a-policy\" { phase a {} }"))
        .await
        .unwrap();

    let listed = repo.list_documents().await.unwrap();

    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, a.document.id);
    assert_eq!(listed[0].slug, "a-policy");
    assert_eq!(listed[1].id, b.document.id);
    assert_eq!(listed[1].slug, "b-policy");
}

#[tokio::test]
async fn duplicate_source_returns_existing_version() {
    let (repo, _tmp) = repo().await;
    let draft = draft("same", "policy \"same\" { phase a {} }");
    let first = repo
        .create_document_with_version(draft.clone())
        .await
        .unwrap();

    let second = repo
        .add_version(first.document.id, draft.source_text)
        .await
        .unwrap();

    assert_eq!(second.id, first.version.id);
    assert_eq!(second.version_number, 1);
}

#[tokio::test]
async fn add_version_advances_current_version_and_epoch() {
    let (repo, _tmp) = repo().await;
    let created = repo
        .create_document_with_version(draft("advance", "policy \"advance\" { phase a {} }"))
        .await
        .unwrap();

    let added = repo
        .add_version(
            created.document.id,
            "policy \"advance\" { phase a {} phase b { depends_on: [a] } }".to_owned(),
        )
        .await
        .unwrap();
    let document = repo
        .get_document(created.document.id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(added.version_number, 2);
    assert_eq!(document.current_accepted_version_id, Some(added.id));
    assert_eq!(document.epoch, created.document.epoch + 1);
}

#[tokio::test]
async fn cross_document_current_version_is_rejected() {
    let (pool, _tmp) = pool().await;
    let repo = SqlitePolicyRepo::new(pool.clone());
    let a = repo
        .create_document_with_version(draft("a", "policy \"a\" { phase a {} }"))
        .await
        .unwrap();
    let b = repo
        .create_document_with_version(draft("b", "policy \"b\" { phase b {} }"))
        .await
        .unwrap();

    let err =
        sqlx::query("UPDATE policy_documents SET current_accepted_version_id = ? WHERE id = ?")
            .bind(i64::try_from(a.version.id.0).unwrap())
            .bind(i64::try_from(b.document.id.0).unwrap())
            .execute(&pool)
            .await
            .unwrap_err();

    assert!(
        err.to_string()
            .contains("policy current version must belong to document")
    );
}

#[tokio::test]
async fn raw_sql_rejects_unstable_policy_document_slug() {
    let (pool, _tmp) = pool().await;

    let err = sqlx::query(
        "INSERT INTO policy_documents (slug, display_name, created_at) VALUES (?, ?, ?)",
    )
    .bind("Bad Slug")
    .bind("bad")
    .bind("1970-01-01T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap_err();

    assert!(err.to_string().contains("CHECK"));
}

#[tokio::test]
async fn concurrent_add_version_has_one_winner() {
    let (pool, _tmp) = pool().await;
    let repo_a = SqlitePolicyRepo::new(pool.clone());
    let repo_b = SqlitePolicyRepo::new(pool);
    let created = repo_a
        .create_document_with_version(draft("race", "policy \"race\" { phase a {} }"))
        .await
        .unwrap();

    let source = "policy \"race\" { phase a {} phase b { depends_on: [a] } }";
    let (left, right) = tokio::join!(
        repo_a.add_version(created.document.id, source.to_owned()),
        repo_b.add_version(created.document.id, source.to_owned())
    );

    assert!(
        left.is_ok() || right.is_ok(),
        "at least one concurrent writer should create or observe version 2"
    );
    let versions = repo_a.list_versions(created.document.id).await.unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|version| version.version_number)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    let version2 = versions.last().unwrap();
    for result in [&left, &right] {
        match result {
            Ok(returned) => assert_eq!(returned.id, version2.id),
            Err(err) => assert_eq!(err.code(), "CONFLICT"),
        }
    }
}
