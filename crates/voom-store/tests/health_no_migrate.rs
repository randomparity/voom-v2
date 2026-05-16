#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

use voom_store::{SchemaState, connect, probe_schema};

/// `connect()` and `probe_schema()` must NEVER create the migration tracking
/// table. This is the contract that makes `voom health` safe to run against
/// a DB the operator hasn't yet initialized.
#[tokio::test]
async fn connect_then_probe_leaves_db_uninitialized() {
    let pool = connect("sqlite::memory:").await.unwrap();
    assert_eq!(
        probe_schema(&pool).await.unwrap(),
        SchemaState::Uninitialized
    );

    // Re-probe; still uninitialized.
    assert_eq!(
        probe_schema(&pool).await.unwrap(),
        SchemaState::Uninitialized
    );

    // Direct inspection: _sqlx_migrations table must not exist.
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(exists, 0, "read-side calls must not create migration table");
}
