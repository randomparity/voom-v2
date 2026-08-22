//! Remote execution use cases for node-owned workers.

use secrecy::SecretString;
use serde_json::Value as JsonValue;
use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{
    ArtifactAccessMode, ErrorCode, FailureClass, LeaseId, NodeId, NodeIncarnationEndReason,
    NodeIncarnationId, NodeIncarnationStatus, OperationKind, ScanSessionId, TicketId, VoomError,
    WorkerId, owner_access_evidence::OwnerAccessEvidence,
};
use voom_events::payload::ScanSessionLifecyclePayload;
use voom_events::{Event, SubjectType};
use voom_store::repo::execution::remote_idempotency::RemoteMutationReplay;
use voom_store::repo::scan::sessions::ScanSession;

use crate::ControlPlane;

use super::{append_event, commit_tx};

mod acquire;
mod activation;
mod complete;
mod fail;
mod heartbeat;
mod recover;

#[cfg(test)]
use acquire::{
    capacity_suppression_key, scheduler_reason, score_remote_candidates, suppression_key,
};

pub(super) const ROUTE_ACQUIRE: &str = "POST /v1/execution/lease/acquire";

pub(crate) use recover::validate_remote_node_live;

#[derive(Debug, Clone)]
pub struct RemoteActivateInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
    pub incarnation_id: NodeIncarnationId,
    pub workers: Vec<RemoteWorkerDeclaration>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteWorkerDeclaration {
    pub logical_name: String,
    pub operations: Vec<OperationKind>,
    pub artifact_access: Vec<ArtifactAccessMode>,
    pub max_parallel: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivatedWorker {
    pub logical_name: String,
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteActivateOutcome {
    pub node_id: NodeId,
    pub node_epoch: u64,
    pub incarnation_id: NodeIncarnationId,
    pub heartbeat_ttl_seconds: u32,
    pub workers: Vec<ActivatedWorker>,
}

#[derive(Debug, Clone)]
pub struct RemoteDeactivateInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
    pub incarnation_id: NodeIncarnationId,
    pub reason: NodeIncarnationEndReason,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteDeactivateOutcome {
    pub node_id: NodeId,
    pub node_epoch: u64,
    pub incarnation_id: NodeIncarnationId,
    pub status: NodeIncarnationStatus,
    pub reason: NodeIncarnationEndReason,
    pub retired_worker_ids: Vec<WorkerId>,
}

#[derive(Debug, Clone)]
pub struct RemoteNodeHeartbeatInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
    pub idempotency_key: String,
    pub request_hash: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteNodeHeartbeatOutcome {
    pub node_id: NodeId,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct RemoteAcquireInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
    pub worker_id: WorkerId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub lease_ttl_seconds: i64,
}

#[derive(Debug, Clone, PartialEq)]
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
#[serde(tag = "outcome", rename_all = "snake_case")]
enum RemoteAcquireOutcomeWire {
    Idle(RemoteAcquireIdleWire),
    NoCandidate(RemoteAcquireNoCandidateWire),
    Leased(RemoteLeaseDispatch),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteAcquireIdleWire {
    worker_id: WorkerId,
    scheduler_decision_id: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteAcquireNoCandidateWire {
    worker_id: WorkerId,
    scheduler_decision_id: u64,
}

impl serde::Serialize for RemoteAcquireOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let wire = RemoteAcquireOutcomeWire::from(self.clone());
        serde::Serialize::serialize(&wire, serializer)
    }
}

impl<'de> serde::Deserialize<'de> for RemoteAcquireOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = <RemoteAcquireOutcomeWire as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Self::from(wire))
    }
}

impl From<RemoteAcquireOutcome> for RemoteAcquireOutcomeWire {
    fn from(outcome: RemoteAcquireOutcome) -> Self {
        match outcome {
            RemoteAcquireOutcome::Idle {
                worker_id,
                scheduler_decision_id,
            } => Self::Idle(RemoteAcquireIdleWire {
                worker_id,
                scheduler_decision_id,
            }),
            RemoteAcquireOutcome::NoCandidate {
                worker_id,
                scheduler_decision_id,
            } => Self::NoCandidate(RemoteAcquireNoCandidateWire {
                worker_id,
                scheduler_decision_id,
            }),
            RemoteAcquireOutcome::Leased(dispatch) => Self::Leased(dispatch),
        }
    }
}

impl From<RemoteAcquireOutcomeWire> for RemoteAcquireOutcome {
    fn from(wire: RemoteAcquireOutcomeWire) -> Self {
        match wire {
            RemoteAcquireOutcomeWire::Idle(RemoteAcquireIdleWire {
                worker_id,
                scheduler_decision_id,
            }) => Self::Idle {
                worker_id,
                scheduler_decision_id,
            },
            RemoteAcquireOutcomeWire::NoCandidate(RemoteAcquireNoCandidateWire {
                worker_id,
                scheduler_decision_id,
            }) => Self::NoCandidate {
                worker_id,
                scheduler_decision_id,
            },
            RemoteAcquireOutcomeWire::Leased(dispatch) => Self::Leased(dispatch),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct RemoteArtifactAccessPlan {
    pub id: u64,
    pub owner_node_id: Option<u64>,
    pub access_evidence: Option<OwnerAccessEvidence>,
}

/// The exact consumption evidence a worker must echo to complete a lease
/// (issue #479, ADR 0073). Unknown fields are rejected: the legacy
/// synthetic/shared-mount echo shape stopped validating with #477.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatedArtifactAccess {
    pub validated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_node_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_evidence: Option<OwnerAccessEvidence>,
}

/// Regression for the removed-field rule (AGENTS.md): removing a serialized
/// field removes the accepted input too. The legacy synthetic/shared-mount
/// plan fields must fail decode on every mirror of this contract.
#[cfg(test)]
#[test]
fn legacy_artifact_access_plan_fields_rejected_on_decode() {
    for legacy in [
        r#"{"id":1,"input_handles":["handle:input:synthetic"],
            "output_handles":["handle:output:synthetic"],
            "selected_access_mode":"shared_mount"}"#,
        r#"{"id":1,"owner_node_id":null,"access_evidence":null,
            "selected_access_mode":"shared_mount"}"#,
    ] {
        let err = serde_json::from_str::<RemoteArtifactAccessPlan>(legacy).unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }
}

#[derive(Debug, Clone)]
pub struct RemoteLeaseHeartbeatInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
    pub worker_id: WorkerId,
    pub lease_id: LeaseId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub lease_ttl_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteLeaseHeartbeatOutcome {
    pub lease_id: LeaseId,
    pub worker_id: WorkerId,
    pub ttl_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct RemoteCompleteInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
    pub worker_id: WorkerId,
    pub lease_id: LeaseId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub result: JsonValue,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub incarnation_id: NodeIncarnationId,
    pub worker_id: WorkerId,
    pub lease_id: LeaseId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub reason: String,
    pub class: FailureClass,
    pub evidence: JsonValue,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteFailOutcome {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub artifact_access_plan: RemoteArtifactAccessPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRecoverReport {
    pub stale_nodes: Vec<NodeId>,
    pub stale_scan_sessions: Vec<ScanSessionId>,
    pub expired_leases: Vec<LeaseId>,
    pub requeued_tickets: Vec<TicketId>,
    pub failed_tickets: Vec<TicketId>,
}

async fn append_scan_stale_events_in_tx(
    control_plane: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    sessions: &[ScanSession],
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    for session in sessions {
        append_event(
            &control_plane.events,
            tx,
            SubjectType::ScanSession,
            Some(session.id.0),
            now,
            Event::ScanSessionStale(ScanSessionLifecyclePayload {
                scan_session_id: session.id,
                storage_root_id: session.storage_root_id,
                status: session.status,
            }),
        )
        .await?;
    }
    Ok(())
}

impl ControlPlane {
    /// Finish a replay branch: commit the (read-only) reservation transaction
    /// and return the stored outcome. A stored `Error` replay is already
    /// terminal and returned as-is. An `Ok { data }` replay is decoded with
    /// `decode`; if that fails the stored result no longer matches the running
    /// binary, so the row is repointed to a terminal `Error` in the same
    /// transaction — the already-executed mutation is never re-run, and future
    /// replays return a deterministic error instead of re-failing decode.
    pub(crate) async fn finish_replay_in_tx<T, F>(
        &self,
        mut tx: Transaction<'_, Sqlite>,
        slot: ReplaySlot,
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
                            &slot.idempotency_key,
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

pub(super) fn route_node_activate(node_id: NodeId) -> String {
    format!("POST /v1/execution/node/{}/activate", node_id.0)
}

pub(super) fn route_node_deactivate(node_id: NodeId) -> String {
    format!("POST /v1/execution/node/{}/deactivate", node_id.0)
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

pub(crate) fn is_remote_replayable_error(err: &VoomError) -> bool {
    matches!(
        err.error_code(),
        ErrorCode::Conflict | ErrorCode::ConfigInvalid | ErrorCode::NotFound
    )
}

/// Identity of the idempotency row a replay decodes from — the tuple
/// `repoint_completed_replay_in_tx` matches on. Owns `route_key` because some
/// routes derive it (`route_lease_*`) rather than holding a borrowable field.
pub(crate) struct ReplaySlot {
    pub(crate) node_id: NodeId,
    pub(crate) route_key: String,
    pub(crate) worker_id: Option<WorkerId>,
    pub(crate) idempotency_key: String,
}

/// Maps a remote-execution input to the idempotency row it replays from, so
/// the replay branch and any poison-repoint target the same row the
/// reservation used.
pub(super) trait ReplayRoute {
    fn replay_slot(&self) -> ReplaySlot;
}

impl ReplayRoute for RemoteAcquireInput {
    fn replay_slot(&self) -> ReplaySlot {
        ReplaySlot {
            node_id: self.node_id,
            route_key: ROUTE_ACQUIRE.to_owned(),
            worker_id: Some(self.worker_id),
            idempotency_key: incarnation_replay_key(self.incarnation_id, &self.idempotency_key),
        }
    }
}

impl ReplayRoute for RemoteActivateInput {
    fn replay_slot(&self) -> ReplaySlot {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_node_activate(self.node_id),
            worker_id: None,
            idempotency_key: self.idempotency_key.clone(),
        }
    }
}

impl ReplayRoute for RemoteDeactivateInput {
    fn replay_slot(&self) -> ReplaySlot {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_node_deactivate(self.node_id),
            worker_id: None,
            idempotency_key: self.idempotency_key.clone(),
        }
    }
}

impl ReplayRoute for RemoteNodeHeartbeatInput {
    fn replay_slot(&self) -> ReplaySlot {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_node_heartbeat(self.node_id),
            worker_id: None,
            idempotency_key: incarnation_replay_key(self.incarnation_id, &self.idempotency_key),
        }
    }
}

impl ReplayRoute for RemoteLeaseHeartbeatInput {
    fn replay_slot(&self) -> ReplaySlot {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_lease_heartbeat(self.lease_id),
            worker_id: Some(self.worker_id),
            idempotency_key: incarnation_replay_key(self.incarnation_id, &self.idempotency_key),
        }
    }
}

impl ReplayRoute for RemoteCompleteInput {
    fn replay_slot(&self) -> ReplaySlot {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_lease_complete(self.lease_id),
            worker_id: Some(self.worker_id),
            idempotency_key: incarnation_replay_key(self.incarnation_id, &self.idempotency_key),
        }
    }
}

impl ReplayRoute for RemoteFailInput {
    fn replay_slot(&self) -> ReplaySlot {
        ReplaySlot {
            node_id: self.node_id,
            route_key: route_lease_fail(self.lease_id),
            worker_id: Some(self.worker_id),
            idempotency_key: incarnation_replay_key(self.incarnation_id, &self.idempotency_key),
        }
    }
}

pub(crate) fn incarnation_replay_key(id: NodeIncarnationId, key: &str) -> String {
    format!("{id}:{key}")
}

pub(super) fn decode_acquire_replay(data: JsonValue) -> Result<RemoteAcquireOutcome, VoomError> {
    decode_replay(data, "remote acquire")
}

pub(crate) fn decode_replay<T>(data: JsonValue, label: &str) -> Result<T, VoomError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(data)
        .map_err(|e| VoomError::Internal(format!("{label} replay decode: {e}")))
}

fn replay_error(code: &str, message: String) -> VoomError {
    match code {
        "CONFLICT" => VoomError::Conflict(message),
        "CONFIG_INVALID" => VoomError::Config(message),
        "NOT_FOUND" => VoomError::NotFound(message),
        _ => VoomError::Internal(format!("remote replay error {code}: {message}")),
    }
}

pub(crate) fn remote_error_message(err: &VoomError) -> String {
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
