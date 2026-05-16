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
    let SchemaState::Current { migration_count, .. } = state else {
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
