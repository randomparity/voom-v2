use super::*;

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

async fn repo() -> (SqlitePolicyRepo, voom_test_support::TempDatabase) {
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

#[derive(Clone, Copy)]
struct PolicyVersionOracle<'a> {
    slug: &'a str,
    source: &'a str,
    source_hash: &'a str,
    compiled_json: &'a str,
}

const POLICY_VERSION_ORACLES: [PolicyVersionOracle<'static>; 2] = [
    PolicyVersionOracle {
        slug: "escaped-title-filters",
        source: include_str!(
            "../../../../voom-policy/fixtures/historical/escaped-title-filters.voom"
        ),
        source_hash: "c4b99a0bae2445fac2f4c2662909d55b91fbb1fe63847dac5fa770541a5e69ba",
        compiled_json: include_str!(
            "../../../../voom-policy/fixtures/compiled/historical-track-filter-source/escaped-title-filters.json"
        ),
    },
    PolicyVersionOracle {
        slug: "escaped-title-filter-boundaries",
        source: include_str!(
            "../../../../voom-policy/fixtures/historical/escaped-title-filter-boundaries.voom"
        ),
        source_hash: "03fcaff76ec94729cbb436aebd2711cf52f0329c9aa4be168357c2a59a841e32",
        compiled_json: include_str!(
            "../../../../voom-policy/fixtures/compiled/historical-track-filter-source/escaped-title-filter-boundaries.json"
        ),
    },
];

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
async fn create_document_rejects_source_for_different_policy_slug() {
    let (repo, _tmp) = repo().await;

    let err = repo
        .create_document_with_version(draft(
            "stable-policy",
            "policy \"different-policy\" { phase a {} }",
        ))
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFIG_INVALID");
    assert!(repo.list_documents().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_document_with_duplicate_slug_is_conflict() {
    let (repo, _tmp) = repo().await;
    repo.create_document_with_version(draft(
        "production-normalize",
        "policy \"production-normalize\" { phase a {} }",
    ))
    .await
    .unwrap();

    let err = repo
        .create_document_with_version(draft(
            "production-normalize",
            "policy \"production-normalize\" { phase a {} }",
        ))
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFLICT");
    assert_eq!(repo.list_documents().await.unwrap().len(), 1);
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
        .add_version(
            first.document.id,
            draft.source_text,
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();

    assert_eq!(second.id, first.version.id);
    assert_eq!(second.version_number, 1);
}

#[tokio::test]
async fn escaped_title_upgrade_returns_historical_versions_without_mutation() {
    for oracle in POLICY_VERSION_ORACLES {
        let (pool, _tmp) = pool().await;
        let repo = SqlitePolicyRepo::new(pool.clone());
        let expected_json = oracle_json(oracle);
        let (document_id, version_id) = seed_stored_version(&pool, oracle, &expected_json).await;
        let document_before = repo.get_document(document_id).await.unwrap().unwrap();
        let versions_before = repo.list_versions(document_id).await.unwrap();
        let json_before = stored_compiled_json_text(&pool, version_id).await;

        let returned = repo
            .add_version(
                document_id,
                oracle.source.to_owned(),
                time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
            )
            .await
            .unwrap();

        assert_eq!(returned.id, version_id, "oracle {}", oracle.slug);
        assert_eq!(
            repo.get_document(document_id).await.unwrap().unwrap(),
            document_before,
            "oracle {}",
            oracle.slug
        );
        assert_eq!(
            repo.list_versions(document_id).await.unwrap(),
            versions_before,
            "oracle {}",
            oracle.slug
        );
        assert_eq!(
            stored_compiled_json_text(&pool, version_id).await,
            json_before,
            "oracle {}",
            oracle.slug
        );
        assert_eq!(
            versions_before.as_slice(),
            std::slice::from_ref(&returned),
            "oracle {}",
            oracle.slug
        );
        assert_eq!(versions_before.len(), 1, "oracle {}", oracle.slug);
        assert_version_matches_oracle(&returned, oracle, &expected_json);
    }
}

#[tokio::test]
async fn escaped_title_current_versions_remain_rollback_readable() {
    for oracle in POLICY_VERSION_ORACLES {
        let (pool, _tmp) = pool().await;
        let repo = SqlitePolicyRepo::new(pool.clone());
        let expected_json = oracle_json(oracle);

        let created = repo
            .create_document_with_version(draft(oracle.slug, oracle.source))
            .await
            .unwrap();

        assert_version_matches_oracle(&created.version, oracle, &expected_json);
        assert_eq!(created.document.epoch, 1, "oracle {}", oracle.slug);
        assert_eq!(
            created.document.current_accepted_version_id,
            Some(created.version.id),
            "oracle {}",
            oracle.slug
        );
        assert_eq!(
            stored_compiled_json_text(&pool, created.version.id).await,
            serde_json::to_string(&expected_json).unwrap(),
            "oracle {}",
            oracle.slug
        );
    }
}

#[tokio::test]
async fn duplicate_lookup_precedes_current_source_compilation() {
    let (pool, _tmp) = pool().await;
    let repo = SqlitePolicyRepo::new(pool.clone());
    let source = r#"policy "ordering-sentinel" {
  phase a {
    keep subtitle where title contains "terminal\"
  }
}
"#;
    assert!(voom_policy::compile_policy(source).is_err());
    let source_hash = voom_policy::source_hash(source);
    let compiled_json = serde_json::json!({
        "policy_name": "ordering-sentinel",
        "slug": "ordering-sentinel",
        "source_hash": source_hash,
        "schema_version": 2,
        "metadata": {},
        "config": {},
        "phases": [],
        "phase_order": [],
        "warnings": [],
        "provenance": {
            "compiler": "voom-policy",
            "format": "sprint4-v2",
            "flags": {}
        }
    });
    let sentinel = PolicyVersionOracle {
        slug: "ordering-sentinel",
        source,
        source_hash: compiled_json["source_hash"].as_str().unwrap(),
        compiled_json: "",
    };
    let (document_id, version_id) = seed_stored_version(&pool, sentinel, &compiled_json).await;

    let returned = repo
        .add_version(
            document_id,
            source.to_owned(),
            time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        )
        .await
        .unwrap();

    assert_eq!(returned.id, version_id);
    assert_eq!(repo.list_versions(document_id).await.unwrap(), [returned]);
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
            time::OffsetDateTime::UNIX_EPOCH,
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
async fn add_version_rejects_source_for_different_policy_slug() {
    let (repo, _tmp) = repo().await;
    let created = repo
        .create_document_with_version(draft(
            "same-policy",
            "policy \"same-policy\" { phase a {} }",
        ))
        .await
        .unwrap();

    let err = repo
        .add_version(
            created.document.id,
            "policy \"other-policy\" { phase a {} }".to_owned(),
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFIG_INVALID");
    assert_eq!(
        repo.list_versions(created.document.id).await.unwrap(),
        [created.version]
    );
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
async fn raw_sql_rejects_non_hex_policy_version_hash() {
    let (pool, _tmp) = pool().await;
    let document_id = sqlx::query(
        "INSERT INTO policy_documents (slug, display_name, created_at) VALUES (?, ?, ?)",
    )
    .bind("bad-hash")
    .bind("bad hash")
    .bind("1970-01-01T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();

    let err = sqlx::query(
        "INSERT INTO policy_versions \
         (policy_document_id, version_number, source_text, source_hash, schema_version, \
          compiled_json, created_at) VALUES (?, 1, ?, ?, 2, '{}', ?)",
    )
    .bind(document_id)
    .bind("policy \"bad-hash\" { phase a {} }")
    .bind("g".repeat(64))
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
        repo_a.add_version(
            created.document.id,
            source.to_owned(),
            time::OffsetDateTime::UNIX_EPOCH
        ),
        repo_b.add_version(
            created.document.id,
            source.to_owned(),
            time::OffsetDateTime::UNIX_EPOCH
        )
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

fn oracle_json(oracle: PolicyVersionOracle<'_>) -> serde_json::Value {
    let json: serde_json::Value = serde_json::from_str(oracle.compiled_json).unwrap();
    assert_eq!(voom_policy::source_hash(oracle.source), oracle.source_hash);
    assert_eq!(json["slug"], oracle.slug);
    assert_eq!(json["source_hash"], oracle.source_hash);
    assert_eq!(json["schema_version"], 2);
    json
}

async fn seed_stored_version(
    pool: &SqlitePool,
    oracle: PolicyVersionOracle<'_>,
    compiled_json: &serde_json::Value,
) -> (PolicyDocumentId, PolicyVersionId) {
    assert_eq!(compiled_json["slug"], oracle.slug);
    assert_eq!(compiled_json["source_hash"], oracle.source_hash);
    let document_id = sqlx::query(
        "INSERT INTO policy_documents (slug, display_name, created_at) VALUES (?, ?, ?)",
    )
    .bind(oracle.slug)
    .bind(oracle.slug)
    .bind("1970-01-01T00:00:00Z")
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let version_id = sqlx::query(
        "INSERT INTO policy_versions \
         (policy_document_id, version_number, source_text, source_hash, schema_version, \
          compiled_json, created_at) VALUES (?, 1, ?, ?, 2, ?, ?)",
    )
    .bind(document_id)
    .bind(oracle.source)
    .bind(oracle.source_hash)
    .bind(serde_json::to_string(compiled_json).unwrap())
    .bind("1970-01-01T00:00:00Z")
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "UPDATE policy_documents \
         SET current_accepted_version_id = ?, epoch = 1 WHERE id = ?",
    )
    .bind(version_id)
    .bind(document_id)
    .execute(pool)
    .await
    .unwrap();

    (
        PolicyDocumentId(u64::try_from(document_id).unwrap()),
        PolicyVersionId(u64::try_from(version_id).unwrap()),
    )
}

async fn stored_compiled_json_text(pool: &SqlitePool, version_id: PolicyVersionId) -> String {
    sqlx::query_scalar("SELECT compiled_json FROM policy_versions WHERE id = ?")
        .bind(i64::try_from(version_id.0).unwrap())
        .fetch_one(pool)
        .await
        .unwrap()
}

fn assert_version_matches_oracle(
    version: &PolicyVersion,
    oracle: PolicyVersionOracle<'_>,
    expected_json: &serde_json::Value,
) {
    assert_eq!(version.version_number, 1, "oracle {}", oracle.slug);
    assert_eq!(version.source_text, oracle.source, "oracle {}", oracle.slug);
    assert_eq!(
        version.source_hash, oracle.source_hash,
        "oracle {}",
        oracle.slug
    );
    assert_eq!(version.schema_version, 2, "oracle {}", oracle.slug);
    assert_eq!(
        version.compiled_json, *expected_json,
        "oracle {}",
        oracle.slug
    );
}
