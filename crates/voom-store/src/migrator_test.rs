//! Migration upgrade-path tests. Migration 0042's preflight guard must fail
//! the whole migration transaction while any non-terminal byte-touching media
//! workflow ticket carries a payload without the nested `media_dispatch`
//! envelope, and must stay silent otherwise. Migration 0044 must backfill a
//! placement for every populated commit record (ADR 0103). Pattern follows the
//! migration-0037 guard tests in `init_test.rs`.

use std::borrow::Cow;

use sqlx::migrate::{Migration, MigrationType, Migrator};
use voom_test_support::TempDatabase;

/// Apply the embedded migrations through physical version `last`, so a test
/// can seed rows in the older shape and then run the real upgrade path.
async fn apply_through_version(pool: &sqlx::SqlitePool, last: i64) {
    let embedded = crate::test_support::embedded_migrator();
    let prefix = Migrator {
        migrations: Cow::Owned(
            embedded
                .migrations
                .iter()
                .filter(|m| m.version <= last)
                .map(|m| {
                    Migration::new(
                        m.version,
                        Cow::Borrowed(m.description.as_ref()),
                        MigrationType::Simple,
                        Cow::Borrowed(m.sql.as_ref()),
                        false,
                    )
                })
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    let mut conn = pool.acquire().await.unwrap();
    prefix.run(&mut *conn).await.unwrap();
}

async fn pool_one_behind() -> (sqlx::SqlitePool, TempDatabase) {
    let tmp = TempDatabase::new().unwrap();
    let url = crate::test_support::sqlite_url_for(tmp.path());
    let pool = crate::test_support::create_uninitialized_pool(&url)
        .await
        .unwrap();
    apply_through_version(&pool, 4).await;
    (pool, tmp)
}

const T0: &str = "1970-01-01T00:00:00Z";

/// Seed one workflow ticket with the given state and rendered payload.
async fn seed_ticket(pool: &sqlx::SqlitePool, id: i64, state: &str, rendered: &str) {
    let payload = format!(
        "{{\"workflow_id\":\"wf\",\"plan_id\":\"p\",\"node_id\":\"n\",\
         \"operation\":\"transcode_video\",\"rendered_payload\":{rendered}}}"
    );
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, next_eligible_at, \
          created_at, state_changed_at) \
         VALUES (?, NULL, 'transcode_video', ?, 0, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(state)
    .bind(payload)
    .bind(T0)
    .bind(T0)
    .bind(T0)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_0042_guard_rejects_unrenderable_media_tickets_before_any_mutation() {
    let (pool, _tmp) = pool_one_behind().await;
    // Pre-0042 renderer output: path-shaped fields, no media_dispatch envelope.
    seed_ticket(
        &pool,
        1,
        "leased",
        "{\"operation\":\"transcode_video\",\"source_storage_root_id\":7,\
          \"source_location_id\":9,\"source_file_version_id\":9000001,\
          \"staging_root\":\"/tmp/stage\"}",
    )
    .await;

    let err = crate::init::init_on(&pool).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("_0042_no_unrenderable_media_workflow_tickets"),
        "guard must name itself: {err}"
    );

    // Nothing was mutated: migration 0042 is not recorded and the ticket row
    // is untouched.
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied, 4);
    let state: String = sqlx::query_scalar("SELECT state FROM tickets WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "leased");
}

#[tokio::test]
async fn migration_0042_applies_once_every_media_ticket_is_drainable() {
    let (pool, _tmp) = pool_one_behind().await;
    // Terminal byte-touching tickets never block.
    seed_ticket(&pool, 1, "succeeded", "{\"operation\":\"transcode_video\"}").await;
    seed_ticket(&pool, 2, "failed", "{\"operation\":\"remux\"}").await;
    // A pending byte-touching ticket already carrying the envelope passes.
    seed_ticket(
        &pool,
        3,
        "pending",
        "{\"operation\":\"transcode_video\",\
          \"media_dispatch\":{\"operation\":\"transcode_video\",\"schema\":3}}",
    )
    .await;
    // Non-byte-touching operations are out of scope.
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, next_eligible_at, \
          created_at, state_changed_at) \
         VALUES (4, NULL, 'scan_library', 'ready', 0, \
                 '{\"workflow_id\":\"wf\",\"plan_id\":\"p\",\"node_id\":\"n\",\
                    \"operation\":\"scan_library\",\"rendered_payload\":\
                    {\"operation\":\"scan_library\",\"source_storage_root_id\":7}}', \
                 ?, ?, ?)",
    )
    .bind(T0)
    .bind(T0)
    .bind(T0)
    .execute(&pool)
    .await
    .unwrap();

    let report = crate::init::init_on(&pool).await.unwrap();
    assert_eq!(report.migrations_applied, 3);

    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(applied, vec![1, 2, 3, 4, 5, 6, 7]);
}

/// Seed one commit record with its own handle, staging location, and
/// verification; `result_locator` adds a result version and location.
async fn seed_commit_record(
    pool: &sqlx::SqlitePool,
    id: i64,
    state: &str,
    target_path: &str,
    result_locator: Option<&str>,
) {
    for statement in [
        "INSERT INTO artifact_handles (id, privacy_class, durability_class, \
         allowed_access_modes, mutability, file_version_id, created_at) \
         VALUES (?1, 'internal', 'staging', '[]', 'immutable', 1, '1970-01-01T00:00:00Z')",
        "INSERT INTO artifact_locations (id, artifact_handle_id, kind, value, observed_at) \
         VALUES (?1, ?1, 'staging', '/staging/' || ?1, '1970-01-01T00:00:00Z')",
        "INSERT INTO artifact_verifications (id, artifact_handle_id, artifact_location_id, \
         path, worker_id, status, expected_size_bytes, expected_checksum, \
         observed_size_bytes, observed_checksum, report, started_at, finished_at) \
         VALUES (?1, ?1, ?1, '/staging/' || ?1, 1, 'succeeded', 10, 'sum', 10, 'sum', '{}', \
         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    ] {
        sqlx::query(statement).bind(id).execute(pool).await.unwrap();
    }
    let result_id = result_locator.map(|_| id + 100);
    if let Some(locator) = result_locator {
        sqlx::query(
            "INSERT INTO file_versions (id, file_asset_id, content_hash, size_bytes, \
             produced_by, produced_from_version_id, created_at) \
             VALUES (?1, 1, 'result-' || ?1, 10, 'staged_commit', 1, '1970-01-01T00:00:00Z')",
        )
        .bind(result_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO file_locations (id, file_version_id, address_state, storage_root_id, \
             provider_relative_locator, observed_at) \
             VALUES (?1, ?1, 'rooted', 9000001, ?2, '1970-01-01T00:00:00Z')",
        )
        .bind(result_id)
        .bind(locator)
        .execute(pool)
        .await
        .unwrap();
    }
    let (failure, finished) = match state {
        "failed" => (Some("commit_failure"), Some(T0)),
        "committed" => (None, Some(T0)),
        _ => (None, None),
    };
    sqlx::query(
        "INSERT INTO artifact_commit_records (id, artifact_handle_id, source_file_version_id, \
         verification_id, target_path, result_file_version_id, result_file_location_id, state, \
         failure_class, error_code, message, report, started_at, finished_at) \
         VALUES (?1, ?1, 1, ?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, '{}', '1970-01-01T00:00:00Z', ?8)",
    )
    .bind(id)
    .bind(target_path)
    .bind(result_id)
    .bind(state)
    .bind(failure)
    .bind(failure.map(|_| "COMMIT_FAILURE"))
    .bind(failure.map(|_| "seeded failure"))
    .bind(finished)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_0044_backfills_placement_for_populated_commit_records() {
    let tmp = TempDatabase::new().unwrap();
    let url = crate::test_support::sqlite_url_for(tmp.path());
    let pool = crate::test_support::create_uninitialized_pool(&url)
        .await
        .unwrap();
    apply_through_version(&pool, 6).await;
    crate::test_support::seed_test_storage_root(&pool)
        .await
        .unwrap();
    for statement in [
        "INSERT INTO workers (id, name, kind, status, registered_at, last_seen_at) VALUES \
         (1, 'worker', 'synthetic', 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        "INSERT INTO file_assets (id, created_at) VALUES (1, '1970-01-01T00:00:00Z')",
        "INSERT INTO file_versions (id, file_asset_id, content_hash, size_bytes, produced_by, \
         created_at) VALUES (1, 1, 'source', 10, 'ingest', '1970-01-01T00:00:00Z')",
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    // A workflow commit still in flight keeps its intent and no state.
    seed_commit_record(
        &pool,
        1,
        "pending",
        "/root/.committed/transcode/a.mkv",
        None,
    )
    .await;
    // A manual commit outside the working dirs was deliberately left in place.
    seed_commit_record(&pool, 2, "committed", "/root/b.mkv", Some("b.mkv")).await;
    // A workflow result still in its working dir awaits the move.
    seed_commit_record(
        &pool,
        3,
        "committed",
        "/root/.committed/transcode/c.mkv",
        Some(".committed/transcode/c.mkv"),
    )
    .await;
    // A workflow result whose location promotion already repointed was moved.
    seed_commit_record(
        &pool,
        4,
        "committed",
        "/root/.committed/remux/d.mkv",
        Some("out/d.mkv"),
    )
    .await;
    // A failed commit records its intent but never a state.
    seed_commit_record(&pool, 5, "failed", "/root/.committed/audio/e.mka", None).await;

    let report = crate::init::init_on(&pool).await.unwrap();
    assert_eq!(report.migrations_applied, 1);

    let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT id, placement_intent, placement_state FROM artifact_commit_records ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let expected = [
        (1, "staged", None),
        (2, "retained", Some("retained")),
        (3, "staged", Some("staged")),
        (4, "staged", Some("placed")),
        (5, "staged", None),
    ]
    .map(|(id, intent, state)| (id, intent.to_owned(), state.map(str::to_owned)));
    assert_eq!(rows, expected);

    let index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' \
         AND name = 'artifact_commit_records_by_result_location'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(index, 1);
}
