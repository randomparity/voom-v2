use async_trait::async_trait;
use sqlx::SqlitePool;
use voom_core::VoomError;

use super::Repository;

#[async_trait]
pub trait SchemaMetaRepo: Repository {
    async fn get(&self, key: &str) -> Result<Option<String>, VoomError>;
    async fn set(&self, key: &str, value: &str) -> Result<(), VoomError>;
}

#[derive(Debug)]
pub struct SqliteSchemaMetaRepo {
    pool: SqlitePool,
}

impl SqliteSchemaMetaRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteSchemaMetaRepo {}

#[async_trait]
impl SchemaMetaRepo for SqliteSchemaMetaRepo {
    async fn get(&self, key: &str) -> Result<Option<String>, VoomError> {
        sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("schema_meta get({key:?}) failed: {e}")))
    }

    async fn set(&self, key: &str, value: &str) -> Result<(), VoomError> {
        sqlx::query(
            "INSERT INTO schema_meta (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|e| VoomError::Database(format!("schema_meta set({key:?}) failed: {e}")))
    }
}
