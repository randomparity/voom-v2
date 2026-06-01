//! Scheduler-owned node capacity limits.

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{NodeId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, iso8601, map_row_err, parse_iso8601, u32_from_i64};

#[derive(Debug, Clone)]
pub struct SchedulerNodeLimit {
    pub node_id: NodeId,
    pub max_parallel_leases: u32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SqliteSchedulerNodeLimitRepo {
    pool: SqlitePool,
}

impl SqliteSchedulerNodeLimitRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteSchedulerNodeLimitRepo {}

#[async_trait]
pub trait SchedulerNodeLimitRepo: Repository {
    async fn node_limit_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        node_id: NodeId,
    ) -> Result<u32, VoomError>;

    async fn set_node_limit(
        &self,
        node_id: NodeId,
        max_parallel_leases: u32,
        now: OffsetDateTime,
    ) -> Result<SchedulerNodeLimit, VoomError>;
}

#[async_trait]
impl SchedulerNodeLimitRepo for SqliteSchedulerNodeLimitRepo {
    async fn node_limit_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        node_id: NodeId,
    ) -> Result<u32, VoomError> {
        let row =
            sqlx::query("SELECT max_parallel_leases FROM scheduler_node_limits WHERE node_id = ?")
                .bind(i64_from_u64(node_id.0))
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| VoomError::Database(format!("scheduler_node_limits get: {e}")))?;

        let Some(row) = row else {
            return Ok(1);
        };
        let max_parallel_leases: i64 = row
            .try_get("max_parallel_leases")
            .map_err(|e| map_row_err("scheduler_node_limits", &e))?;
        u32_from_i64(max_parallel_leases)
    }

    async fn set_node_limit(
        &self,
        node_id: NodeId,
        max_parallel_leases: u32,
        now: OffsetDateTime,
    ) -> Result<SchedulerNodeLimit, VoomError> {
        if max_parallel_leases == 0 {
            return Err(VoomError::Config(
                "scheduler node limit must be positive".to_owned(),
            ));
        }

        let now = iso8601(now)?;
        let row = sqlx::query(
            "INSERT INTO scheduler_node_limits \
             (node_id, max_parallel_leases, created_at, updated_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(node_id) DO UPDATE SET \
                 max_parallel_leases = excluded.max_parallel_leases, \
                 updated_at = excluded.updated_at \
             RETURNING node_id, max_parallel_leases, created_at, updated_at",
        )
        .bind(i64_from_u64(node_id.0))
        .bind(i64::from(max_parallel_leases))
        .bind(&now)
        .bind(&now)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("scheduler_node_limits upsert: {e}")))?;
        row_to_node_limit(&row)
    }
}

fn row_to_node_limit(row: &sqlx::sqlite::SqliteRow) -> Result<SchedulerNodeLimit, VoomError> {
    let node_id: i64 = row
        .try_get("node_id")
        .map_err(|e| map_row_err("scheduler_node_limits", &e))?;
    let max_parallel_leases: i64 = row
        .try_get("max_parallel_leases")
        .map_err(|e| map_row_err("scheduler_node_limits", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("scheduler_node_limits", &e))?;
    let updated_at: String = row
        .try_get("updated_at")
        .map_err(|e| map_row_err("scheduler_node_limits", &e))?;
    Ok(SchedulerNodeLimit {
        node_id: NodeId(u64::try_from(node_id).map_err(|e| {
            VoomError::Database(format!("scheduler_node_limits.node_id out of range: {e}"))
        })?),
        max_parallel_leases: u32_from_i64(max_parallel_leases)?,
        created_at: parse_iso8601(&created_at)?,
        updated_at: parse_iso8601(&updated_at)?,
    })
}

#[cfg(test)]
#[path = "scheduler_node_limits_test.rs"]
mod tests;
