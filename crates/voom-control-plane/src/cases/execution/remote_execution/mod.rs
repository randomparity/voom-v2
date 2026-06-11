//! Remote execution use cases for node-owned workers.

use secrecy::SecretString;
use serde_json::Value as JsonValue;
use sqlx::{Sqlite, Transaction};
use time::Duration;
use voom_core::{ErrorCode, FailureClass, LeaseId, NodeId, TicketId, VoomError, WorkerId};
use voom_store::repo::artifact_access_plans::{ArtifactAccessMode, ArtifactAccessPlanStatus};
use voom_store::repo::remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteMutationReplay,
};

use crate::ControlPlane;

use super::{begin_immediate_tx, commit_tx};

mod acquire;
mod complete;
mod recover;

#[cfg(test)]
pub(crate) use acquire::{
    capacity_suppression_key, scheduler_reason, score_remote_candidates, suppression_key,
};

pub(super) const ROUTE_ACQUIRE: &str = "POST /v1/execution/lease/acquire";

#[derive(Debug, Clone)]
pub struct RemoteNodeHeartbeatInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteNodeHeartbeatOutcome {
    pub node_id: NodeId,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct RemoteAcquireInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub worker_id: WorkerId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub lease_ttl_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RemoteAcquireOutcome {
    Idle {
        worker_id: WorkerId,
        scheduler_decision_id: u64,
    },
    NoCandidate {
        worker_id: WorkerId,
        scheduler_decision_id: u64,
    },
    Leased(RemoteLeaseDispatch),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteLeaseDispatch {
    pub lease_id: LeaseId,
    pub scheduler_decision_id: u64,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub operation: String,
    pub dispatch_payload: JsonValue,
    pub lease_ttl_seconds: i64,
    pub heartbeat_after_seconds: i64,
    pub artifact_access_plan: RemoteArtifactAccessPlan,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteArtifactAccessPlan {
    pub id: u64,
    pub input_handles: Vec<String>,
    pub output_handles: Vec<String>,
    pub selected_access_mode: ArtifactAccessMode,
}

#[derive(Debug, Clone)]
pub struct RemoteLeaseHeartbeatInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub worker_id: WorkerId,
    pub lease_id: LeaseId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub lease_ttl_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteLeaseHeartbeatOutcome {
    pub lease_id: LeaseId,
    pub worker_id: WorkerId,
    pub ttl_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct RemoteCompleteInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub worker_id: WorkerId,
    pub lease_id: LeaseId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub result: JsonValue,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteCompleteOutcome {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub artifact_access_plan: RemoteArtifactAccessPlan,
}

#[derive(Debug, Clone)]
pub struct RemoteFailInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub worker_id: WorkerId,
    pub lease_id: LeaseId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub reason: String,
    pub class: FailureClass,
    pub evidence: JsonValue,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteFailOutcome {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub artifact_access_plan: RemoteArtifactAccessPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRecoverReport {
    pub stale_nodes: Vec<NodeId>,
    pub expired_leases: Vec<LeaseId>,
    pub requeued_tickets: Vec<TicketId>,
    pub failed_tickets: Vec<TicketId>,
}

struct RemoteFailPrepared {
    plan_id: u64,
    status: ArtifactAccessPlanStatus,
}

impl ControlPlane {
    /// Record a remote node heartbeat.
    ///
    /// # Errors
    /// Returns authentication, idempotency, retired-node, or heartbeat errors.
    pub async fn remote_node_heartbeat(
        &self,
        input: RemoteNodeHeartbeatInput,
    ) -> Result<RemoteNodeHeartbeatOutcome, VoomError> {
        let now = self.clock().now();
        let route_key = route_node_heartbeat(input.node_id);
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let auth = self
            .verify_remote_node_token_in_tx(&mut tx, input.node_id, &input.token)
            .await?;

        match self
            .remote_idempotency
            .reserve_or_replay_in_tx(
                &mut tx,
                RemoteIdempotencyInput {
                    node_id: input.node_id,
                    route_key: route_key.clone(),
                    worker_id: None,
                    idempotency_key: input.idempotency_key.clone(),
                    request_hash: input.request_hash.clone(),
                    created_at: now,
                },
            )
            .await?
        {
            IdempotencyOutcome::Reserved => {}
            IdempotencyOutcome::Replay(replay) => {
                return self
                    .finish_replay_in_tx(tx, input.replay_slot(), replay, |data| {
                        decode_replay::<RemoteNodeHeartbeatOutcome>(data, "remote node heartbeat")
                    })
                    .await;
            }
        }

        if let Err(err) = recover::validate_remote_node_live(&auth, input.node_id, now, false) {
            self.complete_remote_error_in_tx(
                &mut tx,
                input.node_id,
                &route_key,
                None,
                &input.idempotency_key,
                &err,
            )
            .await?;
            commit_tx(tx).await?;
            return Err(err);
        }

        let node = self
            .heartbeat_node_in_tx(&mut tx, input.node_id, now)
            .await?;
        let outcome = RemoteNodeHeartbeatOutcome {
            node_id: node.id,
            status: node.status.as_str().to_owned(),
        };
        self.complete_remote_ok_in_tx(
            &mut tx,
            input.node_id,
            &route_key,
            None,
            &input.idempotency_key,
            &outcome,
        )
        .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    /// Finish a replay branch: commit the (read-only) reservation transaction
    /// and return the stored outcome. A stored `Error` replay is already
    /// terminal and returned as-is. An `Ok { data }` replay is decoded with
    /// `decode`; if that fails the stored result no longer matches the running
    /// binary, so the row is repointed to a terminal `Error` in the same
    /// transaction — the already-executed mutation is never re-run, and future
    /// replays return a deterministic error instead of re-failing decode.
    async fn finish_replay_in_tx<T, F>(
        &self,
        mut tx: Transaction<'_, Sqlite>,
        slot: ReplaySlot<'_>,
        replay: RemoteMutationReplay,
        decode: F,
    ) -> Result<T, VoomError>
    where
        F: FnOnce(JsonValue) -> Result<T, VoomError>,
    {
        match replay {
            RemoteMutationReplay::Error { code, message } => {
                commit_tx(tx).await?;
                Err(replay_error(&code, message))
            }
            RemoteMutationReplay::Ok { data } => match decode(data) {
                Ok(out) => {
                    commit_tx(tx).await?;
                    Ok(out)
                }
                Err(err) => {
                    // The stored result of an already-completed operation no
                    // longer decodes (schema drift or corruption). Repointing it
                    // to a terminal Error masks a success as a permanent failure,
                    // so surface it: an operator needs to know a completed
                    // operation became unreadable.
                    tracing::warn!(
                        node_id = slot.node_id.0,
                        route_key = %slot.route_key,
                        idempotency_key = %slot.idempotency_key,
                        error = %err,
                        "idempotency replay result is unreadable; repointing row to a terminal error"
                    );
                    self.remote_idempotency
                        .repoint_completed_replay_in_tx(
                            &mut tx,
                            slot.node_id,
                            &slot.route_key,
                            slot.worker_id,
                            slot.idempotency_key,
                            RemoteMutationReplay::Error {
                                code: err.code().to_owned(),
                                message: remote_error_message(&err),
                            },
                        )
                        .await?;
                    commit_tx(tx).await?;
                    Err(err)
                }
            },
        }
    }

    /// Heartbeat a held remote lease without emitting audit events.
    ///
    /// # Errors
    /// Returns authentication, idempotency, ownership, or lease heartbeat errors.
    pub async fn remote_lease_heartbeat(
        &self,
        input: RemoteLeaseHeartbeatInput,
    ) -> Result<RemoteLeaseHeartbeatOutcome, VoomError> {
        let now = self.clock().now();
        let route_key = route_lease_heartbeat(input.lease_id);
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let auth = self
            .verify_remote_node_token_in_tx(&mut tx, input.node_id, &input.token)
            .await?;

        match self
            .remote_idempotency
            .reserve_or_replay_in_tx(
                &mut tx,
                RemoteIdempotencyInput {
                    node_id: input.node_id,
                    route_key: route_key.clone(),
                    worker_id: Some(input.worker_id),
                    idempotency_key: input.idempotency_key.clone(),
                    request_hash: input.request_hash.clone(),
                    created_at: now,
                },
            )
            .await?
        {
            IdempotencyOutcome::Reserved => {}
            IdempotencyOutcome::Replay(replay) => {
                return self
                    .finish_replay_in_tx(tx, input.replay_slot(), replay, |data| {
                        decode_replay::<RemoteLeaseHeartbeatOutcome>(data, "remote lease heartbeat")
                    })
                    .await;
            }
        }

        if let Err(err) = recover::validate_remote_node_live(&auth, input.node_id, now, false) {
            self.complete_remote_error_in_tx(
                &mut tx,
                input.node_id,
                &route_key,
                Some(input.worker_id),
                &input.idempotency_key,
                &err,
            )
            .await?;
            commit_tx(tx).await?;
            return Err(err);
        }

        let outcome = match self
            .remote_lease_heartbeat_preflight_in_tx(&mut tx, &input)
            .await
        {
            Ok(()) => {
                self.remote_lease_heartbeat_mutation_in_tx(&mut tx, &input, now)
                    .await?
            }
            Err(err) => {
                if !is_remote_replayable_error(&err) {
                    return Err(err);
                }
                self.complete_remote_error_in_tx(
                    &mut tx,
                    input.node_id,
                    &route_key,
                    Some(input.worker_id),
                    &input.idempotency_key,
                    &err,
                )
                .await?;
                commit_tx(tx).await?;
                return Err(err);
            }
        };
        self.complete_remote_ok_in_tx(
            &mut tx,
            input.node_id,
            &route_key,
            Some(input.worker_id),
            &input.idempotency_key,
            &outcome,
        )
        .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn remote_lease_heartbeat_preflight_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteLeaseHeartbeatInput,
    ) -> Result<(), VoomError> {
        recover::require_positive_ttl(input.lease_ttl_seconds)?;
        let worker = self
            .workers
            .node_owned_worker_in_tx(tx, input.worker_id, input.node_id)
            .await?;
        recover::require_remote_worker(&worker)?;
        self.leases
            .get_held_for_worker_in_tx(tx, input.lease_id, input.worker_id)
            .await?;
        Ok(())
    }

    async fn remote_lease_heartbeat_mutation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteLeaseHeartbeatInput,
        now: time::OffsetDateTime,
    ) -> Result<RemoteLeaseHeartbeatOutcome, VoomError> {
        let lease = self
            .heartbeat_lease_in_tx(
                tx,
                input.lease_id,
                Duration::seconds(input.lease_ttl_seconds),
                now,
            )
            .await?;
        Ok(RemoteLeaseHeartbeatOutcome {
            lease_id: lease.id,
            worker_id: lease.worker_id,
            ttl_seconds: lease.ttl_seconds,
        })
    }

    /// Fail a held remote lease and update its selected artifact plan.
    ///
    /// # Errors
    /// Returns authentication, idempotency, ownership, artifact plan, or
    /// lease-failure errors.
    pub async fn remote_fail(
        &self,
        input: RemoteFailInput,
    ) -> Result<RemoteFailOutcome, VoomError> {
        let now = self.clock().now();
        let route_key = route_lease_fail(input.lease_id);
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let auth = self
            .verify_remote_node_token_in_tx(&mut tx, input.node_id, &input.token)
            .await?;

        match self
            .remote_idempotency
            .reserve_or_replay_in_tx(
                &mut tx,
                RemoteIdempotencyInput {
                    node_id: input.node_id,
                    route_key: route_key.clone(),
                    worker_id: Some(input.worker_id),
                    idempotency_key: input.idempotency_key.clone(),
                    request_hash: input.request_hash.clone(),
                    created_at: now,
                },
            )
            .await?
        {
            IdempotencyOutcome::Reserved => {}
            IdempotencyOutcome::Replay(replay) => {
                return self
                    .finish_replay_in_tx(tx, input.replay_slot(), replay, |data| {
                        decode_replay::<RemoteFailOutcome>(data, "remote fail")
                    })
                    .await;
            }
        }

        if let Err(err) = recover::validate_remote_node_live(&auth, input.node_id, now, false) {
            self.complete_remote_error_in_tx(
                &mut tx,
                input.node_id,
                &route_key,
                Some(input.worker_id),
                &input.idempotency_key,
                &err,
            )
            .await?;
            commit_tx(tx).await?;
            return Err(err);
        }

        let prepared = match self.remote_fail_preflight_in_tx(&mut tx, &input).await {
            Ok(prepared) => prepared,
            Err(err) => {
                if !is_remote_replayable_error(&err) {
                    return Err(err);
                }
                self.complete_remote_error_in_tx(
                    &mut tx,
                    input.node_id,
                    &route_key,
                    Some(input.worker_id),
                    &input.idempotency_key,
                    &err,
                )
                .await?;
                commit_tx(tx).await?;
                return Err(err);
            }
        };
        let outcome = self
            .remote_fail_mutation_in_tx(&mut tx, &input, &route_key, prepared, now)
            .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn remote_fail_preflight_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteFailInput,
    ) -> Result<RemoteFailPrepared, VoomError> {
        let worker = self
            .workers
            .node_owned_worker_in_tx(tx, input.worker_id, input.node_id)
            .await?;
        recover::require_remote_worker(&worker)?;
        self.leases
            .get_held_for_worker_in_tx(tx, input.lease_id, input.worker_id)
            .await?;
        let plan = self
            .artifact_access_plans
            .get_by_lease_in_tx(tx, input.lease_id)
            .await?
            .ok_or_else(|| {
                VoomError::Conflict(format!(
                    "remote fail rejected: lease {} has no artifact access plan",
                    input.lease_id
                ))
            })?;
        Ok(RemoteFailPrepared {
            plan_id: plan.id,
            status: complete::artifact_failure_status(input.class, &input.reason),
        })
    }

    async fn remote_fail_mutation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteFailInput,
        route_key: &str,
        prepared: RemoteFailPrepared,
        now: time::OffsetDateTime,
    ) -> Result<RemoteFailOutcome, VoomError> {
        let marked = self
            .artifact_access_plans
            .mark_status_in_tx(
                tx,
                prepared.plan_id,
                prepared.status,
                Some(input.reason.clone()),
                input.evidence.clone(),
                now,
            )
            .await?;
        let failed = self
            .fail_lease_in_tx(tx, input.lease_id, input.reason.clone(), input.class, now)
            .await?;
        let outcome = RemoteFailOutcome {
            lease_id: failed.id,
            ticket_id: failed.ticket_id,
            worker_id: failed.worker_id,
            artifact_access_plan: acquire::remote_plan(&marked),
        };
        self.complete_remote_ok_in_tx(
            tx,
            input.node_id,
            route_key,
            Some(input.worker_id),
            &input.idempotency_key,
            &outcome,
        )
        .await?;
        Ok(outcome)
    }
}

pub(super) fn route_lease_complete(lease_id: LeaseId) -> String {
    format!("POST /v1/execution/lease/{}/complete", lease_id.0)
}

fn route_node_heartbeat(node_id: NodeId) -> String {
    format!("POST /v1/execution/node/{}/heartbeat", node_id.0)
}

fn route_lease_heartbeat(lease_id: LeaseId) -> String {
    format!("POST /v1/execution/lease/{}/heartbeat", lease_id.0)
}

fn route_lease_fail(lease_id: LeaseId) -> String {
    format!("POST /v1/execution/lease/{}/fail", lease_id.0)
}

pub(super) fn is_remote_replayable_error(err: &VoomError) -> bool {
    matches!(
        err.error_code(),
        ErrorCode::Conflict | ErrorCode::ConfigInvalid | ErrorCode::NotFound
    )
}

/// Identity of the idempotency row a replay decodes from — the tuple
/// `repoint_completed_replay_in_tx` matches on. Owns `route_key` because some
/// routes derive it (`route_lease_*`) rather than holding a borrowable field.
pub(super) struct ReplaySlot<'a> {
    node_id: NodeId,
    route_key: String,
    worker_id: Option<WorkerId>,
    idempotency_key: &'a str,
}

/// Maps a remote-execution input to the idempotency row it replays from, so
/// the replay branch and any poison-repoint target the same row the
/// reservation used.
pub(super) trait ReplayRoute {
    fn replay_slot(&self) -> ReplaySlot<'_>;
}

impl ReplayRoute for RemoteAcquireInput {
    fn replay_slot(&self) -> ReplaySlot<'_> {
        ReplaySlot {
            node_id: self.node_id,
            route_key: ROUTE_ACQUIRE.to_owned(),
            worker_id: Some(self.worker_id),
            idempotency_key: &self.idempotency_key,
        }
    }
}

impl ReplayRoute for RemoteNodeHeartbeatInput {
    fn replay_slot(&self) -> ReplaySlot<'_> {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_node_heartbeat(self.node_id),
            worker_id: None,
            idempotency_key: &self.idempotency_key,
        }
    }
}

impl ReplayRoute for RemoteLeaseHeartbeatInput {
    fn replay_slot(&self) -> ReplaySlot<'_> {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_lease_heartbeat(self.lease_id),
            worker_id: Some(self.worker_id),
            idempotency_key: &self.idempotency_key,
        }
    }
}

impl ReplayRoute for RemoteCompleteInput {
    fn replay_slot(&self) -> ReplaySlot<'_> {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_lease_complete(self.lease_id),
            worker_id: Some(self.worker_id),
            idempotency_key: &self.idempotency_key,
        }
    }
}

impl ReplayRoute for RemoteFailInput {
    fn replay_slot(&self) -> ReplaySlot<'_> {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_lease_fail(self.lease_id),
            worker_id: Some(self.worker_id),
            idempotency_key: &self.idempotency_key,
        }
    }
}

pub(super) fn decode_acquire_replay(data: JsonValue) -> Result<RemoteAcquireOutcome, VoomError> {
    let data = acquire_replay_with_legacy_decision_id(data);
    serde_json::from_value(data)
        .map_err(|e| VoomError::Internal(format!("remote acquire replay decode: {e}")))
}

pub(super) fn decode_replay<T>(data: JsonValue, label: &str) -> Result<T, VoomError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(data)
        .map_err(|e| VoomError::Internal(format!("{label} replay decode: {e}")))
}

fn acquire_replay_with_legacy_decision_id(mut data: JsonValue) -> JsonValue {
    let Some(outcome) = data.get("outcome").and_then(JsonValue::as_str) else {
        return data;
    };
    if !matches!(outcome, "idle" | "no_candidate" | "leased") {
        return data;
    }
    let Some(object) = data.as_object_mut() else {
        return data;
    };
    object
        .entry("scheduler_decision_id")
        .or_insert(JsonValue::from(0_u64));
    data
}

fn replay_error(code: &str, message: String) -> VoomError {
    match code {
        "CONFLICT" => VoomError::Conflict(message),
        "CONFIG_INVALID" => VoomError::Config(message),
        "NOT_FOUND" => VoomError::NotFound(message),
        _ => VoomError::Internal(format!("remote replay error {code}: {message}")),
    }
}

pub(super) fn remote_error_message(err: &VoomError) -> String {
    match err {
        VoomError::Conflict(message)
        | VoomError::Config(message)
        | VoomError::NotFound(message) => message.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
