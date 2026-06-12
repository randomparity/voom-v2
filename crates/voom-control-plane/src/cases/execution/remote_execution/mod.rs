//! Remote execution use cases for node-owned workers.

use secrecy::SecretString;
use serde_json::Value as JsonValue;
use sqlx::{Sqlite, Transaction};
use voom_core::{ErrorCode, FailureClass, LeaseId, NodeId, TicketId, VoomError, WorkerId};
use voom_store::repo::artifact_access_plans::ArtifactAccessMode;
use voom_store::repo::remote_idempotency::RemoteMutationReplay;

use crate::ControlPlane;

use super::commit_tx;

mod acquire;
mod complete;
mod fail;
mod heartbeat;
mod recover;

#[cfg(test)]
use acquire::{
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

impl ControlPlane {
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
}

pub(super) fn route_lease_complete(lease_id: LeaseId) -> String {
    format!("POST /v1/execution/lease/{}/complete", lease_id.0)
}

pub(super) fn route_node_heartbeat(node_id: NodeId) -> String {
    format!("POST /v1/execution/node/{}/heartbeat", node_id.0)
}

pub(super) fn route_lease_heartbeat(lease_id: LeaseId) -> String {
    format!("POST /v1/execution/lease/{}/heartbeat", lease_id.0)
}

pub(super) fn route_lease_fail(lease_id: LeaseId) -> String {
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
