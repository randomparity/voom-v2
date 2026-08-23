//! Migration 0042 preflight-guard tests: the guard must fail the whole
//! migration transaction while any non-terminal byte-touching media workflow
//! ticket carries a payload without the nested `media_dispatch` envelope, and
//! must stay silent otherwise. Pattern follows the migration-0037 guard tests
//! in `init_test.rs`.

use std::borrow::Cow;

use sqlx::migrate::{Migration, MigrationType, Migrator};
use voom_test_support::TempDatabase;

/// Apply every embedded migration except the last (physical versions 1–4),
/// leaving the database one migration behind so a test can seed pre-0042
/// rows and then run the real upgrade path.
async fn apply_through_0041(pool: &sqlx::SqlitePool) {
    let embedded = crate::test_support::embedded_migrator();
    let prefix = Migrator {
        migrations: Cow::Owned(
            embedded.migrations[..embedded.migrations.len() - 1]
                .iter()
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
    apply_through_0041(&pool).await;
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
    assert_eq!(report.migrations_applied, 1);

    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(applied, vec![1, 2, 3, 4, 5]);
}
