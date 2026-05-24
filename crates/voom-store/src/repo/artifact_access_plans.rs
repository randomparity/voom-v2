//! Synthetic artifact access plans selected during remote lease acquisition.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{LeaseId, NodeId, TicketId, VoomError, WorkerId};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAccessMode {
    SharedMount,
    ControlPlanePlaceholder,
    StagedOutputPlaceholder,
}

impl ArtifactAccessMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SharedMount => "shared_mount",
            Self::ControlPlanePlaceholder => "control_plane_placeholder",
            Self::StagedOutputPlaceholder => "staged_output_placeholder",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "shared_mount" => Ok(Self::SharedMount),
            "control_plane_placeholder" => Ok(Self::ControlPlanePlaceholder),
            "staged_output_placeholder" => Ok(Self::StagedOutputPlaceholder),
            other => Err(VoomError::Database(format!(
                "artifact_access_plans.selected_access_mode {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAccessPlanStatus {
    Selected,
    Consumed,
    Rejected,
    Failed,
}

impl ArtifactAccessPlanStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Consumed => "consumed",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "selected" => Ok(Self::Selected),
            "consumed" => Ok(Self::Consumed),
            "rejected" => Ok(Self::Rejected),
            "failed" => Ok(Self::Failed),
            other => Err(VoomError::Database(format!(
                "artifact_access_plans.status {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewArtifactAccessPlan {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub node_id: NodeId,
    pub input_handles: Vec<String>,
    pub output_handles: Vec<String>,
    pub selected_access_mode: ArtifactAccessMode,
    pub evidence: JsonValue,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ArtifactAccessPlan {
    pub id: u64,
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub node_id: NodeId,
    pub input_handles: Vec<String>,
    pub output_handles: Vec<String>,
    pub selected_access_mode: ArtifactAccessMode,
    pub status: ArtifactAccessPlanStatus,
    pub reason: Option<String>,
    pub evidence: JsonValue,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[async_trait]
pub trait ArtifactAccessPlanRepo: Repository {
    async fn create_selected_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewArtifactAccessPlan,
    ) -> Result<ArtifactAccessPlan, VoomError>;

    async fn create_selected(
        &self,
        input: NewArtifactAccessPlan,
    ) -> Result<ArtifactAccessPlan, VoomError>;

    async fn mark_status_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: u64,
        status: ArtifactAccessPlanStatus,
        reason: Option<String>,
        evidence: JsonValue,
        now: OffsetDateTime,
    ) -> Result<ArtifactAccessPlan, VoomError>;

    async fn mark_status(
        &self,
        id: u64,
        status: ArtifactAccessPlanStatus,
        reason: Option<String>,
        evidence: JsonValue,
        now: OffsetDateTime,
    ) -> Result<ArtifactAccessPlan, VoomError>;

    async fn get_by_lease(
        &self,
        lease_id: LeaseId,
    ) -> Result<Option<ArtifactAccessPlan>, VoomError>;

    async fn get_by_lease_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        lease_id: LeaseId,
    ) -> Result<Option<ArtifactAccessPlan>, VoomError>;

    async fn list_by_ticket(
        &self,
        ticket_id: TicketId,
    ) -> Result<Vec<ArtifactAccessPlan>, VoomError>;

    async fn list_by_worker(
        &self,
        worker_id: WorkerId,
    ) -> Result<Vec<ArtifactAccessPlan>, VoomError>;

    async fn list_by_node(&self, node_id: NodeId) -> Result<Vec<ArtifactAccessPlan>, VoomError>;

    async fn list_by_mode_and_status(
        &self,
        mode: ArtifactAccessMode,
        status: ArtifactAccessPlanStatus,
    ) -> Result<Vec<ArtifactAccessPlan>, VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqliteArtifactAccessPlanRepo {
    pool: SqlitePool,
}

impl SqliteArtifactAccessPlanRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteArtifactAccessPlanRepo {}

#[async_trait]
impl ArtifactAccessPlanRepo for SqliteArtifactAccessPlanRepo {
    async fn create_selected_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewArtifactAccessPlan,
    ) -> Result<ArtifactAccessPlan, VoomError> {
        validate_plan_coherence_in_tx(tx, &input).await?;

        let input_handles = serialize_json(&input.input_handles, "input_handles")?;
        let output_handles = serialize_json(&input.output_handles, "output_handles")?;
        let evidence = serialize_json(&input.evidence, "artifact access evidence")?;
        let now = iso8601(input.now)?;

        let res = sqlx::query(
            "INSERT INTO artifact_access_plans \
             (lease_id, ticket_id, worker_id, node_id, input_handles, output_handles, \
              selected_access_mode, status, evidence, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'selected', ?, ?, ?)",
        )
        .bind(i64_from_u64(input.lease_id.0))
        .bind(i64_from_u64(input.ticket_id.0))
        .bind(i64_from_u64(input.worker_id.0))
        .bind(i64_from_u64(input.node_id.0))
        .bind(&input_handles)
        .bind(&output_handles)
        .bind(input.selected_access_mode.as_str())
        .bind(&evidence)
        .bind(&now)
        .bind(&now)
        .execute(&mut **tx)
        .await
        .map_err(|e| map_insert_err(&e, input.lease_id))?;

        get_by_id_in_tx(tx, u64_from_i64(res.last_insert_rowid())).await
    }

    async fn create_selected(
        &self,
        input: NewArtifactAccessPlan,
    ) -> Result<ArtifactAccessPlan, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let plan = self.create_selected_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(plan)
    }

    async fn mark_status_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: u64,
        status: ArtifactAccessPlanStatus,
        reason: Option<String>,
        evidence: JsonValue,
        now: OffsetDateTime,
    ) -> Result<ArtifactAccessPlan, VoomError> {
        if status == ArtifactAccessPlanStatus::Selected {
            return Err(VoomError::Conflict(format!(
                "artifact_access_plans id={id} cannot transition to selected"
            )));
        }

        let evidence = serialize_json(&evidence, "artifact access evidence")?;
        let now = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE artifact_access_plans \
             SET status = ?, reason = ?, evidence = ?, updated_at = ? \
             WHERE id = ? AND status = 'selected'",
        )
        .bind(status.as_str())
        .bind(reason)
        .bind(&evidence)
        .bind(&now)
        .bind(i64_from_u64(id))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_access_plans status update: {e}")))?;

        if res.rows_affected() != 1 {
            return match get_optional_by_id_in_tx(tx, id).await? {
                Some(plan) => Err(VoomError::Conflict(format!(
                    "artifact_access_plans id={id} already {}",
                    plan.status.as_str()
                ))),
                None => Err(VoomError::NotFound(format!(
                    "artifact_access_plans id={id} not found"
                ))),
            };
        }

        get_by_id_in_tx(tx, id).await
    }

    async fn mark_status(
        &self,
        id: u64,
        status: ArtifactAccessPlanStatus,
        reason: Option<String>,
        evidence: JsonValue,
        now: OffsetDateTime,
    ) -> Result<ArtifactAccessPlan, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let plan = self
            .mark_status_in_tx(&mut tx, id, status, reason, evidence, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(plan)
    }

    async fn get_by_lease(
        &self,
        lease_id: LeaseId,
    ) -> Result<Option<ArtifactAccessPlan>, VoomError> {
        let row = sqlx::query(SELECT_PLAN_BY_LEASE)
            .bind(i64_from_u64(lease_id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("artifact_access_plans get_by_lease: {e}")))?;
        row.as_ref().map(row_to_plan).transpose()
    }

    async fn get_by_lease_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        lease_id: LeaseId,
    ) -> Result<Option<ArtifactAccessPlan>, VoomError> {
        let row = sqlx::query(SELECT_PLAN_BY_LEASE)
            .bind(i64_from_u64(lease_id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| {
                VoomError::Database(format!("artifact_access_plans get_by_lease_in_tx: {e}"))
            })?;
        row.as_ref().map(row_to_plan).transpose()
    }

    async fn list_by_ticket(
        &self,
        ticket_id: TicketId,
    ) -> Result<Vec<ArtifactAccessPlan>, VoomError> {
        list_by_i64(
            &self.pool,
            SELECT_PLANS_BY_TICKET,
            i64_from_u64(ticket_id.0),
            "list_by_ticket",
        )
        .await
    }

    async fn list_by_worker(
        &self,
        worker_id: WorkerId,
    ) -> Result<Vec<ArtifactAccessPlan>, VoomError> {
        list_by_i64(
            &self.pool,
            SELECT_PLANS_BY_WORKER,
            i64_from_u64(worker_id.0),
            "list_by_worker",
        )
        .await
    }

    async fn list_by_node(&self, node_id: NodeId) -> Result<Vec<ArtifactAccessPlan>, VoomError> {
        list_by_i64(
            &self.pool,
            SELECT_PLANS_BY_NODE,
            i64_from_u64(node_id.0),
            "list_by_node",
        )
        .await
    }

    async fn list_by_mode_and_status(
        &self,
        mode: ArtifactAccessMode,
        status: ArtifactAccessPlanStatus,
    ) -> Result<Vec<ArtifactAccessPlan>, VoomError> {
        let rows = sqlx::query(SELECT_PLANS_BY_MODE_STATUS)
            .bind(mode.as_str())
            .bind(status.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| {
                VoomError::Database(format!(
                    "artifact_access_plans list_by_mode_and_status: {e}"
                ))
            })?;
        rows.iter().map(row_to_plan).collect()
    }
}

const SELECT_PLAN_BY_ID: &str = "SELECT id, lease_id, ticket_id, worker_id, node_id, \
     input_handles, output_handles, selected_access_mode, status, reason, evidence, created_at, \
     updated_at FROM artifact_access_plans WHERE id = ?";
const SELECT_PLAN_BY_LEASE: &str = "SELECT id, lease_id, ticket_id, worker_id, node_id, \
     input_handles, output_handles, selected_access_mode, status, reason, evidence, created_at, \
     updated_at FROM artifact_access_plans WHERE lease_id = ?";
const SELECT_PLANS_BY_TICKET: &str = "SELECT id, lease_id, ticket_id, worker_id, node_id, \
     input_handles, output_handles, selected_access_mode, status, reason, evidence, created_at, \
     updated_at FROM artifact_access_plans WHERE ticket_id = ? ORDER BY id";
const SELECT_PLANS_BY_WORKER: &str = "SELECT id, lease_id, ticket_id, worker_id, node_id, \
     input_handles, output_handles, selected_access_mode, status, reason, evidence, created_at, \
     updated_at FROM artifact_access_plans WHERE worker_id = ? ORDER BY id";
const SELECT_PLANS_BY_NODE: &str = "SELECT id, lease_id, ticket_id, worker_id, node_id, \
     input_handles, output_handles, selected_access_mode, status, reason, evidence, created_at, \
     updated_at FROM artifact_access_plans WHERE node_id = ? ORDER BY id";
const SELECT_PLANS_BY_MODE_STATUS: &str = "SELECT id, lease_id, ticket_id, worker_id, node_id, \
     input_handles, output_handles, selected_access_mode, status, reason, evidence, created_at, \
     updated_at FROM artifact_access_plans WHERE selected_access_mode = ? AND status = ? \
     ORDER BY id";

async fn validate_plan_coherence_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &NewArtifactAccessPlan,
) -> Result<(), VoomError> {
    let row = sqlx::query(
        "SELECT l.ticket_id AS lease_ticket_id, l.worker_id AS lease_worker_id, \
                w.node_id AS worker_node_id \
         FROM leases l \
         LEFT JOIN workers w ON w.id = ? \
         WHERE l.id = ?",
    )
    .bind(i64_from_u64(input.worker_id.0))
    .bind(i64_from_u64(input.lease_id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("artifact_access_plans coherence check: {e}")))?;

    let Some(row) = row else {
        return Err(VoomError::NotFound(format!(
            "leases id={} not found",
            input.lease_id.0
        )));
    };

    let lease_ticket_id: i64 = row
        .try_get("lease_ticket_id")
        .map_err(|e| map_row_err("artifact_access_plans coherence", &e))?;
    let lease_worker_id: i64 = row
        .try_get("lease_worker_id")
        .map_err(|e| map_row_err("artifact_access_plans coherence", &e))?;

    if u64_from_i64(lease_ticket_id) != input.ticket_id.0 {
        return Err(VoomError::Conflict(format!(
            "artifact_access_plans lease_id={} belongs to ticket_id={}, not ticket_id={}",
            input.lease_id.0,
            u64_from_i64(lease_ticket_id),
            input.ticket_id.0
        )));
    }
    if u64_from_i64(lease_worker_id) != input.worker_id.0 {
        return Err(VoomError::Conflict(format!(
            "artifact_access_plans lease_id={} belongs to worker_id={}, not worker_id={}",
            input.lease_id.0,
            u64_from_i64(lease_worker_id),
            input.worker_id.0
        )));
    }

    let worker_node_id: Option<i64> = row
        .try_get("worker_node_id")
        .map_err(|e| map_row_err("artifact_access_plans coherence", &e))?;
    let Some(worker_node_id) = worker_node_id else {
        return Err(VoomError::NotFound(format!(
            "workers id={} not found",
            input.worker_id.0
        )));
    };

    if u64_from_i64(worker_node_id) != input.node_id.0 {
        return Err(VoomError::Conflict(format!(
            "artifact_access_plans worker_id={} belongs to node_id={}, not node_id={}",
            input.worker_id.0,
            u64_from_i64(worker_node_id),
            input.node_id.0
        )));
    }

    Ok(())
}

async fn get_by_id_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: u64,
) -> Result<ArtifactAccessPlan, VoomError> {
    get_optional_by_id_in_tx(tx, id).await?.ok_or_else(|| {
        VoomError::Internal(format!(
            "artifact_access_plans row vanished after write id={id}"
        ))
    })
}

async fn get_optional_by_id_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: u64,
) -> Result<Option<ArtifactAccessPlan>, VoomError> {
    let row = sqlx::query(SELECT_PLAN_BY_ID)
        .bind(i64_from_u64(id))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_access_plans get_by_id: {e}")))?;
    row.as_ref().map(row_to_plan).transpose()
}

fn map_insert_err(err: &sqlx::Error, lease_id: LeaseId) -> VoomError {
    if is_unique_violation(err) {
        VoomError::Conflict(format!(
            "artifact_access_plans lease_id={} already has a selected plan",
            lease_id.0
        ))
    } else {
        VoomError::Database(format!("artifact_access_plans insert: {err}"))
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

async fn list_by_i64(
    pool: &SqlitePool,
    sql: &str,
    value: i64,
    label: &'static str,
) -> Result<Vec<ArtifactAccessPlan>, VoomError> {
    let rows = sqlx::query(sql)
        .bind(value)
        .fetch_all(pool)
        .await
        .map_err(|e| VoomError::Database(format!("artifact_access_plans {label}: {e}")))?;
    rows.iter().map(row_to_plan).collect()
}

fn row_to_plan(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactAccessPlan, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let lease_id: i64 = row
        .try_get("lease_id")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let ticket_id: i64 = row
        .try_get("ticket_id")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let worker_id: i64 = row
        .try_get("worker_id")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let node_id: i64 = row
        .try_get("node_id")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let input_handles: String = row
        .try_get("input_handles")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let output_handles: String = row
        .try_get("output_handles")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let mode: String = row
        .try_get("selected_access_mode")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let status: String = row
        .try_get("status")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let reason: Option<String> = row
        .try_get("reason")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let evidence: String = row
        .try_get("evidence")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;
    let updated_at: String = row
        .try_get("updated_at")
        .map_err(|e| map_row_err("artifact_access_plans", &e))?;

    Ok(ArtifactAccessPlan {
        id: u64_from_i64(id),
        lease_id: LeaseId(u64_from_i64(lease_id)),
        ticket_id: TicketId(u64_from_i64(ticket_id)),
        worker_id: WorkerId(u64_from_i64(worker_id)),
        node_id: NodeId(u64_from_i64(node_id)),
        input_handles: serde_json::from_str(&input_handles).map_err(|e| {
            VoomError::Database(format!("artifact_access_plans input_handles: {e}"))
        })?,
        output_handles: serde_json::from_str(&output_handles).map_err(|e| {
            VoomError::Database(format!("artifact_access_plans output_handles: {e}"))
        })?,
        selected_access_mode: ArtifactAccessMode::parse(&mode)?,
        status: ArtifactAccessPlanStatus::parse(&status)?,
        reason,
        evidence: serde_json::from_str(&evidence)
            .map_err(|e| VoomError::Database(format!("artifact_access_plans evidence: {e}")))?,
        created_at: parse_iso8601(&created_at)?,
        updated_at: parse_iso8601(&updated_at)?,
    })
}

#[cfg(test)]
#[path = "artifact_access_plans_test.rs"]
mod tests;
