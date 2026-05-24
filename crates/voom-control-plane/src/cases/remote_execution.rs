//! Remote execution use cases for node-owned workers.

use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value as JsonValue, json};
use sqlx::{Sqlite, Transaction};
use time::Duration;
use voom_core::{FailureClass, LeaseId, NodeId, TicketId, VoomError, WorkerId};
use voom_store::repo::artifact_access_plans::{
    ArtifactAccessMode, ArtifactAccessPlan, ArtifactAccessPlanRepo, ArtifactAccessPlanStatus,
    NewArtifactAccessPlan,
};
use voom_store::repo::leases::{LeaseRepo, NewLease};
use voom_store::repo::nodes::{NodeAuthRecord, NodeKind, NodeRepo, NodeStatus};
use voom_store::repo::remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteIdempotencyRepo, RemoteMutationReplay,
};
use voom_store::repo::tickets::{Ticket, TicketRepo};
use voom_store::repo::workers::{Worker, WorkerKind, WorkerOperationEligibility, WorkerRepo};

use crate::ControlPlane;
use crate::node_auth::verify_node_token;

use super::{begin_tx, commit_tx};

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
    Idle { worker_id: WorkerId },
    Leased(RemoteLeaseDispatch),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteLeaseDispatch {
    pub lease_id: LeaseId,
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

enum RemoteAcquirePrepared {
    Idle(RemoteAcquireOutcome),
    Leased {
        ticket: Ticket,
        eligibility: WorkerOperationEligibility,
    },
}

struct RemoteCompletePrepared {
    ticket_id: TicketId,
    worker_id: WorkerId,
    plan_id: u64,
    evidence: JsonValue,
}

struct RemoteFailPrepared {
    ticket_id: TicketId,
    worker_id: WorkerId,
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
        let mut tx = begin_tx(&self.pool).await?;
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
                let out = replay_node_heartbeat(replay)?;
                commit_tx(tx).await?;
                return Ok(out);
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
        let mut tx = begin_tx(&self.pool).await?;
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
                let out = replay_acquire(replay)?;
                commit_tx(tx).await?;
                return Ok(out);
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
                complete_remote_replayable_error(&err)?;
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
            RemoteAcquirePrepared::Idle(outcome) => {
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
            } => {
                self.remote_acquire_leased_in_tx(&mut tx, &input, ticket, eligibility, now)
                    .await?
            }
        };
        commit_tx(tx).await?;
        Ok(outcome)
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
            return Ok(RemoteAcquirePrepared::Idle(RemoteAcquireOutcome::Idle {
                worker_id: input.worker_id,
            }));
        }

        let mut first_ineligible = None;
        for ticket in tickets {
            let eligibility = self
                .workers
                .operation_eligibility_in_tx(tx, input.worker_id, &ticket.kind)
                .await?;
            match require_eligible(input.worker_id, &ticket, &eligibility) {
                Ok(()) => {
                    return Ok(RemoteAcquirePrepared::Leased {
                        ticket,
                        eligibility,
                    });
                }
                Err(err) => {
                    first_ineligible.get_or_insert(err);
                }
            }
        }
        let err = first_ineligible.ok_or_else(|| {
            VoomError::Internal("remote acquire candidate set vanished".to_owned())
        })?;
        Err(err)
    }

    async fn remote_acquire_leased_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        ticket: Ticket,
        eligibility: WorkerOperationEligibility,
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
                artifact_plan_input(input, &ticket, &eligibility, lease.id, now),
            )
            .await?;
        let outcome = RemoteAcquireOutcome::Leased(RemoteLeaseDispatch {
            lease_id: lease.id,
            ticket_id: ticket.id,
            worker_id: input.worker_id,
            operation: ticket.kind,
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
        let mut tx = begin_tx(&self.pool).await?;
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
                let out = replay_lease_heartbeat(replay)?;
                commit_tx(tx).await?;
                return Ok(out);
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
                complete_remote_replayable_error(&err)?;
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
        let mut tx = begin_tx(&self.pool).await?;
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
                let out = replay_complete(replay)?;
                commit_tx(tx).await?;
                return Ok(out);
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
                complete_remote_replayable_error(&err)?;
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
        let held = self
            .leases
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
        if input.result["artifact_access"]["validated"] != JsonValue::Bool(true) {
            return Err(VoomError::Conflict(
                "remote complete rejected: artifact access validation missing".to_owned(),
            ));
        }
        let evidence = input
            .result
            .get("artifact_access")
            .cloned()
            .unwrap_or_else(|| json!({}));
        Ok(RemoteCompletePrepared {
            ticket_id: held.ticket_id,
            worker_id: held.worker_id,
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
            ticket_id: prepared.ticket_id,
            worker_id: prepared.worker_id,
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
        let mut tx = begin_tx(&self.pool).await?;
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
                let out = replay_fail(replay)?;
                commit_tx(tx).await?;
                return Ok(out);
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
                complete_remote_replayable_error(&err)?;
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
        let held = self
            .leases
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
            ticket_id: held.ticket_id,
            worker_id: held.worker_id,
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
            ticket_id: prepared.ticket_id,
            worker_id: prepared.worker_id,
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
) -> Result<Vec<String>, VoomError> {
    sqlx::query_scalar::<_, String>(
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
    .map_err(|e| VoomError::Database(format!("worker candidate operations: {e}")))
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

fn require_eligible(
    worker_id: WorkerId,
    ticket: &Ticket,
    eligibility: &WorkerOperationEligibility,
) -> Result<(), VoomError> {
    if !eligibility.has_capability {
        return Err(VoomError::Conflict(format!(
            "remote acquire rejected: worker {worker_id} lacks capability for {}",
            ticket.kind
        )));
    }
    if !eligibility.has_grant {
        return Err(VoomError::Conflict(format!(
            "remote acquire rejected: worker {worker_id} lacks grant for {}",
            ticket.kind
        )));
    }
    if eligibility.is_denied {
        return Err(VoomError::Conflict(format!(
            "remote acquire rejected: worker {worker_id} is denied for {}",
            ticket.kind
        )));
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

fn complete_remote_replayable_error(err: &VoomError) -> Result<(), VoomError> {
    match err {
        VoomError::Conflict(_) | VoomError::Config(_) | VoomError::NotFound(_) => Ok(()),
        VoomError::Database(message) => Err(VoomError::Database(message.clone())),
        VoomError::Migration(message) => Err(VoomError::Migration(message.clone())),
        VoomError::DirtyMigration(message) => Err(VoomError::DirtyMigration(message.clone())),
        VoomError::SchemaTooNew(message) => Err(VoomError::SchemaTooNew(message.clone())),
        VoomError::Internal(message) => Err(VoomError::Internal(message.clone())),
        VoomError::DependencyCycle(message) => Err(VoomError::DependencyCycle(message.clone())),
        VoomError::BlockedByUseLease(message) => Err(VoomError::BlockedByUseLease(message.clone())),
        VoomError::BlockedByPendingCommit(message) => {
            Err(VoomError::BlockedByPendingCommit(message.clone()))
        }
        VoomError::BlockedByClosureGrew(message) => {
            Err(VoomError::BlockedByClosureGrew(message.clone()))
        }
        VoomError::StaleIdentityEvidence(message) => {
            Err(VoomError::StaleIdentityEvidence(message.clone()))
        }
        VoomError::ClosureResolutionIncomplete(message) => {
            Err(VoomError::ClosureResolutionIncomplete(message.clone()))
        }
        VoomError::WorkerTimeout(message) => Err(VoomError::WorkerTimeout(message.clone())),
        VoomError::WorkerCrash(message) => Err(VoomError::WorkerCrash(message.clone())),
        VoomError::NoEligibleWorker(message) => Err(VoomError::NoEligibleWorker(message.clone())),
        VoomError::ArtifactUnavailable(message) => {
            Err(VoomError::ArtifactUnavailable(message.clone()))
        }
        VoomError::ArtifactChecksumMismatch(message) => {
            Err(VoomError::ArtifactChecksumMismatch(message.clone()))
        }
        VoomError::ExternalSystemUnavailable(message) => {
            Err(VoomError::ExternalSystemUnavailable(message.clone()))
        }
        VoomError::ExternalSystemRateLimited(message) => {
            Err(VoomError::ExternalSystemRateLimited(message.clone()))
        }
        VoomError::VerificationFailure(message) => {
            Err(VoomError::VerificationFailure(message.clone()))
        }
        VoomError::BackupFailure(message) => Err(VoomError::BackupFailure(message.clone())),
        VoomError::CommitFailure(message) => Err(VoomError::CommitFailure(message.clone())),
        VoomError::PolicyParseError(message) => Err(VoomError::PolicyParseError(message.clone())),
        VoomError::PolicyValidationError(message) => {
            Err(VoomError::PolicyValidationError(message.clone()))
        }
        VoomError::PlanGeneration(message) => Err(VoomError::PlanGeneration(message.clone())),
        VoomError::ComplianceReport(message) => Err(VoomError::ComplianceReport(message.clone())),
        VoomError::PolicyExecution(message) => Err(VoomError::PolicyExecution(message.clone())),
        VoomError::MissingCapability(message) => Err(VoomError::MissingCapability(message.clone())),
        VoomError::MalformedWorkerResult(message) => {
            Err(VoomError::MalformedWorkerResult(message.clone()))
        }
        VoomError::UserCancellation(message) => Err(VoomError::UserCancellation(message.clone())),
        VoomError::ApprovalRequired(message) => Err(VoomError::ApprovalRequired(message.clone())),
        VoomError::PriorityPolicyConflict(message) => {
            Err(VoomError::PriorityPolicyConflict(message.clone()))
        }
        VoomError::WorkerRetired(message) => Err(VoomError::WorkerRetired(message.clone())),
        VoomError::WorkerIncarnationStale(message) => {
            Err(VoomError::WorkerIncarnationStale(message.clone()))
        }
        VoomError::AmbiguousWorkerSelection(message) => {
            Err(VoomError::AmbiguousWorkerSelection(message.clone()))
        }
    }
}

fn artifact_plan_input(
    input: &RemoteAcquireInput,
    ticket: &Ticket,
    eligibility: &WorkerOperationEligibility,
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
        selected_access_mode: select_access_mode(&eligibility.artifact_access),
        evidence: json!({
            "selected_by": "remote_acquire",
            "route": ROUTE_ACQUIRE,
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

fn select_access_mode(modes: &[String]) -> ArtifactAccessMode {
    if modes.iter().any(|mode| mode == "shared_mount") {
        ArtifactAccessMode::SharedMount
    } else if modes.iter().any(|mode| mode == "control_plane_placeholder") {
        ArtifactAccessMode::ControlPlanePlaceholder
    } else if modes.iter().any(|mode| mode == "staged_output_placeholder") {
        ArtifactAccessMode::StagedOutputPlaceholder
    } else {
        ArtifactAccessMode::ControlPlanePlaceholder
    }
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

fn replay_acquire(replay: RemoteMutationReplay) -> Result<RemoteAcquireOutcome, VoomError> {
    match replay {
        RemoteMutationReplay::Ok { data } => serde_json::from_value(data)
            .map_err(|e| VoomError::Internal(format!("remote acquire replay decode: {e}"))),
        RemoteMutationReplay::Error { code, message } => Err(replay_error(&code, message)),
    }
}

fn replay_node_heartbeat(
    replay: RemoteMutationReplay,
) -> Result<RemoteNodeHeartbeatOutcome, VoomError> {
    match replay {
        RemoteMutationReplay::Ok { data } => serde_json::from_value(data)
            .map_err(|e| VoomError::Internal(format!("remote node heartbeat replay decode: {e}"))),
        RemoteMutationReplay::Error { code, message } => Err(replay_error(&code, message)),
    }
}

fn replay_lease_heartbeat(
    replay: RemoteMutationReplay,
) -> Result<RemoteLeaseHeartbeatOutcome, VoomError> {
    match replay {
        RemoteMutationReplay::Ok { data } => serde_json::from_value(data)
            .map_err(|e| VoomError::Internal(format!("remote lease heartbeat replay decode: {e}"))),
        RemoteMutationReplay::Error { code, message } => Err(replay_error(&code, message)),
    }
}

fn replay_complete(replay: RemoteMutationReplay) -> Result<RemoteCompleteOutcome, VoomError> {
    match replay {
        RemoteMutationReplay::Ok { data } => serde_json::from_value(data)
            .map_err(|e| VoomError::Internal(format!("remote complete replay decode: {e}"))),
        RemoteMutationReplay::Error { code, message } => Err(replay_error(&code, message)),
    }
}

fn replay_fail(replay: RemoteMutationReplay) -> Result<RemoteFailOutcome, VoomError> {
    match replay {
        RemoteMutationReplay::Ok { data } => serde_json::from_value(data)
            .map_err(|e| VoomError::Internal(format!("remote fail replay decode: {e}"))),
        RemoteMutationReplay::Error { code, message } => Err(replay_error(&code, message)),
    }
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
#[path = "remote_execution_test.rs"]
mod tests;
