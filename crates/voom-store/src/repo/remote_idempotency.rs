//! Remote execution route idempotency records.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{NodeId, VoomError, WorkerId};

use super::Repository;
use super::common::{i64_from_u64, iso8601, map_row_err, serialize_json};

#[derive(Debug, Clone)]
pub struct RemoteIdempotencyInput {
    pub node_id: NodeId,
    pub route_key: String,
    pub worker_id: Option<WorkerId>,
    pub idempotency_key: String,
    pub request_hash: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IdempotencyOutcome {
    Reserved,
    Replay(RemoteMutationReplay),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RemoteMutationReplay {
    Ok { data: JsonValue },
    Error { code: String, message: String },
}

#[async_trait]
pub trait RemoteIdempotencyRepo: Repository {
    async fn reserve_or_replay_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: RemoteIdempotencyInput,
    ) -> Result<IdempotencyOutcome, VoomError>;

    async fn complete_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        node_id: NodeId,
        route_key: &str,
        worker_id: Option<WorkerId>,
        idempotency_key: &str,
        response: RemoteMutationReplay,
    ) -> Result<(), VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqliteRemoteIdempotencyRepo {
    #[expect(
        dead_code,
        reason = "Repository impls keep the pool even when this trait only exposes in-tx primitives"
    )]
    pool: SqlitePool,
}

impl SqliteRemoteIdempotencyRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteRemoteIdempotencyRepo {}

#[async_trait]
impl RemoteIdempotencyRepo for SqliteRemoteIdempotencyRepo {
    async fn reserve_or_replay_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: RemoteIdempotencyInput,
    ) -> Result<IdempotencyOutcome, VoomError> {
        let worker_scope_id = worker_scope_id(input.worker_id);
        let worker_id = input.worker_id.map(|id| i64_from_u64(id.0));
        let created_at = iso8601(input.created_at)?;
        let inserted = sqlx::query(
            "INSERT INTO remote_idempotency_keys \
             (node_id, route_key, worker_scope_id, worker_id, idempotency_key, request_hash, \
              status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'in_progress', ?) \
             ON CONFLICT(node_id, route_key, worker_scope_id, idempotency_key) DO NOTHING",
        )
        .bind(i64_from_u64(input.node_id.0))
        .bind(&input.route_key)
        .bind(worker_scope_id)
        .bind(worker_id)
        .bind(&input.idempotency_key)
        .bind(&input.request_hash)
        .bind(&created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("remote idempotency insert: {e}")))?;

        if inserted.rows_affected() == 1 {
            return Ok(IdempotencyOutcome::Reserved);
        }

        let existing = sqlx::query(
            "SELECT request_hash, response_json, status FROM remote_idempotency_keys \
             WHERE node_id = ? AND route_key = ? AND worker_scope_id = ? AND idempotency_key = ?",
        )
        .bind(i64_from_u64(input.node_id.0))
        .bind(&input.route_key)
        .bind(worker_scope_id)
        .bind(&input.idempotency_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("remote idempotency get: {e}")))?;

        let Some(row) = existing else {
            return Err(VoomError::Database(
                "remote idempotency insert conflict had no existing row".to_owned(),
            ));
        };

        let request_hash: String = row
            .try_get("request_hash")
            .map_err(|e| map_row_err("remote_idempotency_keys", &e))?;
        if request_hash != input.request_hash {
            return Err(VoomError::Conflict(
                "idempotency key reused with different request body".to_owned(),
            ));
        }

        let status: String = row
            .try_get("status")
            .map_err(|e| map_row_err("remote_idempotency_keys", &e))?;
        match status.as_str() {
            "completed" => {
                let response_json: String = row
                    .try_get("response_json")
                    .map_err(|e| map_row_err("remote_idempotency_keys", &e))?;
                let response = serde_json::from_str(&response_json).map_err(|e| {
                    VoomError::Database(format!("remote idempotency response_json: {e}"))
                })?;
                Ok(IdempotencyOutcome::Replay(response))
            }
            "in_progress" => Err(VoomError::Conflict(
                "idempotency key is already in progress".to_owned(),
            )),
            other => Err(VoomError::Database(format!(
                "remote_idempotency_keys.status {other:?} not in vocab"
            ))),
        }
    }

    async fn complete_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        node_id: NodeId,
        route_key: &str,
        worker_id: Option<WorkerId>,
        idempotency_key: &str,
        response: RemoteMutationReplay,
    ) -> Result<(), VoomError> {
        let response_json = serialize_json(&response, "remote idempotency response_json")?;
        let res = sqlx::query(
            "UPDATE remote_idempotency_keys \
             SET status = 'completed', response_json = ? \
             WHERE node_id = ? AND route_key = ? AND worker_scope_id = ? \
               AND idempotency_key = ? AND status = 'in_progress'",
        )
        .bind(&response_json)
        .bind(i64_from_u64(node_id.0))
        .bind(route_key)
        .bind(worker_scope_id(worker_id))
        .bind(idempotency_key)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("remote idempotency complete: {e}")))?;

        if res.rows_affected() == 1 {
            Ok(())
        } else {
            Err(VoomError::Conflict(format!(
                "remote idempotency key {idempotency_key:?} is not reserved"
            )))
        }
    }
}

fn worker_scope_id(worker_id: Option<WorkerId>) -> i64 {
    worker_id.map_or(0, |id| i64_from_u64(id.0))
}

#[cfg(test)]
#[path = "remote_idempotency_test.rs"]
mod tests;
