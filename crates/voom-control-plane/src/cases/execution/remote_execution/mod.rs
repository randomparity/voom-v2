//! Remote execution use cases for node-owned workers.

use std::collections::HashMap;

use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value as JsonValue, json};
use sqlx::{Row, Sqlite, Transaction};
use time::{Duration, OffsetDateTime};
use voom_core::{
    ErrorCode, FailureClass, LeaseId, NodeId, TicketId, TicketOperation, VoomError, WorkerId,
};
use voom_scheduler::{
    NodeCandidate, SCORING_VERSION, SchedulerCandidate, SchedulerScorer, ScoreDecision,
    ScoreOutcome, ScoreReasonCode, TicketCandidate, WorkerCandidate,
};
use voom_store::repo::artifact_access_plans::{
    ArtifactAccessMode, ArtifactAccessPlan, ArtifactAccessPlanStatus, NewArtifactAccessPlan,
};
use voom_store::repo::leases::NewLease;
use voom_store::repo::nodes::{NodeAuthRecord, NodeKind, NodeStatus};
use voom_store::repo::remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteMutationReplay,
};
use voom_store::repo::scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecision, SchedulerDecisionKind, SchedulerDecisionOutcome,
    SchedulerReasonCode as StoreSchedulerReasonCode, SchedulerRequestSource,
};
use voom_store::repo::tickets::Ticket;
use voom_store::repo::workers::{Worker, WorkerKind, WorkerOperationEligibility};

use crate::ControlPlane;
use crate::node_auth::verify_node_token;

use super::{begin_immediate_tx, commit_tx};

const ROUTE_ACQUIRE: &str = "POST /v1/execution/lease/acquire";

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

#[expect(
    clippy::large_enum_variant,
    reason = "Task 3 carries the selected scheduler decision through prepared state for lease linking"
)]
enum RemoteAcquirePrepared {
    Idle(RemoteAcquireOutcome),
    NoCandidate(RemoteAcquireOutcome),
    Leased {
        ticket: Ticket,
        eligibility: WorkerOperationEligibility,
        scheduler_decision: SchedulerDecision,
        selected_access_mode: ArtifactAccessMode,
    },
}

#[derive(Debug)]
struct RemoteAcquireCandidateSet {
    tickets: Vec<Ticket>,
    candidates: Vec<SchedulerCandidate>,
    eligibility_by_operation: HashMap<TicketOperation, WorkerOperationEligibility>,
}

#[derive(Debug, Clone, Copy)]
struct SelectedCapacityFull<'a> {
    reason_code: StoreSchedulerReasonCode,
    selected_candidate: &'a SchedulerCandidate,
    observed_active: u32,
    observed_limit: u32,
}

struct RemoteCompletePrepared {
    plan_id: u64,
    evidence: JsonValue,
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

        if let Err(err) = validate_remote_node_live(&auth, input.node_id, now, false) {
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

    /// Acquire the next ready ticket for a node-owned remote worker.
    ///
    /// # Errors
    /// Returns authentication, idempotency, eligibility, lease, or artifact
    /// access plan errors.
    pub async fn remote_acquire(
        &self,
        input: RemoteAcquireInput,
    ) -> Result<RemoteAcquireOutcome, VoomError> {
        let now = self.clock().now();
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
                    route_key: ROUTE_ACQUIRE.to_owned(),
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
                    .finish_replay_in_tx(tx, input.replay_slot(), replay, decode_acquire_replay)
                    .await;
            }
        }

        if let Err(err) = validate_remote_node_live(&auth, input.node_id, now, true) {
            self.complete_remote_error_in_tx(
                &mut tx,
                input.node_id,
                ROUTE_ACQUIRE,
                Some(input.worker_id),
                &input.idempotency_key,
                &err,
            )
            .await?;
            commit_tx(tx).await?;
            return Err(err);
        }

        let prepared = match self
            .remote_acquire_preflight_in_tx(&mut tx, &input, now)
            .await
        {
            Ok(prepared) => prepared,
            Err(err) => {
                if !is_remote_replayable_error(&err) {
                    return Err(err);
                }
                self.complete_remote_error_in_tx(
                    &mut tx,
                    input.node_id,
                    ROUTE_ACQUIRE,
                    Some(input.worker_id),
                    &input.idempotency_key,
                    &err,
                )
                .await?;
                commit_tx(tx).await?;
                return Err(err);
            }
        };

        let outcome = match prepared {
            RemoteAcquirePrepared::Idle(outcome) | RemoteAcquirePrepared::NoCandidate(outcome) => {
                self.complete_remote_ok_in_tx(
                    &mut tx,
                    input.node_id,
                    ROUTE_ACQUIRE,
                    Some(input.worker_id),
                    &input.idempotency_key,
                    &outcome,
                )
                .await?;
                commit_tx(tx).await?;
                return Ok(outcome);
            }
            RemoteAcquirePrepared::Leased {
                ticket,
                eligibility,
                scheduler_decision,
                selected_access_mode,
            } => {
                self.remote_acquire_leased_in_tx(
                    &mut tx,
                    &input,
                    ticket,
                    eligibility,
                    scheduler_decision,
                    selected_access_mode,
                    now,
                )
                .await?
            }
        };
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

    async fn remote_acquire_preflight_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        now: time::OffsetDateTime,
    ) -> Result<RemoteAcquirePrepared, VoomError> {
        require_positive_ttl(input.lease_ttl_seconds)?;
        let worker = self
            .workers
            .node_owned_worker_in_tx(tx, input.worker_id, input.node_id)
            .await?;
        require_remote_worker(&worker)?;
        let operations = worker_candidate_operations_in_tx(tx, input.worker_id).await?;
        let tickets = self
            .tickets
            .ready_for_operations_in_tx(tx, &operations, now)
            .await?;
        if tickets.is_empty() {
            #[expect(
                clippy::default_constructed_unit_structs,
                reason = "Task 3 intentionally wires the default scheduler scorer"
            )]
            let mut score = SchedulerScorer::default().score(&[])?;
            set_operation_set(&mut score.explanation, &operations);
            let decision = self
                .scheduler_decisions
                .create_or_suppress_in_tx(tx, decision_from_score(input, &score, None, now))
                .await?;
            return Ok(RemoteAcquirePrepared::Idle(RemoteAcquireOutcome::Idle {
                worker_id: input.worker_id,
                scheduler_decision_id: decision.id,
            }));
        }

        let candidate_set = self
            .remote_acquire_candidates_in_tx(tx, input, tickets)
            .await?;
        let score = score_remote_candidates(&candidate_set.candidates)?;
        match score.outcome {
            ScoreOutcome::Idle => Err(VoomError::Internal(
                "remote acquire scorer returned idle for non-empty candidates".to_owned(),
            )),
            ScoreOutcome::NoEligibleCandidate => {
                let decision = self
                    .scheduler_decisions
                    .create_or_suppress_in_tx(tx, decision_from_score(input, &score, None, now))
                    .await?;
                Ok(RemoteAcquirePrepared::NoCandidate(
                    RemoteAcquireOutcome::NoCandidate {
                        worker_id: input.worker_id,
                        scheduler_decision_id: decision.id,
                    },
                ))
            }
            ScoreOutcome::Selected => {
                self.remote_acquire_selected_in_tx(tx, input, &candidate_set, &score, now)
                    .await
            }
        }
    }

    async fn remote_acquire_selected_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        candidate_set: &RemoteAcquireCandidateSet,
        score: &ScoreDecision,
        now: time::OffsetDateTime,
    ) -> Result<RemoteAcquirePrepared, VoomError> {
        let selected = score.selected.as_ref().ok_or_else(|| {
            VoomError::Internal("remote acquire selected score missing tuple".to_owned())
        })?;
        let selected_candidate = candidate_set
            .candidates
            .iter()
            .find(|candidate| {
                candidate.ticket.ticket_id == selected.ticket_id
                    && candidate.worker.worker_id == selected.worker_id
                    && candidate.node.node_id == selected.node_id
            })
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "remote acquire selected candidate vanished ticket={}",
                    selected.ticket_id
                ))
            })?;
        let ticket = candidate_set
            .tickets
            .iter()
            .find(|ticket| ticket.id == selected.ticket_id)
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "remote acquire selected ticket vanished id={}",
                    selected.ticket_id
                ))
            })?
            .clone();
        let eligibility = candidate_set
            .eligibility_by_operation
            .get(&ticket.kind)
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "remote acquire selected eligibility vanished operation={}",
                    ticket.kind
                ))
            })?
            .clone();
        if let Some(outcome) = self
            .recheck_selected_remote_capacity_in_tx(tx, input, selected_candidate, &ticket, now)
            .await?
        {
            return Ok(RemoteAcquirePrepared::NoCandidate(outcome));
        }

        let selected_access_mode = artifact_access_mode_from_scheduler(&selected.access_mode)?;
        let scheduler_decision = self
            .scheduler_decisions
            .create_in_tx(
                tx,
                decision_from_score(
                    input,
                    score,
                    Some((selected.ticket_id, selected.worker_id, selected.node_id)),
                    now,
                ),
            )
            .await?;
        Ok(RemoteAcquirePrepared::Leased {
            ticket,
            eligibility,
            scheduler_decision,
            selected_access_mode,
        })
    }

    async fn remote_acquire_candidates_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        tickets: Vec<Ticket>,
    ) -> Result<RemoteAcquireCandidateSet, VoomError> {
        let mut eligibility_by_operation = HashMap::new();
        let mut worker_active_by_operation = HashMap::new();
        let mut worker_limit_by_operation = HashMap::new();
        let node_limit = self
            .scheduler_node_limits
            .node_limit_in_tx(tx, input.node_id)
            .await?;
        let node_active_leases = active_lease_count_for_node_in_tx(tx, input.node_id).await?;
        let mut candidates = Vec::with_capacity(tickets.len());

        for ticket in &tickets {
            let eligibility =
                if let Some(eligibility) = eligibility_by_operation.get(&ticket.kind).cloned() {
                    eligibility
                } else {
                    let eligibility = self
                        .workers
                        .operation_eligibility_in_tx(tx, input.worker_id, &ticket.kind)
                        .await?;
                    eligibility_by_operation.insert(ticket.kind.clone(), eligibility.clone());
                    eligibility
                };

            let worker_active = if let Some(active) = worker_active_by_operation.get(&ticket.kind) {
                *active
            } else {
                let active = active_lease_count_for_worker_operation_in_tx(
                    tx,
                    input.worker_id,
                    &ticket.kind,
                )
                .await?;
                worker_active_by_operation.insert(ticket.kind.clone(), active);
                active
            };
            let worker_limit = if let Some(limit) = worker_limit_by_operation.get(&ticket.kind) {
                *limit
            } else {
                let limit =
                    max_parallel_for_worker_operation_in_tx(tx, input.worker_id, &ticket.kind)
                        .await?;
                worker_limit_by_operation.insert(ticket.kind.clone(), limit);
                limit
            };
            candidates.push(candidate_from_ticket(
                input,
                ticket,
                &eligibility,
                worker_active,
                worker_limit,
                node_active_leases,
                node_limit,
            )?);
        }

        Ok(RemoteAcquireCandidateSet {
            tickets,
            candidates,
            eligibility_by_operation,
        })
    }

    async fn recheck_selected_remote_capacity_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        selected_candidate: &SchedulerCandidate,
        ticket: &Ticket,
        now: time::OffsetDateTime,
    ) -> Result<Option<RemoteAcquireOutcome>, VoomError> {
        // Candidate scoring uses advisory capacity facts; re-read the selected
        // worker and node before lease creation so capacity decisions use the
        // current transaction view.
        let worker_active =
            active_lease_count_for_worker_operation_in_tx(tx, input.worker_id, &ticket.kind)
                .await?;
        let worker_limit =
            max_parallel_for_worker_operation_in_tx(tx, input.worker_id, &ticket.kind).await?;
        if worker_active >= worker_limit {
            return self
                .capacity_no_candidate_in_tx(
                    tx,
                    input,
                    SelectedCapacityFull {
                        reason_code: StoreSchedulerReasonCode::WorkerCapacityFull,
                        selected_candidate,
                        observed_active: worker_active,
                        observed_limit: worker_limit,
                    },
                    now,
                )
                .await
                .map(Some);
        }

        let node_active = active_lease_count_for_node_in_tx(tx, input.node_id).await?;
        let node_limit = self
            .scheduler_node_limits
            .node_limit_in_tx(tx, input.node_id)
            .await?;
        if node_active >= node_limit {
            return self
                .capacity_no_candidate_in_tx(
                    tx,
                    input,
                    SelectedCapacityFull {
                        reason_code: StoreSchedulerReasonCode::NodeCapacityFull,
                        selected_candidate,
                        observed_active: node_active,
                        observed_limit: node_limit,
                    },
                    now,
                )
                .await
                .map(Some);
        }

        Ok(None)
    }

    async fn capacity_no_candidate_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        capacity: SelectedCapacityFull<'_>,
        now: time::OffsetDateTime,
    ) -> Result<RemoteAcquireOutcome, VoomError> {
        let decision = self
            .scheduler_decisions
            .create_or_suppress_in_tx(
                tx,
                capacity_decision(
                    input,
                    capacity.reason_code,
                    capacity.selected_candidate,
                    1,
                    capacity.observed_active,
                    capacity.observed_limit,
                    now,
                ),
            )
            .await?;
        Ok(RemoteAcquireOutcome::NoCandidate {
            worker_id: input.worker_id,
            scheduler_decision_id: decision.id,
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "remote acquire keeps the transaction input and selected facts explicit"
    )]
    async fn remote_acquire_leased_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        ticket: Ticket,
        eligibility: WorkerOperationEligibility,
        scheduler_decision: SchedulerDecision,
        selected_access_mode: ArtifactAccessMode,
        now: time::OffsetDateTime,
    ) -> Result<RemoteAcquireOutcome, VoomError> {
        let lease = self
            .acquire_lease_in_tx(
                tx,
                NewLease {
                    ticket_id: ticket.id,
                    worker_id: input.worker_id,
                    ttl: Duration::seconds(input.lease_ttl_seconds),
                    now,
                },
            )
            .await?;
        let plan = self
            .artifact_access_plans
            .create_selected_in_tx(
                tx,
                artifact_plan_input(
                    input,
                    &ticket,
                    &eligibility,
                    selected_access_mode,
                    lease.id,
                    now,
                ),
            )
            .await?;
        let scheduler_decision = self
            .scheduler_decisions
            .link_selected_lease_in_tx(tx, scheduler_decision.id, lease.id, now)
            .await?;
        let outcome = RemoteAcquireOutcome::Leased(RemoteLeaseDispatch {
            lease_id: lease.id,
            scheduler_decision_id: scheduler_decision.id,
            ticket_id: ticket.id,
            worker_id: input.worker_id,
            operation: ticket.kind.into_string(),
            dispatch_payload: ticket.payload,
            lease_ttl_seconds: lease.ttl_seconds,
            heartbeat_after_seconds: heartbeat_after_seconds(lease.ttl_seconds),
            artifact_access_plan: remote_plan(&plan),
        });
        self.complete_remote_ok_in_tx(
            tx,
            input.node_id,
            ROUTE_ACQUIRE,
            Some(input.worker_id),
            &input.idempotency_key,
            &outcome,
        )
        .await?;
        Ok(outcome)
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

        if let Err(err) = validate_remote_node_live(&auth, input.node_id, now, false) {
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
        require_positive_ttl(input.lease_ttl_seconds)?;
        let worker = self
            .workers
            .node_owned_worker_in_tx(tx, input.worker_id, input.node_id)
            .await?;
        require_remote_worker(&worker)?;
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

    /// Complete a held remote lease successfully.
    ///
    /// # Errors
    /// Returns authentication, idempotency, ownership, artifact validation,
    /// or lease-release errors.
    pub async fn remote_complete(
        &self,
        input: RemoteCompleteInput,
    ) -> Result<RemoteCompleteOutcome, VoomError> {
        let now = self.clock().now();
        let route_key = route_lease_complete(input.lease_id);
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
                        decode_replay::<RemoteCompleteOutcome>(data, "remote complete")
                    })
                    .await;
            }
        }

        if let Err(err) = validate_remote_node_live(&auth, input.node_id, now, false) {
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

        let prepared = match self.remote_complete_preflight_in_tx(&mut tx, &input).await {
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
            .remote_complete_mutation_in_tx(&mut tx, &input, &route_key, prepared, now)
            .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn remote_complete_preflight_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteCompleteInput,
    ) -> Result<RemoteCompletePrepared, VoomError> {
        let worker = self
            .workers
            .node_owned_worker_in_tx(tx, input.worker_id, input.node_id)
            .await?;
        require_remote_worker(&worker)?;
        self.leases
            .get_held_for_worker_in_tx(tx, input.lease_id, input.worker_id)
            .await?;
        let plan = self
            .artifact_access_plans
            .get_by_lease_in_tx(tx, input.lease_id)
            .await?
            .ok_or_else(|| {
                VoomError::Conflict(format!(
                    "remote complete rejected: lease {} has no artifact access plan",
                    input.lease_id
                ))
            })?;
        let evidence = validated_artifact_complete_evidence(&input.result, &plan)?;
        Ok(RemoteCompletePrepared {
            plan_id: plan.id,
            evidence,
        })
    }

    async fn remote_complete_mutation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteCompleteInput,
        route_key: &str,
        prepared: RemoteCompletePrepared,
        now: time::OffsetDateTime,
    ) -> Result<RemoteCompleteOutcome, VoomError> {
        let consumed = self
            .artifact_access_plans
            .mark_status_in_tx(
                tx,
                prepared.plan_id,
                ArtifactAccessPlanStatus::Consumed,
                Some("worker validated artifact access".to_owned()),
                prepared.evidence,
                now,
            )
            .await?;
        let released = self
            .release_lease_in_tx(tx, input.lease_id, input.result.clone(), now)
            .await?;
        let outcome = RemoteCompleteOutcome {
            lease_id: released.id,
            ticket_id: released.ticket_id,
            worker_id: released.worker_id,
            artifact_access_plan: remote_plan(&consumed),
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

        if let Err(err) = validate_remote_node_live(&auth, input.node_id, now, false) {
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
        require_remote_worker(&worker)?;
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
            status: artifact_failure_status(input.class, &input.reason),
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
            artifact_access_plan: remote_plan(&marked),
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

    /// Run remote recovery primitives for stale nodes and expired leases.
    ///
    /// # Errors
    /// Propagates stale-node marking or lease-expiry errors.
    pub async fn remote_recover(
        &self,
        now: time::OffsetDateTime,
    ) -> Result<RemoteRecoverReport, VoomError> {
        let stale_nodes = self.mark_stale_nodes(now).await?;
        let expired = self.expire_due(now).await?;
        Ok(RemoteRecoverReport {
            stale_nodes: stale_nodes.iter().map(|node| node.id).collect(),
            expired_leases: expired.expired_leases,
            requeued_tickets: expired.requeued_tickets,
            failed_tickets: expired
                .failed_expiries
                .iter()
                .map(|failed| failed.ticket_id)
                .collect(),
        })
    }

    pub(crate) async fn verify_remote_node_token_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        token: &SecretString,
    ) -> Result<NodeAuthRecord, VoomError> {
        let auth = self
            .nodes
            .auth_record_in_tx(tx, node_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("remote node {node_id} not found")))?;
        if auth.kind != NodeKind::Remote {
            return Err(VoomError::Conflict(format!(
                "remote node {node_id} is not a remote node"
            )));
        }
        if !verify_node_token(token.expose_secret(), &auth.auth_token_hash) {
            return Err(VoomError::Conflict(format!(
                "remote node {node_id} token mismatch"
            )));
        }
        Ok(auth)
    }

    async fn complete_remote_ok_in_tx<T>(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        route_key: &str,
        worker_id: Option<WorkerId>,
        idempotency_key: &str,
        outcome: &T,
    ) -> Result<(), VoomError>
    where
        T: serde::Serialize,
    {
        self.remote_idempotency
            .complete_in_tx(
                tx,
                node_id,
                route_key,
                worker_id,
                idempotency_key,
                RemoteMutationReplay::Ok {
                    data: serde_json::to_value(outcome).map_err(|e| {
                        VoomError::Internal(format!("remote replay serialization: {e}"))
                    })?,
                },
            )
            .await
    }

    async fn complete_remote_error_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        route_key: &str,
        worker_id: Option<WorkerId>,
        idempotency_key: &str,
        err: &VoomError,
    ) -> Result<(), VoomError> {
        self.remote_idempotency
            .complete_in_tx(
                tx,
                node_id,
                route_key,
                worker_id,
                idempotency_key,
                RemoteMutationReplay::Error {
                    code: err.code().to_owned(),
                    message: remote_error_message(err),
                },
            )
            .await
    }
}

fn route_lease_complete(lease_id: LeaseId) -> String {
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

async fn worker_candidate_operations_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    worker_id: WorkerId,
) -> Result<Vec<TicketOperation>, VoomError> {
    let operations = sqlx::query_scalar::<_, String>(
        "SELECT operation FROM worker_capabilities WHERE worker_id = ? \
         UNION \
         SELECT value AS operation FROM worker_grants, json_each(worker_grants.can_execute) \
         WHERE worker_id = ? \
         ORDER BY operation ASC",
    )
    .bind(i64::try_from(worker_id.0).map_err(|_| {
        VoomError::Config(format!("worker id {} does not fit sqlite i64", worker_id.0))
    })?)
    .bind(i64::try_from(worker_id.0).map_err(|_| {
        VoomError::Config(format!("worker id {} does not fit sqlite i64", worker_id.0))
    })?)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("worker candidate operations", e))?;
    operations
        .into_iter()
        .map(|operation| {
            TicketOperation::from_stored(operation, "worker candidate operations.operation")
        })
        .collect()
}

fn candidate_from_ticket(
    input: &RemoteAcquireInput,
    ticket: &Ticket,
    eligibility: &WorkerOperationEligibility,
    worker_active: u32,
    worker_limit: u32,
    node_active: u32,
    node_limit: u32,
) -> Result<SchedulerCandidate, VoomError> {
    if worker_limit == 0 || node_limit == 0 {
        return Err(VoomError::Config(
            "scheduler candidate limits must be positive".to_owned(),
        ));
    }
    Ok(SchedulerCandidate {
        ticket: TicketCandidate {
            ticket_id: ticket.id,
            operation: ticket.kind.clone(),
            priority: ticket.priority,
            next_eligible_at_epoch_seconds: ticket.next_eligible_at.unix_timestamp(),
        },
        worker: WorkerCandidate {
            worker_id: input.worker_id,
            node_id: input.node_id,
            executable: true,
            has_capability: eligibility.has_capability,
            has_grant: eligibility.has_grant,
            denied: eligibility.is_denied,
            active_leases: worker_active,
            max_parallel: worker_limit,
            artifact_access: eligibility.artifact_access.clone(),
        },
        node: NodeCandidate {
            node_id: input.node_id,
            executable: true,
            heartbeat_fresh: true,
            active_leases: node_active,
            max_parallel_leases: node_limit,
        },
    })
}

fn score_remote_candidates(candidates: &[SchedulerCandidate]) -> Result<ScoreDecision, VoomError> {
    if candidates.is_empty() {
        #[expect(
            clippy::default_constructed_unit_structs,
            reason = "Task 4 keeps scorer ownership of idle explanations"
        )]
        return SchedulerScorer::default().score(candidates);
    }

    // Remote acquire is still scoped to one worker's ready-ticket snapshot, so
    // candidate breadth stays bounded. Keep the scorer API simple with cloned
    // homogeneous operation slices unless this path grows beyond that scope.
    let mut operation_order = Vec::new();
    let mut by_operation: HashMap<TicketOperation, Vec<SchedulerCandidate>> = HashMap::new();
    for candidate in candidates {
        if !by_operation.contains_key(&candidate.ticket.operation) {
            operation_order.push(candidate.ticket.operation.clone());
        }
        by_operation
            .entry(candidate.ticket.operation.clone())
            .or_default()
            .push(candidate.clone());
    }

    #[expect(
        clippy::default_constructed_unit_structs,
        reason = "Task 4 intentionally uses the default scheduler scorer"
    )]
    let scorer = SchedulerScorer::default();
    let mut best_selected: Option<(ScoreDecision, SchedulerCandidate)> = None;
    let mut first_no_candidate = None;
    let mut group_scores = Vec::new();

    for operation in operation_order {
        let operation_candidates = by_operation.remove(&operation).ok_or_else(|| {
            VoomError::Internal(format!(
                "remote acquire candidate group vanished operation={operation}"
            ))
        })?;
        let score = scorer.score(&operation_candidates)?;
        match score.outcome {
            ScoreOutcome::Selected => {
                let selected_candidate =
                    selected_candidate_for_score(&score, &operation_candidates)?;
                match &best_selected {
                    Some((best_score, best_candidate))
                        if !selected_score_is_better(
                            &score,
                            &selected_candidate,
                            best_score,
                            best_candidate,
                        ) => {}
                    _ => best_selected = Some((score.clone(), selected_candidate)),
                }
            }
            ScoreOutcome::NoEligibleCandidate => {
                first_no_candidate.get_or_insert_with(|| score.clone());
            }
            ScoreOutcome::Idle => {}
        }
        group_scores.push(score);
    }

    if let Some((score, _)) = best_selected {
        return Ok(aggregate_score_decision(
            score,
            &group_scores,
            candidates.len(),
        ));
    }
    first_no_candidate
        .map(|score| aggregate_score_decision(score, &group_scores, candidates.len()))
        .ok_or_else(|| VoomError::Internal("remote acquire scorer returned no decision".to_owned()))
}

fn aggregate_score_decision(
    mut base: ScoreDecision,
    group_scores: &[ScoreDecision],
    candidate_count: usize,
) -> ScoreDecision {
    let mut candidate_rows = Vec::new();
    let mut operations = Vec::new();
    for score in group_scores {
        if let Some(operation) = score
            .explanation
            .get("operation")
            .and_then(JsonValue::as_str)
            && !operations.contains(&operation.to_owned())
        {
            operations.push(operation.to_owned());
        }
        if let Some(rows) = score
            .explanation
            .get("candidates")
            .and_then(JsonValue::as_array)
        {
            candidate_rows.extend(rows.iter().cloned());
        }
    }
    if let Some(object) = base.explanation.as_object_mut() {
        object.insert("candidates".to_owned(), JsonValue::Array(candidate_rows));
        object.insert("operation_set".to_owned(), json!(operations));
        if operations.len() != 1 {
            object.insert("operation".to_owned(), JsonValue::Null);
        }
    }
    base.candidate_count = candidate_count;
    if base.outcome == ScoreOutcome::NoEligibleCandidate {
        base.reason_code = first_rejection_reason(&base.explanation);
    }
    base
}

fn first_rejection_reason(explanation: &JsonValue) -> ScoreReasonCode {
    explanation
        .get("candidates")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("reasons").and_then(JsonValue::as_array))
        .flatten()
        .filter_map(JsonValue::as_str)
        .filter_map(ScoreReasonCode::parse)
        .min_by_key(|reason| reason.priority())
        .unwrap_or(ScoreReasonCode::NoEligibleCandidate)
}

fn selected_candidate_for_score(
    score: &voom_scheduler::ScoreDecision,
    candidates: &[SchedulerCandidate],
) -> Result<SchedulerCandidate, VoomError> {
    let selected = score
        .selected
        .as_ref()
        .ok_or_else(|| VoomError::Internal("selected score missing tuple".to_owned()))?;
    candidates
        .iter()
        .find(|candidate| {
            candidate.ticket.ticket_id == selected.ticket_id
                && candidate.worker.worker_id == selected.worker_id
                && candidate.node.node_id == selected.node_id
        })
        .cloned()
        .ok_or_else(|| {
            VoomError::Internal(format!(
                "selected score references missing candidate ticket={}",
                selected.ticket_id
            ))
        })
}

fn selected_score_is_better(
    challenger: &ScoreDecision,
    challenger_candidate: &SchedulerCandidate,
    incumbent: &ScoreDecision,
    incumbent_candidate: &SchedulerCandidate,
) -> bool {
    let challenger_score = challenger
        .selected
        .as_ref()
        .map_or(i64::MIN, |selected| selected.score);
    let incumbent_score = incumbent
        .selected
        .as_ref()
        .map_or(i64::MIN, |selected| selected.score);
    challenger_score > incumbent_score
        || (challenger_score == incumbent_score
            && selected_candidate_key(challenger_candidate)
                < selected_candidate_key(incumbent_candidate))
}

fn selected_candidate_key(
    candidate: &SchedulerCandidate,
) -> (std::cmp::Reverse<i64>, i64, u64, u64, u64) {
    (
        std::cmp::Reverse(candidate.ticket.priority),
        candidate.ticket.next_eligible_at_epoch_seconds,
        candidate.node.node_id.0,
        candidate.worker.worker_id.0,
        candidate.ticket.ticket_id.0,
    )
}

async fn active_lease_count_for_node_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    node_id: NodeId,
) -> Result<u32, VoomError> {
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) \
         FROM leases \
         JOIN workers ON workers.id = leases.worker_id \
         WHERE leases.state = 'held' AND workers.node_id = ?",
    )
    .bind(sqlite_id(node_id.0, "node id")?)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("node active lease count", e))?;
    count_to_u32(count, "node active lease count")
}

async fn active_lease_count_for_worker_operation_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    worker_id: WorkerId,
    operation: &TicketOperation,
) -> Result<u32, VoomError> {
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) \
         FROM leases \
         JOIN tickets ON tickets.id = leases.ticket_id \
         WHERE leases.state = 'held' AND leases.worker_id = ? AND tickets.kind = ?",
    )
    .bind(sqlite_id(worker_id.0, "worker id")?)
    .bind(operation.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("worker operation active lease count", e))?;
    count_to_u32(count, "worker operation active lease count")
}

async fn max_parallel_for_worker_operation_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    worker_id: WorkerId,
    operation: &TicketOperation,
) -> Result<u32, VoomError> {
    let rows =
        sqlx::query("SELECT max_parallel FROM worker_grants WHERE worker_id = ? ORDER BY id")
            .bind(sqlite_id(worker_id.0, "worker id")?)
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("worker max_parallel read", e))?;

    let mut operation_limit = None;
    let mut wildcard_limit = None;
    for row in rows {
        let raw: String = row
            .try_get("max_parallel")
            .map_err(|e| VoomError::database_context("worker max_parallel row", e))?;
        let value: JsonValue = serde_json::from_str(&raw)
            .map_err(|e| VoomError::database_context("parse worker max_parallel", e))?;
        operation_limit = max_optional_limit(
            operation_limit,
            json_positive_u32(value.get(operation.as_str()), "max_parallel operation")?,
        );
        wildcard_limit = max_optional_limit(
            wildcard_limit,
            json_positive_u32(value.get("*"), "max_parallel wildcard")?,
        );
    }

    Ok(operation_limit.or(wildcard_limit).unwrap_or(1))
}

fn max_optional_limit(current: Option<u32>, candidate: Option<u32>) -> Option<u32> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (Some(current), None) => Some(current),
        (None, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

fn json_positive_u32(
    value: Option<&JsonValue>,
    label: &'static str,
) -> Result<Option<u32>, VoomError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(limit) = value.as_u64() else {
        return Err(VoomError::Config(format!("{label} must be an integer")));
    };
    if limit == 0 {
        return Err(VoomError::Config(format!("{label} must be positive")));
    }
    u32::try_from(limit)
        .map(Some)
        .map_err(|_| VoomError::Config(format!("{label} does not fit u32")))
}

fn sqlite_id(id: u64, label: &'static str) -> Result<i64, VoomError> {
    i64::try_from(id)
        .map_err(|_| VoomError::Config(format!("{label} {id} does not fit sqlite i64")))
}

fn count_to_u32(count: i64, label: &'static str) -> Result<u32, VoomError> {
    u32::try_from(count).map_err(|_| VoomError::database(format!("{label} does not fit u32")))
}

fn artifact_access_mode_from_scheduler(mode: &str) -> Result<ArtifactAccessMode, VoomError> {
    match mode {
        "shared_mount" => Ok(ArtifactAccessMode::SharedMount),
        "control_plane_placeholder" => Ok(ArtifactAccessMode::ControlPlanePlaceholder),
        "staged_output_placeholder" => Ok(ArtifactAccessMode::StagedOutputPlaceholder),
        other => Err(VoomError::Internal(format!(
            "scheduler selected unsupported artifact access mode {other:?}"
        ))),
    }
}

fn validate_remote_node_live(
    auth: &NodeAuthRecord,
    node_id: NodeId,
    now: time::OffsetDateTime,
    require_fresh_for_acquire: bool,
) -> Result<(), VoomError> {
    if auth.status == NodeStatus::Retired {
        return Err(VoomError::Conflict(format!(
            "remote node {node_id} is retired"
        )));
    }
    if require_fresh_for_acquire {
        if auth.status == NodeStatus::Stale {
            return Err(VoomError::Conflict(format!(
                "remote node {node_id} is stale"
            )));
        }
        let expires_at =
            auth.last_seen_at + Duration::seconds(i64::from(auth.heartbeat_ttl_seconds));
        if expires_at <= now {
            return Err(VoomError::Conflict(format!(
                "remote node {node_id} heartbeat expired"
            )));
        }
    }
    Ok(())
}

fn require_remote_worker(worker: &Worker) -> Result<(), VoomError> {
    if worker.kind != WorkerKind::Remote {
        return Err(VoomError::Conflict(format!(
            "remote execution rejected: worker {} is not a remote worker",
            worker.id
        )));
    }
    Ok(())
}

fn require_positive_ttl(ttl_seconds: i64) -> Result<(), VoomError> {
    if ttl_seconds <= 0 {
        return Err(VoomError::Config(format!(
            "lease ttl must be positive, got {ttl_seconds}s"
        )));
    }
    Ok(())
}

fn is_remote_replayable_error(err: &VoomError) -> bool {
    matches!(
        err.error_code(),
        ErrorCode::Conflict | ErrorCode::ConfigInvalid | ErrorCode::NotFound
    )
}

fn decision_from_score(
    input: &RemoteAcquireInput,
    score: &voom_scheduler::ScoreDecision,
    selected: Option<(TicketId, WorkerId, NodeId)>,
    now: OffsetDateTime,
) -> NewSchedulerDecision {
    let (ticket_id, selected_worker_id, selected_node_id) = selected
        .map_or((None, None, None), |(ticket_id, worker_id, node_id)| {
            (Some(ticket_id), Some(worker_id), Some(node_id))
        });
    let (decision_kind, outcome) = match score.outcome {
        ScoreOutcome::Selected => (
            SchedulerDecisionKind::LeaseAcquire,
            SchedulerDecisionOutcome::Selected,
        ),
        ScoreOutcome::Idle => (SchedulerDecisionKind::Idle, SchedulerDecisionOutcome::Idle),
        ScoreOutcome::NoEligibleCandidate => (
            SchedulerDecisionKind::NoCandidate,
            SchedulerDecisionOutcome::NoEligibleCandidate,
        ),
    };

    NewSchedulerDecision {
        decision_kind,
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some(input.idempotency_key.clone()),
        request_node_id: Some(input.node_id),
        request_worker_id: Some(input.worker_id),
        ticket_id,
        selected_worker_id,
        selected_node_id,
        selected_lease_id: None,
        outcome,
        reason_code: scheduler_reason(score.reason_code),
        summary: scheduler_summary(score),
        candidate_count: u32::try_from(score.candidate_count).unwrap_or(u32::MAX),
        selected_score: match score.outcome {
            ScoreOutcome::Selected => score.selected.as_ref().map(|selected| selected.score),
            ScoreOutcome::Idle | ScoreOutcome::NoEligibleCandidate => None,
        },
        suppression_key: suppression_key(input, score),
        explanation: score.explanation.clone(),
        now,
    }
}

fn capacity_decision(
    input: &RemoteAcquireInput,
    reason_code: StoreSchedulerReasonCode,
    selected_candidate: &SchedulerCandidate,
    candidate_count: usize,
    observed_active: u32,
    observed_limit: u32,
    now: OffsetDateTime,
) -> NewSchedulerDecision {
    let reason = reason_code.as_str();
    NewSchedulerDecision {
        decision_kind: SchedulerDecisionKind::NoCandidate,
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some(input.idempotency_key.clone()),
        request_node_id: Some(input.node_id),
        request_worker_id: Some(input.worker_id),
        ticket_id: None,
        selected_worker_id: None,
        selected_node_id: None,
        selected_lease_id: None,
        outcome: SchedulerDecisionOutcome::NoEligibleCandidate,
        reason_code,
        summary: format!("no eligible candidate: {reason}"),
        candidate_count: u32::try_from(candidate_count).unwrap_or(u32::MAX),
        selected_score: None,
        suppression_key: Some(capacity_suppression_key(
            input,
            reason,
            &selected_candidate.ticket.operation,
        )),
        explanation: json!({
            "scoring_version": SCORING_VERSION,
            "outcome": "no_eligible_candidate",
            "reason": reason,
            "operation": selected_candidate.ticket.operation.as_str(),
            "selected_ticket_id": selected_candidate.ticket.ticket_id.0,
            "observed": {
                "active_leases": observed_active,
                "limit": observed_limit
            }
        }),
        now,
    }
}

fn scheduler_reason(reason: ScoreReasonCode) -> StoreSchedulerReasonCode {
    match reason {
        ScoreReasonCode::Selected => StoreSchedulerReasonCode::Selected,
        ScoreReasonCode::NoReadyTicket => StoreSchedulerReasonCode::NoReadyTicket,
        ScoreReasonCode::MissingCapability => StoreSchedulerReasonCode::MissingCapability,
        ScoreReasonCode::MissingGrant => StoreSchedulerReasonCode::MissingGrant,
        ScoreReasonCode::OperationDenied => StoreSchedulerReasonCode::OperationDenied,
        ScoreReasonCode::WorkerNotExecutable => StoreSchedulerReasonCode::WorkerNotExecutable,
        ScoreReasonCode::NodeNotExecutable => StoreSchedulerReasonCode::NodeNotExecutable,
        ScoreReasonCode::HeartbeatExpired => StoreSchedulerReasonCode::HeartbeatExpired,
        ScoreReasonCode::UnsupportedArtifactAccess => {
            StoreSchedulerReasonCode::UnsupportedArtifactAccess
        }
        ScoreReasonCode::WorkerCapacityFull => StoreSchedulerReasonCode::WorkerCapacityFull,
        ScoreReasonCode::NodeCapacityFull => StoreSchedulerReasonCode::NodeCapacityFull,
        ScoreReasonCode::NoEligibleCandidate => StoreSchedulerReasonCode::NoEligibleCandidate,
    }
}

fn scheduler_summary(score: &voom_scheduler::ScoreDecision) -> String {
    match score.outcome {
        ScoreOutcome::Selected => {
            if let Some(selected) = &score.selected {
                format!(
                    "selected worker {} on node {} for ticket {}",
                    selected.worker_id, selected.node_id, selected.ticket_id
                )
            } else {
                "selected scheduler candidate".to_owned()
            }
        }
        ScoreOutcome::Idle => "no ready tickets".to_owned(),
        ScoreOutcome::NoEligibleCandidate => {
            format!("no eligible candidate: {}", score.reason_code.as_str())
        }
    }
}

fn suppression_key(
    input: &RemoteAcquireInput,
    score: &voom_scheduler::ScoreDecision,
) -> Option<String> {
    if score.outcome == ScoreOutcome::Selected {
        return None;
    }
    Some(remote_acquire_suppression_key(
        input,
        score.reason_code.as_str(),
        &operation_fingerprint(&score.explanation),
    ))
}

fn capacity_suppression_key(
    input: &RemoteAcquireInput,
    reason: &str,
    operation: &TicketOperation,
) -> String {
    remote_acquire_suppression_key(input, reason, operation.as_str())
}

fn remote_acquire_suppression_key(
    input: &RemoteAcquireInput,
    reason: &str,
    operation_fingerprint: &str,
) -> String {
    let bucket = input.lease_ttl_seconds.max(1) / 30;
    format!(
        "remote_acquire:node:{}:worker:{}:reason:{}:ops:{}:bucket:{}",
        input.node_id, input.worker_id, reason, operation_fingerprint, bucket
    )
}

fn set_operation_set(explanation: &mut JsonValue, operations: &[TicketOperation]) {
    if let Some(object) = explanation.as_object_mut() {
        object.insert(
            "operation_set".to_owned(),
            json!(
                operations
                    .iter()
                    .map(TicketOperation::as_str)
                    .collect::<Vec<_>>()
            ),
        );
    }
}

fn operation_fingerprint(explanation: &JsonValue) -> String {
    let mut operations = explanation
        .get("operation_set")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if operations.is_empty()
        && let Some(operation) = explanation.get("operation").and_then(JsonValue::as_str)
    {
        operations.push(operation.to_owned());
    }

    if operations.is_empty() {
        operations = explanation
            .get("candidates")
            .and_then(JsonValue::as_array)
            .into_iter()
            .flatten()
            .filter_map(|candidate| candidate.get("operation").and_then(JsonValue::as_str))
            .map(ToOwned::to_owned)
            .collect();
    }

    operations.sort();
    operations.dedup();
    if operations.is_empty() {
        "none".to_owned()
    } else {
        operations.join("+")
    }
}

fn artifact_plan_input(
    input: &RemoteAcquireInput,
    ticket: &Ticket,
    eligibility: &WorkerOperationEligibility,
    selected_access_mode: ArtifactAccessMode,
    lease_id: LeaseId,
    now: time::OffsetDateTime,
) -> NewArtifactAccessPlan {
    NewArtifactAccessPlan {
        lease_id,
        ticket_id: ticket.id,
        worker_id: input.worker_id,
        node_id: input.node_id,
        input_handles: artifact_handles(&ticket.payload, "inputs"),
        output_handles: artifact_handles(&ticket.payload, "outputs"),
        selected_access_mode,
        evidence: json!({
            "selected_by": "remote_acquire",
            "route": ROUTE_ACQUIRE,
            "advertised_artifact_access": eligibility.artifact_access,
        }),
        now,
    }
}

fn artifact_handles(payload: &JsonValue, direction: &str) -> Vec<String> {
    payload
        .get("artifact_access")
        .and_then(|access| access.get(direction))
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|handles| !handles.is_empty())
        .unwrap_or_else(|| match direction {
            "inputs" => vec!["handle:input:synthetic".to_owned()],
            "outputs" => vec!["handle:output:synthetic".to_owned()],
            _ => Vec::new(),
        })
}

fn artifact_failure_status(class: FailureClass, reason: &str) -> ArtifactAccessPlanStatus {
    if matches!(
        class,
        FailureClass::WorkerTimeout
            | FailureClass::WorkerCrash
            | FailureClass::ProgressTimeout
            | FailureClass::ExternalSystemUnavailable
            | FailureClass::ExternalSystemRateLimited
            | FailureClass::BackupFailure
            | FailureClass::CommitFailure
    ) {
        return ArtifactAccessPlanStatus::Failed;
    }

    let reason = reason.to_ascii_lowercase();
    if matches!(
        class,
        FailureClass::ArtifactUnavailable
            | FailureClass::ArtifactChecksumMismatch
            | FailureClass::MalformedWorkerResult
            | FailureClass::MissingCapability
    ) || reason.contains("artifact")
        || reason.contains("access mode")
        || reason.contains("selected mode")
    {
        ArtifactAccessPlanStatus::Rejected
    } else {
        ArtifactAccessPlanStatus::Failed
    }
}

fn heartbeat_after_seconds(ttl_seconds: i64) -> i64 {
    (ttl_seconds / 2).max(1)
}

fn remote_plan(plan: &ArtifactAccessPlan) -> RemoteArtifactAccessPlan {
    RemoteArtifactAccessPlan {
        id: plan.id,
        input_handles: plan.input_handles.clone(),
        output_handles: plan.output_handles.clone(),
        selected_access_mode: plan.selected_access_mode,
    }
}

fn validated_artifact_complete_evidence(
    result: &JsonValue,
    plan: &ArtifactAccessPlan,
) -> Result<JsonValue, VoomError> {
    let evidence = result.get("artifact_access").ok_or_else(|| {
        VoomError::Conflict(
            "remote complete rejected: artifact access validation missing".to_owned(),
        )
    })?;
    if evidence.get("validated") != Some(&JsonValue::Bool(true)) {
        return Err(VoomError::Conflict(
            "remote complete rejected: artifact access validation missing".to_owned(),
        ));
    }
    let mode = evidence
        .get("mode")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            VoomError::Conflict("remote complete rejected: artifact access mode missing".to_owned())
        })?;
    if mode != plan.selected_access_mode.as_str() {
        return Err(VoomError::Conflict(format!(
            "remote complete rejected: artifact access mode {mode} does not match selected mode {}",
            plan.selected_access_mode.as_str()
        )));
    }
    let inputs = string_array_evidence(evidence, "inputs_consumed")?;
    if inputs != plan.input_handles {
        return Err(VoomError::Conflict(
            "remote complete rejected: artifact input handles do not match selected plan"
                .to_owned(),
        ));
    }
    let outputs = string_array_evidence(evidence, "outputs_declared")?;
    if outputs != plan.output_handles {
        return Err(VoomError::Conflict(
            "remote complete rejected: artifact output handles do not match selected plan"
                .to_owned(),
        ));
    }
    Ok(evidence.clone())
}

fn string_array_evidence(value: &JsonValue, field: &'static str) -> Result<Vec<String>, VoomError> {
    value
        .get(field)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| VoomError::Conflict(format!("remote complete rejected: {field} missing")))?
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                VoomError::Conflict(format!("remote complete rejected: {field} must be strings"))
            })
        })
        .collect()
}

/// Identity of the idempotency row a replay decodes from — the tuple
/// `repoint_completed_replay_in_tx` matches on. Owns `route_key` because some
/// routes derive it (`route_lease_*`) rather than holding a borrowable field.
struct ReplaySlot<'a> {
    node_id: NodeId,
    route_key: String,
    worker_id: Option<WorkerId>,
    idempotency_key: &'a str,
}

/// Maps a remote-execution input to the idempotency row it replays from, so
/// the replay branch and any poison-repoint target the same row the
/// reservation used.
trait ReplayRoute {
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

fn decode_acquire_replay(data: JsonValue) -> Result<RemoteAcquireOutcome, VoomError> {
    let data = acquire_replay_with_legacy_decision_id(data);
    serde_json::from_value(data)
        .map_err(|e| VoomError::Internal(format!("remote acquire replay decode: {e}")))
}

fn decode_replay<T>(data: JsonValue, label: &str) -> Result<T, VoomError>
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

fn remote_error_message(err: &VoomError) -> String {
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
