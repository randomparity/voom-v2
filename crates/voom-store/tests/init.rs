#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use voom_store::test_support::sqlite_url_for;
use voom_store::{SchemaState, connect, expected_migrations, init, probe_schema};
use voom_test_support::TempDatabase;

// Integration tests use the disk-backed public `init(url)` exclusively.
// The :memory: + init_on path is covered by Task 11's lib-internal unit tests.
// init_on is not re-exported from voom-store and is gated behind the test feature.

#[tokio::test]
async fn init_on_disk_creates_schema_meta() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());

    let report = init(&url).await.unwrap();
    assert!(!report.already_initialized);

    let pool = connect(&url).await.unwrap();
    let state = probe_schema(&pool).await.unwrap();
    let SchemaState::Current {
        migration_count, ..
    } = state
    else {
        panic!("expected Current, got {state:?}");
    };
    assert_eq!(migration_count, expected_migrations());
}

#[tokio::test]
async fn second_init_against_same_disk_db_is_noop() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());

    let first = init(&url).await.unwrap();
    assert!(!first.already_initialized);

    let pool = connect(&url).await.unwrap();
    let before = initialization_rows(&pool).await;
    assert_eq!(
        u32::try_from(before.migrations.len()).unwrap(),
        expected_migrations()
    );
    assert!(
        before
            .metadata
            .iter()
            .any(|(key, _)| key == "schema_init_at")
    );
    assert_eq!(before.events.len(), 1);

    let second = init(&url).await.unwrap();
    assert!(second.already_initialized);
    assert_eq!(second.migrations_applied, 0);
    assert_eq!(first.schema_init_at, second.schema_init_at);

    // Idempotence is a durable-state contract. Comparing complete rows catches
    // hidden rewrites without treating parallel disk activity as polling.
    let after = initialization_rows(&pool).await;
    assert_eq!(
        before, after,
        "second init must preserve durable initialization rows"
    );
    pool.close().await;
}

/// Regression: two concurrent `init()` calls against the same on-disk
/// database must both succeed. Without race-safe handling, the loser would
/// surface a "table already exists" / "version already applied" migration
/// error even though the schema is now Current.
///
/// The user-facing contract this pins:
/// - Both calls return `Ok` (no error masquerading as a missing migration).
/// - The final on-disk state is exactly one migration applied.
/// - Both processes observe the same `schema_init_at`, proving they read
///   the same migration row (only one was actually written).
/// - Exactly one peer (the write-lock winner) reports
///   `migrations_applied == expected_migrations()` and
///   `already_initialized == false`; the other (blocked on `BEGIN IMMEDIATE`,
///   then a no-op through `run_direct` once it acquires the lock) reports
///   `migrations_applied == 0` and `already_initialized == true` —
///   deterministically correct under the locked migration flow, not an
///   approximation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_init_on_same_disk_db_is_safe() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());

    // Pre-create the file so both spawned tasks race on migration application,
    // not on file creation.
    voom_store::test_support::create_uninitialized_pool(&url)
        .await
        .unwrap();

    let a_url = url.clone();
    let b_url = url.clone();
    let a = tokio::spawn(async move { init(&a_url).await });
    let b = tokio::spawn(async move { init(&b_url).await });

    let a = a.await.unwrap().unwrap_or_else(|e| {
        panic!("first concurrent init must succeed: {e}");
    });
    let b = b.await.unwrap().unwrap_or_else(|e| {
        panic!("second concurrent init must succeed (race-safe): {e}");
    });

    // Both processes must agree on the durable schema_init_at — only one
    // row was ever inserted.
    assert_eq!(
        a.schema_init_at, b.schema_init_at,
        "both inits must observe the same persisted schema_init_at row"
    );

    // Exactly one peer won the write lock and applied every migration; the
    // other blocked and then no-opped, applying none.
    let reports = [&a, &b];
    let winners = reports
        .iter()
        .filter(|r| r.migrations_applied == expected_migrations() && !r.already_initialized)
        .count();
    let losers = reports
        .iter()
        .filter(|r| r.migrations_applied == 0 && r.already_initialized)
        .count();
    assert_eq!(
        (winners, losers),
        (1, 1),
        "exactly one winner (applied={}, already_initialized=false) and one \
         loser (applied=0, already_initialized=true), got a={a:?} b={b:?}",
        expected_migrations()
    );

    // Final state: exactly one migration applied, Current.
    let pool = voom_store::connect(&url).await.unwrap();
    let state = voom_store::probe_schema(&pool).await.unwrap();
    match state {
        voom_store::SchemaState::Current {
            migration_count, ..
        } => {
            assert_eq!(
                migration_count,
                expected_migrations(),
                "every embedded migration must have produced exactly one row"
            );
        }
        other => panic!("post-race state must be Current, got {other:?}"),
    }
}

/// Stress version of `concurrent_init_on_same_disk_db_is_safe`. Runs the
/// concurrent-init scenario 20 iterations × 6 peers so any future TOCTOU
/// regression in `probe_schema`'s identity guard surfaces deterministically
/// rather than depending on CI runner timing. Each iteration uses a fresh
/// tempfile so iterations are independent.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_init_stress() {
    for iteration in 0..20 {
        let tmp = TempDatabase::new().unwrap();
        let url = sqlite_url_for(tmp.path());

        // Pre-create the file so peers race on migration application, not
        // on file creation. Mirrors the single-shot test's setup.
        voom_store::test_support::create_uninitialized_pool(&url)
            .await
            .unwrap();

        let handles: Vec<_> = (0..6)
            .map(|_| {
                let u = url.clone();
                tokio::spawn(async move { init(&u).await })
            })
            .collect();

        let mut reports = Vec::with_capacity(handles.len());
        for h in handles {
            let report = h.await.unwrap().unwrap_or_else(|e| {
                panic!("iteration {iteration}: concurrent init must succeed: {e}");
            });
            reports.push(report);
        }

        // All peers must agree on the durable schema_init_at — only one row
        // was ever inserted into schema_meta, so all peers must read the
        // same value back.
        let first = reports[0].schema_init_at;
        assert!(
            reports.iter().all(|r| r.schema_init_at == first),
            "iteration {iteration}: peers disagreed on schema_init_at: {reports:?}"
        );

        // Final state: exactly one migration row.
        let pool = voom_store::connect(&url).await.unwrap();
        let state = voom_store::probe_schema(&pool).await.unwrap();
        match state {
            voom_store::SchemaState::Current {
                migration_count, ..
            } => {
                assert_eq!(
                    migration_count,
                    expected_migrations(),
                    "iteration {iteration}: every embedded migration must have \
                     produced exactly one row"
                );
            }
            other => {
                panic!("iteration {iteration}: post-race state must be Current, got {other:?}")
            }
        }
    }
}

type InitializationEventRow = (
    i64,
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    String,
);

#[derive(Debug, PartialEq, Eq)]
struct InitializationRows {
    migrations: Vec<(i64, String, String, i64, Vec<u8>, i64)>,
    metadata: Vec<(String, String)>,
    events: Vec<InitializationEventRow>,
}

async fn initialization_rows(pool: &sqlx::SqlitePool) -> InitializationRows {
    let migrations = sqlx::query_as(
        "SELECT version, description, installed_on, success, checksum, execution_time \
         FROM _sqlx_migrations ORDER BY version",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let metadata = sqlx::query_as("SELECT key, value FROM schema_meta ORDER BY key")
        .fetch_all(pool)
        .await
        .unwrap();
    let events = sqlx::query_as(
        "SELECT event_id, occurred_at, kind, subject_type, subject_id, trace_id, payload \
         FROM events WHERE kind = ? ORDER BY event_id",
    )
    .bind(voom_events::EventKind::SchemaInitialized.as_str())
    .fetch_all(pool)
    .await
    .unwrap();
    InitializationRows {
        migrations,
        metadata,
        events,
    }
}
