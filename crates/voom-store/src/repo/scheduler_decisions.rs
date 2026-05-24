//! Durable scheduler decision logs and scheduler-owned node limits.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{LeaseId, NodeId, TicketId, VoomError, WorkerId};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerDecisionKind {
    LeaseAcquire,
    Idle,
    NoCandidate,
}

impl SchedulerDecisionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LeaseAcquire => "lease_acquire",
            Self::Idle => "idle",
            Self::NoCandidate => "no_candidate",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "lease_acquire" => Ok(Self::LeaseAcquire),
            "idle" => Ok(Self::Idle),
            "no_candidate" => Ok(Self::NoCandidate),
            other => Err(VoomError::Database(format!(
                "scheduler_decisions.decision_kind {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerRequestSource {
    RemoteAcquire,
}

impl SchedulerRequestSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RemoteAcquire => "remote_acquire",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "remote_acquire" => Ok(Self::RemoteAcquire),
            other => Err(VoomError::Database(format!(
                "scheduler_decisions.request_source {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerDecisionOutcome {
    Selected,
    Idle,
    NoEligibleCandidate,
    Rejected,
}

impl SchedulerDecisionOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Idle => "idle",
            Self::NoEligibleCandidate => "no_eligible_candidate",
            Self::Rejected => "rejected",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "selected" => Ok(Self::Selected),
            "idle" => Ok(Self::Idle),
            "no_eligible_candidate" => Ok(Self::NoEligibleCandidate),
            "rejected" => Ok(Self::Rejected),
            other => Err(VoomError::Database(format!(
                "scheduler_decisions.outcome {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerReasonCode {
    Selected,
    NoReadyTicket,
    MissingCapability,
    MissingGrant,
    OperationDenied,
    WorkerNotExecutable,
    NodeNotExecutable,
    HeartbeatExpired,
    UnsupportedArtifactAccess,
    WorkerCapacityFull,
    NodeCapacityFull,
    NoEligibleCandidate,
}

impl SchedulerReasonCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::NoReadyTicket => "no_ready_ticket",
            Self::MissingCapability => "missing_capability",
            Self::MissingGrant => "missing_grant",
            Self::OperationDenied => "operation_denied",
            Self::WorkerNotExecutable => "worker_not_executable",
            Self::NodeNotExecutable => "node_not_executable",
            Self::HeartbeatExpired => "heartbeat_expired",
            Self::UnsupportedArtifactAccess => "unsupported_artifact_access",
            Self::WorkerCapacityFull => "worker_capacity_full",
            Self::NodeCapacityFull => "node_capacity_full",
            Self::NoEligibleCandidate => "no_eligible_candidate",
        }
    }

    pub fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "selected" => Ok(Self::Selected),
            "no_ready_ticket" => Ok(Self::NoReadyTicket),
            "missing_capability" => Ok(Self::MissingCapability),
            "missing_grant" => Ok(Self::MissingGrant),
            "operation_denied" => Ok(Self::OperationDenied),
            "worker_not_executable" => Ok(Self::WorkerNotExecutable),
            "node_not_executable" => Ok(Self::NodeNotExecutable),
            "heartbeat_expired" => Ok(Self::HeartbeatExpired),
            "unsupported_artifact_access" => Ok(Self::UnsupportedArtifactAccess),
            "worker_capacity_full" => Ok(Self::WorkerCapacityFull),
            "node_capacity_full" => Ok(Self::NodeCapacityFull),
            "no_eligible_candidate" => Ok(Self::NoEligibleCandidate),
            other => Err(VoomError::Database(format!(
                "scheduler_decisions.reason_code {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewSchedulerDecision {
    pub decision_kind: SchedulerDecisionKind,
    pub request_source: SchedulerRequestSource,
    pub idempotency_key: Option<String>,
    pub request_node_id: Option<NodeId>,
    pub request_worker_id: Option<WorkerId>,
    pub ticket_id: Option<TicketId>,
    pub selected_worker_id: Option<WorkerId>,
    pub selected_node_id: Option<NodeId>,
    pub selected_lease_id: Option<LeaseId>,
    pub outcome: SchedulerDecisionOutcome,
    pub reason_code: SchedulerReasonCode,
    pub summary: String,
    pub candidate_count: u32,
    pub selected_score: Option<i64>,
    pub suppression_key: Option<String>,
    pub explanation: JsonValue,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SchedulerDecision {
    pub id: u64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub first_seen_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub decision_kind: SchedulerDecisionKind,
    pub request_source: SchedulerRequestSource,
    pub idempotency_key: Option<String>,
    pub request_node_id: Option<NodeId>,
    pub request_worker_id: Option<WorkerId>,
    pub ticket_id: Option<TicketId>,
    pub selected_worker_id: Option<WorkerId>,
    pub selected_node_id: Option<NodeId>,
    pub selected_lease_id: Option<LeaseId>,
    pub outcome: SchedulerDecisionOutcome,
    pub reason_code: SchedulerReasonCode,
    pub summary: String,
    pub candidate_count: u32,
    pub selected_score: Option<i64>,
    pub suppressed_count: u32,
    pub suppression_key: Option<String>,
    pub explanation: JsonValue,
}

#[derive(Debug, Clone, Copy)]
pub struct SchedulerDecisionFilter {
    pub ticket_id: Option<TicketId>,
    pub worker_id: Option<WorkerId>,
    pub node_id: Option<NodeId>,
    pub outcome: Option<SchedulerDecisionOutcome>,
    pub limit: u32,
}

impl Default for SchedulerDecisionFilter {
    fn default() -> Self {
        Self {
            ticket_id: None,
            worker_id: None,
            node_id: None,
            outcome: None,
            limit: 100,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SchedulerNodeLimit {
    pub node_id: NodeId,
    pub max_parallel_leases: u32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SqliteSchedulerDecisionRepo {
    pool: SqlitePool,
}

impl SqliteSchedulerDecisionRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteSchedulerDecisionRepo {}

#[async_trait]
pub trait SchedulerDecisionRepo: Repository {
    async fn create(&self, input: NewSchedulerDecision) -> Result<SchedulerDecision, VoomError>;

    async fn create_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewSchedulerDecision,
    ) -> Result<SchedulerDecision, VoomError>;

    async fn create_or_suppress(
        &self,
        input: NewSchedulerDecision,
    ) -> Result<SchedulerDecision, VoomError>;

    async fn create_or_suppress_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewSchedulerDecision,
    ) -> Result<SchedulerDecision, VoomError>;

    async fn link_selected_lease(
        &self,
        id: u64,
        lease_id: LeaseId,
        now: OffsetDateTime,
    ) -> Result<SchedulerDecision, VoomError>;

    async fn link_selected_lease_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: u64,
        lease_id: LeaseId,
        now: OffsetDateTime,
    ) -> Result<SchedulerDecision, VoomError>;

    async fn get(&self, id: u64) -> Result<Option<SchedulerDecision>, VoomError>;

    async fn list(
        &self,
        filter: SchedulerDecisionFilter,
    ) -> Result<Vec<SchedulerDecision>, VoomError>;

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
impl SchedulerDecisionRepo for SqliteSchedulerDecisionRepo {
    async fn create(&self, input: NewSchedulerDecision) -> Result<SchedulerDecision, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let decision = self.create_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(decision)
    }

    async fn create_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewSchedulerDecision,
    ) -> Result<SchedulerDecision, VoomError> {
        let prepared = prepare_decision_insert(&input)?;
        let sql = decision_insert_sql(&format!("RETURNING {DECISION_COLS}"));
        let row = bind_decision_query(
            sqlx::query(&sql),
            &input,
            prepared.now,
            prepared.explanation,
        )
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scheduler_decisions insert: {e}")))?;
        row_to_decision(&row)
    }

    async fn create_or_suppress(
        &self,
        input: NewSchedulerDecision,
    ) -> Result<SchedulerDecision, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let decision = self.create_or_suppress_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(decision)
    }

    async fn create_or_suppress_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewSchedulerDecision,
    ) -> Result<SchedulerDecision, VoomError> {
        let prepared = prepare_decision_insert(&input)?;
        let sql = decision_insert_sql(&format!(
            "{DECISION_INSERT_SUPPRESS_CLAUSE} RETURNING {DECISION_COLS}"
        ));
        let row = bind_decision_query(
            sqlx::query(&sql),
            &input,
            prepared.now,
            prepared.explanation,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scheduler_decisions upsert: {e}")))?;
        row.as_ref()
            .map(row_to_decision)
            .transpose()?
            .ok_or_else(|| {
                VoomError::Conflict(
                    "scheduler suppression_key already belongs to a different decision".to_owned(),
                )
            })
    }

    async fn link_selected_lease(
        &self,
        id: u64,
        lease_id: LeaseId,
        now: OffsetDateTime,
    ) -> Result<SchedulerDecision, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let decision = self
            .link_selected_lease_in_tx(&mut tx, id, lease_id, now)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(decision)
    }

    async fn link_selected_lease_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: u64,
        lease_id: LeaseId,
        now: OffsetDateTime,
    ) -> Result<SchedulerDecision, VoomError> {
        validate_selected_lease_link_in_tx(tx, id, lease_id).await?;
        let now = iso8601(now)?;
        let row = sqlx::query(&format!(
            "UPDATE scheduler_decisions \
             SET selected_lease_id = ?, updated_at = ? \
             WHERE id = ? AND (selected_lease_id IS NULL OR selected_lease_id = ?) \
             RETURNING {DECISION_COLS}"
        ))
        .bind(i64_from_u64(lease_id.0))
        .bind(now)
        .bind(i64_from_u64(id))
        .bind(i64_from_u64(lease_id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scheduler_decisions link lease: {e}")))?;

        match row.as_ref().map(row_to_decision).transpose()? {
            Some(decision) => Ok(decision),
            None => link_selected_lease_after_empty_update_in_tx(tx, id, lease_id).await,
        }
    }

    async fn get(&self, id: u64) -> Result<Option<SchedulerDecision>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {DECISION_COLS} FROM scheduler_decisions WHERE id = ?"
        ))
        .bind(i64_from_u64(id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("scheduler_decisions get: {e}")))?;
        row.as_ref().map(row_to_decision).transpose()
    }

    async fn list(
        &self,
        filter: SchedulerDecisionFilter,
    ) -> Result<Vec<SchedulerDecision>, VoomError> {
        let ticket_id = filter.ticket_id.map(|id| i64_from_u64(id.0));
        let worker_id = filter.worker_id.map(|id| i64_from_u64(id.0));
        let node_id = filter.node_id.map(|id| i64_from_u64(id.0));
        let outcome = filter.outcome.map(SchedulerDecisionOutcome::as_str);
        let limit = if filter.limit == 0 { 100 } else { filter.limit };

        let rows = sqlx::query(&format!(
            "SELECT {DECISION_COLS} FROM scheduler_decisions \
             WHERE (? IS NULL OR ticket_id = ?) \
               AND (? IS NULL OR request_worker_id = ? OR selected_worker_id = ?) \
               AND (? IS NULL OR request_node_id = ? OR selected_node_id = ?) \
               AND (? IS NULL OR outcome = ?) \
             ORDER BY created_at DESC, id DESC \
             LIMIT ?"
        ))
        .bind(ticket_id)
        .bind(ticket_id)
        .bind(worker_id)
        .bind(worker_id)
        .bind(worker_id)
        .bind(node_id)
        .bind(node_id)
        .bind(node_id)
        .bind(outcome)
        .bind(outcome)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("scheduler_decisions list: {e}")))?;

        rows.iter().map(row_to_decision).collect()
    }

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

const DECISION_COLS: &str = "id, created_at, updated_at, first_seen_at, last_seen_at, \
     decision_kind, request_source, idempotency_key, request_node_id, request_worker_id, \
     ticket_id, selected_worker_id, selected_node_id, selected_lease_id, outcome, reason_code, \
     summary, candidate_count, selected_score, suppressed_count, suppression_key, \
     explanation_json";
const DECISION_INSERT_COLS: &str = "created_at, updated_at, first_seen_at, last_seen_at, \
     decision_kind, request_source, idempotency_key, request_node_id, request_worker_id, \
     ticket_id, selected_worker_id, selected_node_id, selected_lease_id, outcome, reason_code, \
     summary, candidate_count, selected_score, suppression_key, explanation_json";
const DECISION_INSERT_VALUES: &str = "?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?";
const DECISION_INSERT_SUPPRESS_CLAUSE: &str = "ON CONFLICT(suppression_key) WHERE suppression_key IS NOT NULL DO UPDATE SET \
         updated_at = excluded.updated_at, \
         last_seen_at = excluded.last_seen_at, \
         suppressed_count = scheduler_decisions.suppressed_count + 1 \
     WHERE scheduler_decisions.decision_kind = excluded.decision_kind \
       AND scheduler_decisions.request_source = excluded.request_source \
       AND scheduler_decisions.request_node_id IS excluded.request_node_id \
       AND scheduler_decisions.request_worker_id IS excluded.request_worker_id \
       AND scheduler_decisions.ticket_id IS excluded.ticket_id \
       AND scheduler_decisions.selected_worker_id IS excluded.selected_worker_id \
       AND scheduler_decisions.selected_node_id IS excluded.selected_node_id \
       AND scheduler_decisions.outcome = excluded.outcome \
       AND scheduler_decisions.reason_code = excluded.reason_code \
       AND scheduler_decisions.candidate_count = excluded.candidate_count \
       AND scheduler_decisions.selected_score IS excluded.selected_score";

struct PreparedSchedulerDecisionInsert {
    now: String,
    explanation: String,
}

fn prepare_decision_insert(
    input: &NewSchedulerDecision,
) -> Result<PreparedSchedulerDecisionInsert, VoomError> {
    validate_decision_shape(input)?;
    validate_suppression_key(input)?;
    reject_selected_lease_on_create(input)?;
    Ok(PreparedSchedulerDecisionInsert {
        now: iso8601(input.now)?,
        explanation: serialize_json(&input.explanation, "scheduler decision explanation")?,
    })
}

fn decision_insert_sql(suffix: &str) -> String {
    format!(
        "INSERT INTO scheduler_decisions ({DECISION_INSERT_COLS}) \
         VALUES ({DECISION_INSERT_VALUES}) {suffix}"
    )
}

fn validate_decision_shape(input: &NewSchedulerDecision) -> Result<(), VoomError> {
    validate_request_context(input)?;
    match (input.decision_kind, input.outcome) {
        (SchedulerDecisionKind::LeaseAcquire, SchedulerDecisionOutcome::Selected) => {
            if input.ticket_id.is_some()
                && input.selected_worker_id.is_some()
                && input.selected_node_id.is_some()
                && input.reason_code == SchedulerReasonCode::Selected
                && input.candidate_count > 0
                && input.selected_worker_id == input.request_worker_id
                && input.selected_node_id == input.request_node_id
            {
                Ok(())
            } else {
                Err(VoomError::Config(
                    "selected scheduler decisions require selected reason, candidates, ticket, and matching request/selected worker and node ids".to_owned(),
                ))
            }
        }
        (SchedulerDecisionKind::Idle, SchedulerDecisionOutcome::Idle) => {
            if input.ticket_id.is_none()
                && input.selected_worker_id.is_none()
                && input.selected_node_id.is_none()
                && input.selected_lease_id.is_none()
                && input.selected_score.is_none()
                && input.reason_code == SchedulerReasonCode::NoReadyTicket
                && input.candidate_count == 0
            {
                Ok(())
            } else {
                Err(VoomError::Config(
                    "idle scheduler decisions require no-ready-ticket reason, zero candidates, and no selected tuple".to_owned(),
                ))
            }
        }
        (SchedulerDecisionKind::NoCandidate, SchedulerDecisionOutcome::NoEligibleCandidate) => {
            if input.selected_worker_id.is_none()
                && input.selected_node_id.is_none()
                && input.selected_lease_id.is_none()
                && input.selected_score.is_none()
                && input.reason_code != SchedulerReasonCode::Selected
                && input.candidate_count > 0
            {
                Ok(())
            } else {
                Err(VoomError::Config(
                    "no-candidate scheduler decisions require non-selected reason, candidates, and no selected tuple".to_owned(),
                ))
            }
        }
        (SchedulerDecisionKind::LeaseAcquire, SchedulerDecisionOutcome::Rejected) => {
            if input.selected_worker_id.is_none()
                && input.selected_node_id.is_none()
                && input.selected_lease_id.is_none()
                && input.selected_score.is_none()
                && input.reason_code != SchedulerReasonCode::Selected
            {
                Ok(())
            } else {
                Err(VoomError::Config(
                    "rejected scheduler decisions require non-selected reason and no selected tuple".to_owned(),
                ))
            }
        }
        _ => Err(VoomError::Config(format!(
            "scheduler decision kind {} is incompatible with outcome {}",
            input.decision_kind.as_str(),
            input.outcome.as_str()
        ))),
    }
}

fn validate_request_context(input: &NewSchedulerDecision) -> Result<(), VoomError> {
    match input.request_source {
        SchedulerRequestSource::RemoteAcquire => {
            if input.request_node_id.is_some() && input.request_worker_id.is_some() {
                Ok(())
            } else {
                Err(VoomError::Config(
                    "remote-acquire scheduler decisions require request node and worker ids"
                        .to_owned(),
                ))
            }
        }
    }
}

fn validate_suppression_key(input: &NewSchedulerDecision) -> Result<(), VoomError> {
    if input.suppression_key.is_none() {
        return Ok(());
    }
    if matches!(
        (input.decision_kind, input.outcome),
        (SchedulerDecisionKind::Idle, SchedulerDecisionOutcome::Idle)
            | (
                SchedulerDecisionKind::NoCandidate,
                SchedulerDecisionOutcome::NoEligibleCandidate,
            )
    ) {
        return Ok(());
    }
    Err(VoomError::Config(
        "scheduler suppression_key is only valid for idle or no-candidate decisions".to_owned(),
    ))
}

fn reject_selected_lease_on_create(input: &NewSchedulerDecision) -> Result<(), VoomError> {
    if input.selected_lease_id.is_none() {
        return Ok(());
    }
    Err(VoomError::Config(
        "scheduler selected_lease_id must be linked after decision creation".to_owned(),
    ))
}

async fn validate_selected_lease_link_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    decision_id: u64,
    lease_id: LeaseId,
) -> Result<(), VoomError> {
    let row = sqlx::query(
        "SELECT d.decision_kind, d.outcome, d.ticket_id, d.selected_worker_id, \
                d.selected_node_id, d.selected_lease_id, l.ticket_id AS lease_ticket_id, \
                l.worker_id AS lease_worker_id, w.node_id AS lease_node_id \
         FROM scheduler_decisions d \
         LEFT JOIN leases l ON l.id = ? \
         LEFT JOIN workers w ON w.id = l.worker_id \
         WHERE d.id = ?",
    )
    .bind(i64_from_u64(lease_id.0))
    .bind(i64_from_u64(decision_id))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("scheduler_decisions link coherence: {e}")))?;

    let Some(row) = row else {
        return Err(VoomError::NotFound(format!(
            "scheduler_decisions id={decision_id} not found"
        )));
    };

    validate_selected_lease_link_facts(decision_id, lease_id, &link_facts_from_row(&row)?)
}

async fn link_selected_lease_after_empty_update_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    decision_id: u64,
    lease_id: LeaseId,
) -> Result<SchedulerDecision, VoomError> {
    let row = sqlx::query(&format!(
        "SELECT {DECISION_COLS} FROM scheduler_decisions WHERE id = ?"
    ))
    .bind(i64_from_u64(decision_id))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("scheduler_decisions link reread: {e}")))?;

    let Some(row) = row else {
        return Err(VoomError::NotFound(format!(
            "scheduler_decisions id={decision_id} not found"
        )));
    };
    let decision = row_to_decision(&row)?;
    if decision
        .selected_lease_id
        .is_some_and(|existing| existing != lease_id)
    {
        return Err(VoomError::Conflict(format!(
            "scheduler_decisions id={decision_id} is already linked to lease_id={}",
            decision.selected_lease_id.map_or(0, |id| id.0)
        )));
    }
    Ok(decision)
}

#[derive(Debug)]
struct SelectedLeaseLinkFacts {
    decision_kind: String,
    outcome: String,
    decision_ticket_id: Option<i64>,
    decision_worker_id: Option<i64>,
    decision_node_id: Option<i64>,
    existing_lease_id: Option<i64>,
    lease_ticket_id: Option<i64>,
    lease_worker_id: Option<i64>,
    lease_node_id: Option<i64>,
}

fn link_facts_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SelectedLeaseLinkFacts, VoomError> {
    Ok(SelectedLeaseLinkFacts {
        decision_kind: row
            .try_get("decision_kind")
            .map_err(|e| map_row_err("scheduler_decisions link coherence", &e))?,
        outcome: row
            .try_get("outcome")
            .map_err(|e| map_row_err("scheduler_decisions link coherence", &e))?,
        decision_ticket_id: row
            .try_get("ticket_id")
            .map_err(|e| map_row_err("scheduler_decisions link coherence", &e))?,
        decision_worker_id: row
            .try_get("selected_worker_id")
            .map_err(|e| map_row_err("scheduler_decisions link coherence", &e))?,
        decision_node_id: row
            .try_get("selected_node_id")
            .map_err(|e| map_row_err("scheduler_decisions link coherence", &e))?,
        existing_lease_id: row
            .try_get("selected_lease_id")
            .map_err(|e| map_row_err("scheduler_decisions link coherence", &e))?,
        lease_ticket_id: row
            .try_get("lease_ticket_id")
            .map_err(|e| map_row_err("scheduler_decisions link coherence", &e))?,
        lease_worker_id: row
            .try_get("lease_worker_id")
            .map_err(|e| map_row_err("scheduler_decisions link coherence", &e))?,
        lease_node_id: row
            .try_get("lease_node_id")
            .map_err(|e| map_row_err("scheduler_decisions link coherence", &e))?,
    })
}

fn validate_selected_lease_link_facts(
    decision_id: u64,
    lease_id: LeaseId,
    facts: &SelectedLeaseLinkFacts,
) -> Result<(), VoomError> {
    if facts.decision_kind != SchedulerDecisionKind::LeaseAcquire.as_str()
        || facts.outcome != SchedulerDecisionOutcome::Selected.as_str()
    {
        return Err(VoomError::Conflict(format!(
            "scheduler_decisions id={} is {}/{}, not selected lease_acquire",
            decision_id, facts.decision_kind, facts.outcome
        )));
    }

    let Some(lease_ticket_id) = facts.lease_ticket_id else {
        return Err(VoomError::NotFound(format!(
            "leases id={} not found",
            lease_id.0
        )));
    };
    let Some(lease_worker_id) = facts.lease_worker_id else {
        return Err(VoomError::NotFound(format!(
            "leases id={} has no worker",
            lease_id.0
        )));
    };
    let Some(lease_node_id) = facts.lease_node_id else {
        return Err(VoomError::NotFound(format!(
            "workers id={} not found",
            u64_from_i64(lease_worker_id)
        )));
    };

    if facts.decision_ticket_id != Some(lease_ticket_id) {
        return Err(VoomError::Conflict(format!(
            "scheduler_decisions id={decision_id} ticket_id={:?} does not match lease ticket_id={}",
            facts.decision_ticket_id.map(u64_from_i64),
            u64_from_i64(lease_ticket_id)
        )));
    }
    if facts.decision_worker_id != Some(lease_worker_id) {
        return Err(VoomError::Conflict(format!(
            "scheduler_decisions id={decision_id} worker_id={:?} does not match lease worker_id={}",
            facts.decision_worker_id.map(u64_from_i64),
            u64_from_i64(lease_worker_id)
        )));
    }
    if facts.decision_node_id != Some(lease_node_id) {
        return Err(VoomError::Conflict(format!(
            "scheduler_decisions id={decision_id} node_id={:?} does not match lease node_id={}",
            facts.decision_node_id.map(u64_from_i64),
            u64_from_i64(lease_node_id)
        )));
    }
    if let Some(existing_lease_id) = facts.existing_lease_id
        && existing_lease_id != i64_from_u64(lease_id.0)
    {
        return Err(VoomError::Conflict(format!(
            "scheduler_decisions id={decision_id} is already linked to lease_id={}",
            u64_from_i64(existing_lease_id)
        )));
    }

    Ok(())
}

fn bind_decision_query<'a>(
    query: sqlx::query::Query<'a, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'a>>,
    input: &'a NewSchedulerDecision,
    now: String,
    explanation: String,
) -> sqlx::query::Query<'a, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'a>> {
    query
        .bind(now.clone())
        .bind(now.clone())
        .bind(now.clone())
        .bind(now)
        .bind(input.decision_kind.as_str())
        .bind(input.request_source.as_str())
        .bind(input.idempotency_key.as_deref())
        .bind(input.request_node_id.map(|id| i64_from_u64(id.0)))
        .bind(input.request_worker_id.map(|id| i64_from_u64(id.0)))
        .bind(input.ticket_id.map(|id| i64_from_u64(id.0)))
        .bind(input.selected_worker_id.map(|id| i64_from_u64(id.0)))
        .bind(input.selected_node_id.map(|id| i64_from_u64(id.0)))
        .bind(input.selected_lease_id.map(|id| i64_from_u64(id.0)))
        .bind(input.outcome.as_str())
        .bind(input.reason_code.as_str())
        .bind(input.summary.as_str())
        .bind(i64::from(input.candidate_count))
        .bind(input.selected_score)
        .bind(input.suppression_key.as_deref())
        .bind(explanation)
}

fn row_to_decision(row: &sqlx::sqlite::SqliteRow) -> Result<SchedulerDecision, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let updated_at: String = row
        .try_get("updated_at")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let first_seen_at: String = row
        .try_get("first_seen_at")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let last_seen_at: String = row
        .try_get("last_seen_at")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let decision_kind: String = row
        .try_get("decision_kind")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let request_source: String = row
        .try_get("request_source")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let idempotency_key: Option<String> = row
        .try_get("idempotency_key")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let request_node_id: Option<i64> = row
        .try_get("request_node_id")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let request_worker_id: Option<i64> = row
        .try_get("request_worker_id")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let ticket_id: Option<i64> = row
        .try_get("ticket_id")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let selected_worker_id: Option<i64> = row
        .try_get("selected_worker_id")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let selected_node_id: Option<i64> = row
        .try_get("selected_node_id")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let selected_lease_id: Option<i64> = row
        .try_get("selected_lease_id")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let outcome: String = row
        .try_get("outcome")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let reason_code: String = row
        .try_get("reason_code")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let summary: String = row
        .try_get("summary")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let candidate_count: i64 = row
        .try_get("candidate_count")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let selected_score: Option<i64> = row
        .try_get("selected_score")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let suppressed_count: i64 = row
        .try_get("suppressed_count")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let suppression_key: Option<String> = row
        .try_get("suppression_key")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;
    let explanation_json: String = row
        .try_get("explanation_json")
        .map_err(|e| map_row_err("scheduler_decisions", &e))?;

    Ok(SchedulerDecision {
        id: u64_from_i64(id),
        created_at: parse_iso8601(&created_at)?,
        updated_at: parse_iso8601(&updated_at)?,
        first_seen_at: parse_iso8601(&first_seen_at)?,
        last_seen_at: parse_iso8601(&last_seen_at)?,
        decision_kind: SchedulerDecisionKind::parse(&decision_kind)?,
        request_source: SchedulerRequestSource::parse(&request_source)?,
        idempotency_key,
        request_node_id: request_node_id.map(|id| NodeId(u64_from_i64(id))),
        request_worker_id: request_worker_id.map(|id| WorkerId(u64_from_i64(id))),
        ticket_id: ticket_id.map(|id| TicketId(u64_from_i64(id))),
        selected_worker_id: selected_worker_id.map(|id| WorkerId(u64_from_i64(id))),
        selected_node_id: selected_node_id.map(|id| NodeId(u64_from_i64(id))),
        selected_lease_id: selected_lease_id.map(|id| LeaseId(u64_from_i64(id))),
        outcome: SchedulerDecisionOutcome::parse(&outcome)?,
        reason_code: SchedulerReasonCode::parse(&reason_code)?,
        summary,
        candidate_count: u32_from_i64(candidate_count)?,
        selected_score,
        suppressed_count: u32_from_i64(suppressed_count)?,
        suppression_key,
        explanation: serde_json::from_str(&explanation_json).map_err(|e| {
            VoomError::Database(format!("scheduler_decisions explanation_json: {e}"))
        })?,
    })
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
        node_id: NodeId(u64_from_i64(node_id)),
        max_parallel_leases: u32_from_i64(max_parallel_leases)?,
        created_at: parse_iso8601(&created_at)?,
        updated_at: parse_iso8601(&updated_at)?,
    })
}

#[cfg(test)]
#[path = "scheduler_decisions_test.rs"]
mod tests;
