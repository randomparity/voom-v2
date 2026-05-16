#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

use tempfile::NamedTempFile;
use voom_store::repo::{SchemaMetaRepo, SqliteSchemaMetaRepo};
use voom_store::{connect, init};

async fn fresh_initialized_pool() -> (NamedTempFile, sqlx::SqlitePool) {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    init(&url).await.unwrap();
    let pool = connect(&url).await.unwrap();
    (tmp, pool)
}

#[tokio::test]
async fn set_then_get_returns_value() {
    let (_keep, pool) = fresh_initialized_pool().await;
    let repo = SqliteSchemaMetaRepo::new(pool);
    repo.set("hello", "world").await.unwrap();
    assert_eq!(repo.get("hello").await.unwrap().as_deref(), Some("world"));
}

#[tokio::test]
async fn get_missing_key_returns_none() {
    let (_keep, pool) = fresh_initialized_pool().await;
    let repo = SqliteSchemaMetaRepo::new(pool);
    assert!(repo.get("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn set_twice_overwrites() {
    let (_keep, pool) = fresh_initialized_pool().await;
    let repo = SqliteSchemaMetaRepo::new(pool);
    repo.set("k", "v1").await.unwrap();
    repo.set("k", "v2").await.unwrap();
    assert_eq!(repo.get("k").await.unwrap().as_deref(), Some("v2"));
}
