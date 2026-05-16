#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use tempfile::NamedTempFile;
use voom_store::{SchemaState, connect, init, probe_schema};

// Integration tests use the disk-backed public `init(url)` exclusively.
// The :memory: + init_on path is covered by Task 11's lib-internal unit tests.
// init_on is not re-exported from voom-store and is gated behind test-support.

#[tokio::test]
async fn init_on_disk_creates_schema_meta() {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());

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
    assert_eq!(migration_count, 1);
}

#[tokio::test]
async fn second_init_against_same_disk_db_is_noop() {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());

    let first = init(&url).await.unwrap();
    let second = init(&url).await.unwrap();

    assert!(!first.already_initialized);
    assert!(second.already_initialized);
    assert_eq!(second.migrations_applied, 0);
    assert_eq!(first.schema_init_at, second.schema_init_at);
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
///
/// Note: under race the individual `migrations_applied` count is each
/// process's local-snapshot delta — both may report `applied=1` because
/// each saw the schema go from "Uninitialized" (their probe-before) to
/// "Current" (their probe-after), regardless of which one actually
/// inserted the row. That's an accepted approximation; the durable
/// invariant is "exactly one row is in the DB."
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_init_on_same_disk_db_is_safe() {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());

    // Pre-create the file so both spawned tasks race on migration application,
    // not on file creation.
    voom_store::connect_or_create(&url).await.unwrap();

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

    // Final state: exactly one migration applied, Current.
    let pool = voom_store::connect(&url).await.unwrap();
    let state = voom_store::probe_schema(&pool).await.unwrap();
    match state {
        voom_store::SchemaState::Current {
            migration_count, ..
        } => {
            assert_eq!(migration_count, 1, "exactly one migration row must exist");
        }
        other => panic!("post-race state must be Current, got {other:?}"),
    }
}
