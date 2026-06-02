#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

use tempfile::NamedTempFile;
use voom_store::repo::SqliteSchemaMetaRepo;
use voom_store::test_support::fresh_initialized_pool_at;

#[tokio::test]
async fn set_then_get_returns_value() {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let repo = SqliteSchemaMetaRepo::new(pool);
    repo.set("hello", "world").await.unwrap();
    assert_eq!(repo.get("hello").await.unwrap().as_deref(), Some("world"));
}

#[tokio::test]
async fn get_missing_key_returns_none() {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let repo = SqliteSchemaMetaRepo::new(pool);
    assert!(repo.get("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn set_twice_overwrites() {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let repo = SqliteSchemaMetaRepo::new(pool);
    repo.set("k", "v1").await.unwrap();
    repo.set("k", "v2").await.unwrap();
    assert_eq!(repo.get("k").await.unwrap().as_deref(), Some("v2"));
}
