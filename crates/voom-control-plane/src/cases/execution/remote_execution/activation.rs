//! Atomic remote-node incarnation activation and deactivation.

use std::collections::BTreeSet;

use serde_json::json;
use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{
    NodeIncarnationEndReason, NodeIncarnationStatus, TicketOperation, VoomError, WorkerId,
    WorkerKind,
};
use voom_events::payload::{
    NodeIncarnationActivatedPayload, NodeIncarnationEndedPayload, WorkerCapabilityRecordedPayload,
    WorkerGrantRecordedPayload, WorkerLinkedToNodePayload, WorkerRegisteredPayload,
    WorkerRetiredPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::execution::node_incarnations::{NewNodeIncarnation, NodeIncarnation};
use voom_store::repo::execution::nodes::NodeStatus;
use voom_store::repo::execution::remote_idempotency::{IdempotencyOutcome, RemoteIdempotencyInput};
use voom_store::repo::execution::workers::{NewCapability, NewGrant, NewWorker};

use crate::ControlPlane;
use crate::cases::{append_event, begin_immediate_tx, commit_tx};

use super::{
    ActivatedWorker, RemoteActivateInput, RemoteActivateOutcome, RemoteDeactivateInput,
    RemoteDeactivateOutcome, RemoteWorkerDeclaration, ReplayRoute, decode_replay,
};

const MAX_WORKERS: usize = 64;
const MAX_LOGICAL_NAME_BYTES: usize = 64;
const MAX_PARALLEL: u32 = 256;
const MAX_FRESH_ACTIVATIONS: u32 = 5;
const ACTIVATION_WINDOW: time::Duration = time::Duration::seconds(60);

impl ControlPlane {
    /// Atomically start one node-process incarnation and register its complete worker manifest.
    ///
    /// # Errors
    /// Returns configuration, authentication, conflict, or persistence errors.
    pub async fn remote_activate(
        &self,
        input: RemoteActivateInput,
    ) -> Result<RemoteActivateOutcome, VoomError> {
        validate_manifest(&input.workers)?;
        let route_key = super::route_node_activate(input.node_id);
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let now = self.clock().now();
        let auth = self
            .verify_remote_node_token_in_tx(&mut tx, input.node_id, &input.token)
            .await?;
        if auth.status == NodeStatus::Retired {
            return Err(VoomError::Conflict(format!(
                "remote node {} is retired",
                input.node_id
            )));
        }
        match self
            .reserve_activation_replay(&mut tx, &input, &route_key, now)
            .await?
        {
            IdempotencyOutcome::Reserved => {}
            IdempotencyOutcome::Replay(replay) => {
                return self
                    .finish_replay_in_tx(tx, input.replay_slot(), replay, |data| {
                        decode_replay(data, "remote activation")
                    })
                    .await;
            }
        }
        if self
            .node_incarnations
            .get_in_tx(&mut tx, input.incarnation_id)
            .await?
            .is_some()
        {
            return Err(VoomError::Conflict(format!(
                "node incarnation {} was already activated",
                input.incarnation_id
            )));
        }
        let activation_count = self
            .node_incarnations
            .count_started_at_or_after_in_tx(
                &mut tx,
                input.node_id,
                now - ACTIVATION_WINDOW,
                MAX_FRESH_ACTIVATIONS,
            )
            .await?;
        if activation_count >= MAX_FRESH_ACTIVATIONS {
            tracing::warn!(
                node_id = input.node_id.0,
                activation_count,
                activation_limit = MAX_FRESH_ACTIVATIONS,
                window_seconds = ACTIVATION_WINDOW.whole_seconds(),
                "remote node activation quota exceeded"
            );
            return Err(VoomError::Conflict(format!(
                "remote node {} exceeded the limit of {MAX_FRESH_ACTIVATIONS} activations per {} seconds",
                input.node_id,
                ACTIVATION_WINDOW.whole_seconds()
            )));
        }
        if let Some(current) = auth.active_incarnation_id {
            self.end_incarnation_in_tx(
                &mut tx,
                input.node_id,
                current,
                NodeIncarnationStatus::Superseded,
                NodeIncarnationEndReason::Superseded,
                now,
            )
            .await?;
        }
        let outcome = self
            .activate_manifest_in_tx(&mut tx, &input, auth.active_incarnation_id, now)
            .await?;
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

    /// Atomically end one current incarnation and retire its worker set.
    ///
    /// # Errors
    /// Returns configuration, authentication, conflict, or persistence errors.
    pub async fn remote_deactivate(
        &self,
        input: RemoteDeactivateInput,
    ) -> Result<RemoteDeactivateOutcome, VoomError> {
        let status = deactivation_status(input.reason)?;
        let now = self.clock().now();
        let route_key = super::route_node_deactivate(input.node_id);
        let mut tx = begin_immediate_tx(&self.pool).await?;
        self.verify_remote_node_token_in_tx(&mut tx, input.node_id, &input.token)
            .await?;
        match self
            .reserve_deactivation_replay(&mut tx, &input, &route_key, now)
            .await?
        {
            IdempotencyOutcome::Reserved => {}
            IdempotencyOutcome::Replay(replay) => {
                return self
                    .finish_replay_in_tx(tx, input.replay_slot(), replay, |data| {
                        decode_replay(data, "remote deactivation")
                    })
                    .await;
            }
        }
        self.nodes
            .require_active_incarnation_in_tx(&mut tx, input.node_id, input.incarnation_id)
            .await?;
        let retired_worker_ids = self
            .end_incarnation_in_tx(
                &mut tx,
                input.node_id,
                input.incarnation_id,
                status,
                input.reason,
                now,
            )
            .await?;
        let node = self
            .nodes
            .clear_incarnation_in_tx(&mut tx, input.node_id, input.incarnation_id)
            .await?;
        let outcome = RemoteDeactivateOutcome {
            node_id: input.node_id,
            node_epoch: node.epoch,
            incarnation_id: input.incarnation_id,
            status,
            reason: input.reason,
            retired_worker_ids,
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

    /// List one logical node's process incarnations newest first.
    ///
    /// # Errors
    /// Propagates repository and persisted-data validation errors.
    pub async fn list_node_incarnations(
        &self,
        node_id: voom_core::NodeId,
        limit: u32,
    ) -> Result<Vec<NodeIncarnation>, VoomError> {
        self.node_incarnations.list_for_node(node_id, limit).await
    }

    /// Prune eligible terminal activation history for one logical node.
    ///
    /// The effective cutoff never enters the active admission window. Referenced workers,
    /// their owning incarnations, completed replay rows, and append-only events are retained.
    ///
    /// # Errors
    ///
    /// Propagates transaction, repository, timestamp, and persisted-data validation errors.
    pub async fn prune_node_activation_history(
        &self,
        node_id: voom_core::NodeId,
        cutoff: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let quota_floor = self.clock().now() - ACTIVATION_WINDOW;
        let effective_cutoff = cutoff.min(quota_floor);
        let candidates = self
            .node_incarnations
            .terminal_before_in_tx(&mut tx, node_id, effective_cutoff)
            .await?;
        for candidate in candidates {
            self.workers
                .prune_retired_for_incarnation_in_tx(&mut tx, candidate.id)
                .await?;
            self.node_incarnations
                .delete_terminal_if_empty_in_tx(&mut tx, node_id, candidate.id, effective_cutoff)
                .await?;
        }
        commit_tx(tx).await
    }

    async fn activate_manifest_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteActivateInput,
        previous: Option<voom_core::NodeIncarnationId>,
        now: OffsetDateTime,
    ) -> Result<RemoteActivateOutcome, VoomError> {
        self.node_incarnations
            .insert_in_tx(
                tx,
                NewNodeIncarnation {
                    id: input.incarnation_id,
                    node_id: input.node_id,
                    started_at: now,
                },
            )
            .await?;
        let node = self
            .nodes
            .activate_incarnation_in_tx(tx, input.node_id, previous, input.incarnation_id, now)
            .await?;
        let mut workers = Vec::with_capacity(input.workers.len());
        for declaration in &input.workers {
            workers.push(
                self.register_declared_worker_in_tx(tx, input, declaration, now)
                    .await?,
            );
        }
        append_event(
            &self.events,
            tx,
            SubjectType::Node,
            Some(node.id.0),
            now,
            Event::NodeIncarnationActivated(NodeIncarnationActivatedPayload {
                node_id: node.id,
                incarnation_id: input.incarnation_id,
                node_epoch: node.epoch,
                worker_ids: workers.iter().map(|worker| worker.worker_id).collect(),
            }),
        )
        .await?;
        Ok(RemoteActivateOutcome {
            node_id: node.id,
            node_epoch: node.epoch,
            incarnation_id: input.incarnation_id,
            heartbeat_ttl_seconds: node.heartbeat_ttl_seconds,
            workers,
        })
    }

    async fn register_declared_worker_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteActivateInput,
        declaration: &RemoteWorkerDeclaration,
        now: OffsetDateTime,
    ) -> Result<ActivatedWorker, VoomError> {
        let worker = self
            .workers
            .register_in_tx(
                tx,
                NewWorker {
                    name: format!(
                        "node-{}-{}-{}",
                        input.node_id, input.incarnation_id, declaration.logical_name
                    ),
                    kind: WorkerKind::Remote,
                    registered_at: now,
                    node_id: Some(input.node_id),
                },
            )
            .await?;
        let worker = self
            .workers
            .bind_incarnation_in_tx(tx, worker.id, input.node_id, input.incarnation_id)
            .await?;
        self.append_worker_registration_events(tx, &worker, now)
            .await?;
        for operation in declaration.operations.iter().copied() {
            self.record_declared_capability_in_tx(tx, worker.id, declaration, operation, now)
                .await?;
        }
        self.record_declared_grant_in_tx(tx, worker.id, declaration, now)
            .await?;
        Ok(ActivatedWorker {
            logical_name: declaration.logical_name.clone(),
            worker_id: worker.id,
            worker_epoch: worker.epoch,
        })
    }

    async fn append_worker_registration_events(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        worker: &voom_store::repo::execution::workers::Worker,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        append_event(
            &self.events,
            tx,
            SubjectType::Worker,
            Some(worker.id.0),
            now,
            Event::WorkerRegistered(WorkerRegisteredPayload {
                worker_id: worker.id,
                name: worker.name.clone(),
                kind: worker.kind,
            }),
        )
        .await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Worker,
            Some(worker.id.0),
            now,
            Event::WorkerLinkedToNode(WorkerLinkedToNodePayload {
                worker_id: worker.id,
                node_id: worker.node_id.ok_or_else(|| {
                    VoomError::Internal(format!("declared worker {} has no node", worker.id))
                })?,
            }),
        )
        .await
    }

    async fn record_declared_capability_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        worker_id: WorkerId,
        declaration: &RemoteWorkerDeclaration,
        operation: voom_core::OperationKind,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let capability = self
            .workers
            .record_capability_in_tx(
                tx,
                NewCapability {
                    worker_id,
                    operation: operation.into(),
                    codecs: Vec::new(),
                    hardware: Vec::new(),
                    artifact_access: declaration
                        .artifact_access
                        .iter()
                        .map(|mode| mode.as_str().to_owned())
                        .collect(),
                    extra: json!({}),
                },
            )
            .await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Worker,
            Some(worker_id.0),
            now,
            Event::WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload {
                worker_id,
                capability_id: capability.id,
                operation: capability.operation,
            }),
        )
        .await
    }

    async fn record_declared_grant_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        worker_id: WorkerId,
        declaration: &RemoteWorkerDeclaration,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let grant = self
            .workers
            .record_grant_in_tx(
                tx,
                NewGrant {
                    worker_id,
                    can_execute: declaration
                        .operations
                        .iter()
                        .copied()
                        .map(TicketOperation::from)
                        .collect(),
                    can_access_read: Vec::new(),
                    can_access_write: Vec::new(),
                    denies: Vec::new(),
                    max_parallel: json!({"*": declaration.max_parallel}),
                },
            )
            .await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Worker,
            Some(worker_id.0),
            now,
            Event::WorkerGrantRecorded(WorkerGrantRecordedPayload {
                worker_id,
                grant_id: grant.id,
            }),
        )
        .await
    }

    pub(crate) async fn end_incarnation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: voom_core::NodeId,
        incarnation_id: voom_core::NodeIncarnationId,
        status: NodeIncarnationStatus,
        reason: NodeIncarnationEndReason,
        now: OffsetDateTime,
    ) -> Result<Vec<WorkerId>, VoomError> {
        let retired = self
            .workers
            .retire_live_for_incarnation_in_tx(tx, incarnation_id, now)
            .await?;
        for worker in &retired {
            append_event(
                &self.events,
                tx,
                SubjectType::Worker,
                Some(worker.id.0),
                now,
                Event::WorkerRetired(WorkerRetiredPayload {
                    worker_id: worker.id,
                }),
            )
            .await?;
        }
        let ended = self
            .node_incarnations
            .end_in_tx(tx, incarnation_id, status, reason, now)
            .await?;
        let retired_worker_ids: Vec<WorkerId> = retired.iter().map(|worker| worker.id).collect();
        append_event(
            &self.events,
            tx,
            SubjectType::Node,
            Some(node_id.0),
            now,
            Event::NodeIncarnationEnded(NodeIncarnationEndedPayload {
                node_id,
                incarnation_id,
                status: ended.status,
                reason,
                retired_worker_ids: retired_worker_ids.clone(),
            }),
        )
        .await?;
        Ok(retired_worker_ids)
    }

    async fn reserve_activation_replay(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteActivateInput,
        route_key: &str,
        now: OffsetDateTime,
    ) -> Result<IdempotencyOutcome, VoomError> {
        self.remote_idempotency
            .reserve_or_replay_in_tx(
                tx,
                RemoteIdempotencyInput {
                    node_id: input.node_id,
                    route_key: route_key.to_owned(),
                    worker_id: None,
                    idempotency_key: input.idempotency_key.clone(),
                    request_hash: input.request_hash.clone(),
                    created_at: now,
                },
            )
            .await
    }

    async fn reserve_deactivation_replay(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteDeactivateInput,
        route_key: &str,
        now: OffsetDateTime,
    ) -> Result<IdempotencyOutcome, VoomError> {
        self.remote_idempotency
            .reserve_or_replay_in_tx(
                tx,
                RemoteIdempotencyInput {
                    node_id: input.node_id,
                    route_key: route_key.to_owned(),
                    worker_id: None,
                    idempotency_key: input.idempotency_key.clone(),
                    request_hash: input.request_hash.clone(),
                    created_at: now,
                },
            )
            .await
    }
}

fn validate_manifest(workers: &[RemoteWorkerDeclaration]) -> Result<(), VoomError> {
    if workers.is_empty() || workers.len() > MAX_WORKERS {
        return Err(VoomError::Config(format!(
            "activation workers must contain 1..={MAX_WORKERS} declarations"
        )));
    }
    let mut names = BTreeSet::new();
    for worker in workers {
        validate_declaration(worker)?;
        if !names.insert(worker.logical_name.as_str()) {
            return Err(VoomError::Config(format!(
                "activation worker logical name {:?} is duplicated",
                worker.logical_name
            )));
        }
    }
    Ok(())
}

fn validate_declaration(worker: &RemoteWorkerDeclaration) -> Result<(), VoomError> {
    let valid_name = !worker.logical_name.is_empty()
        && worker.logical_name.len() <= MAX_LOGICAL_NAME_BYTES
        && worker.logical_name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    if !valid_name {
        return Err(VoomError::Config(format!(
            "invalid activation worker logical name {:?}",
            worker.logical_name
        )));
    }
    if worker.operations.is_empty()
        || worker.operations.iter().collect::<BTreeSet<_>>().len() != worker.operations.len()
    {
        return Err(VoomError::Config(format!(
            "activation worker {:?} operations must be non-empty and unique",
            worker.logical_name
        )));
    }
    if worker.artifact_access.is_empty()
        || worker
            .artifact_access
            .iter()
            .map(|mode| mode.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            != worker.artifact_access.len()
    {
        return Err(VoomError::Config(format!(
            "activation worker {:?} artifact access modes must be non-empty and unique",
            worker.logical_name
        )));
    }
    if !(1..=MAX_PARALLEL).contains(&worker.max_parallel) {
        return Err(VoomError::Config(format!(
            "activation worker {:?} max_parallel must be 1..={MAX_PARALLEL}",
            worker.logical_name
        )));
    }
    Ok(())
}

fn deactivation_status(
    reason: NodeIncarnationEndReason,
) -> Result<NodeIncarnationStatus, VoomError> {
    match reason {
        NodeIncarnationEndReason::GracefulShutdown => Ok(NodeIncarnationStatus::Retired),
        NodeIncarnationEndReason::ChildStartupFailed
        | NodeIncarnationEndReason::ChildRestartExhausted => Ok(NodeIncarnationStatus::Failed),
        other => Err(VoomError::Config(format!(
            "remote deactivation reason {} is not allowed",
            other.as_str()
        ))),
    }
}

#[cfg(test)]
#[path = "activation_test.rs"]
mod tests;
